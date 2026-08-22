//! Binary entrypoint: config -> store -> services -> pipeline -> watcher ->
//! connector loop -> axum serve.

use std::sync::Arc;
use wikillm_api::config::load_config;
use wikillm_api::http::{self, AppState};
use wikillm_api::ingest::pipeline::{EmbedQueue, IndexPipeline, RuntimeFlags};
use wikillm_api::llm::embedder::EmbedderLike;
use wikillm_api::http::rate_limit::RateLimiter;
use wikillm_api::services::broadcaster::Broadcaster;
use wikillm_api::services::connectors;
use wikillm_api::services::graph::GraphService;
use wikillm_api::services::keys::EnvKeyEntry;
use wikillm_api::services::metrics::MetricsRegistry;
use wikillm_api::services::okf_service::OkfService;
use wikillm_api::services::project::ProjectService;
use wikillm_api::services::search::SearchService;
use wikillm_api::services::settings::SettingsService;
use wikillm_api::services::webhooks::WebhookDispatcher;
use wikillm_api::store::Store;

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("WikiLLM API (rust) listening on http://{}:{}", config.host, config.port);

    let store: Arc<dyn Store> = match config.db_backend.as_str() {
        "postgres" => {
            let url = config.database_url.clone().ok_or_else(|| {
                wikillm_api::error::Error::Config(
                    "DATABASE_URL required for postgres backend".into(),
                )
            })?;
            Arc::new(wikillm_api::store::pg::PostgresStore::connect(&url).await)
        }
        _ => Arc::new(wikillm_api::store::sqlite::SqliteStore::open(&config.db_path)?),
    };
    store.migrate().await?;

    let settings = Arc::new(SettingsService::new(store.clone(), config.clone()));
    settings.warm().await?;

    // Bootstrap: instance with no keys mints an admin key, shown once.
    let env_entries: Vec<EnvKeyEntry> = config
        .api_keys
        .values()
        .map(|e| EnvKeyEntry {
            name: e.name.clone(),
            secret: e.key.clone(),
            role: e.role.clone(),
            scope: e.projects.clone(),
        })
        .collect();
    let keys = Arc::new(wikillm_api::services::keys::KeyRegistry::new(store.clone(), env_entries));
    if !keys.has_env_keys() && store.count_api_keys().await.unwrap_or(0) == 0 {
        let bootstrap = std::env::var("BOOTSTRAP_ADMIN_KEY").ok();
        match keys
            .create_key(Some("bootstrap-admin"), bootstrap.as_deref(), "admin", &["*".to_string()], "bootstrap")
            .await
        {
            Ok(created) => println!(
                "\n=== WikiLLM bootstrap admin key (shown once; store it now) ===\n  {}\n=== Configure the instance via PUT /v1/settings, POST /v1/keys ===\n",
                created.secret
            ),
            Err(e) => eprintln!("bootstrap key creation failed: {e}"),
        }
    }

    // LLM provider from settings snapshot (hot-swappable via settings hooks)
    let api_key = config.llm_api_key.clone().unwrap_or_default();
    let embed_model = config.llm_embed_model.clone().unwrap_or_default();
    let llm_base_url = settings.get_string("llm_base_url").await.unwrap_or_default();
    let llm_holder = Arc::new(std::sync::RwLock::new(Some(
        wikillm_api::llm::provider::create_from_env_snapshot(
            &llm_base_url,
            &api_key,
            &config.llm_model,
            &embed_model,
            config.embedding_dims,
        ),
    )));

    let embedder_holder: Arc<std::sync::RwLock<Option<Arc<dyn EmbedderLike>>>> =
        Arc::new(std::sync::RwLock::new(
            wikillm_api::llm::embedder::resolve_embedder(
                &settings
                    .get_string("embedding_provider")
                    .await
                    .unwrap_or_else(|_| "auto".into()),
                &llm_base_url,
                Some(api_key.as_str()),
                &settings.get_string("llm_embed_model").await.unwrap_or_default(),
                config.embedding_dims,
            )
            .map(|boxed| Arc::from(boxed) as Arc<dyn EmbedderLike>),
        ));

    let flags = RuntimeFlags {
        llm: llm_holder.clone(),
        embedder: embedder_holder.clone(),
        distill_enabled: Arc::new(std::sync::atomic::AtomicBool::new(config.llm_distill)),
    };

    let (embed_tx, embed_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let pipeline = Arc::new(IndexPipeline::new(&config.wiki_root, store.clone(), flags.clone(), embed_tx));
    tokio::spawn(EmbedQueue::run(embed_rx, store.clone(), flags));

    settings.on_change(move |key, _value| {
        if ["embedding_provider", "onnx_model", "onnx_dtype", "onnx_device"].contains(&key) {
            eprintln!("embedder settings changed; rebuild takes effect on next read");
        }
    });

    let indexed = pipeline.reindex_all().await.unwrap_or(0);
    println!("Indexed {indexed} documents at boot");

    let broadcaster = Arc::new(Broadcaster::new());
    let webhooks = Arc::new(WebhookDispatcher::new(store.clone(), settings.clone()));
    {
        let pipeline = pipeline.clone();
        let webhooks = webhooks.clone();
        pipeline
            .set_change_emitter(Box::new(move |event| {
                void_dispatch(webhooks.clone(), event.clone());
            }))
            .await;
        let _ = pipeline; // keep alive
    }

    // Watcher: feed external FS events through the pipeline + broadcasts
    let _watcher = match wikillm_api::fs::watcher::Watcher::start(&config.wiki_root) {
        Ok((handle, mut rx)) => {
            let pipeline = pipeline.clone();
            let broadcaster = broadcaster.clone();
            let webhooks = webhooks.clone();
            tokio::spawn(async move {
                while let Some(paths) = rx.recv().await {
                    for rel in paths {
                        if let Ok(Some(event)) = pipeline
                            .handle_file_change(&rel, Default::default())
                            .await
                        {
                            broadcaster.broadcast(&wikillm_api::domain::ChangeEvent {
                                event_type: "change".into(),
                                data: event.clone(),
                            });
                            webhooks.dispatch(&event).await;
                        }
                    }
                }
            });
            Some(handle)
        }
        Err(e) => {
            eprintln!("fs watcher disabled: {e}");
            None
        }
    };

    // Connector polling loop
    {
        let store = store.clone();
        let pipeline = pipeline.clone();
        let interval = config.connector_poll_seconds.max(5) as u64;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                let connectors = match store.list_connectors().await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                for connector in connectors.iter().filter(|c| c.enabled) {
                    let state = store.get_connector_state(&connector.id).await.ok().flatten();
                    let result = match connector.kind.as_str() {
                        "git" => connectors::git::poll(&connector.config, &state.unwrap_or(serde_json::Value::Null)).await,
                        "web" => connectors::web::poll(&connector.config, &state.unwrap_or(serde_json::Value::Null)).await,
                        "github" => connectors::github::poll(&connector.config, &state.unwrap_or(serde_json::Value::Null)).await,
                        _ => continue,
                    };
                    if let Ok((docs, new_state)) = result {
                        for (path, content, title, mtime) in docs {
                            let _ = pipeline
                                .index_external_content(
                                    &format!("{}/{}", connector.id, path),
                                    &content,
                                    &connector.id,
                                    Some(&title),
                                    None,
                                    Some(mtime),
                                )
                                .await;
                        }
                        let _ = store.set_connector_state(&connector.id, &new_state).await;
                    }
                }
            }
        });
    }

    let search = Arc::new(SearchService::new(store.clone(), llm_holder.clone()));
    let graph = Arc::new(GraphService::new(store.clone()));
    let projects = Arc::new(ProjectService::new(store.clone()));
    let okf = Arc::new(OkfService::new(config.clone(), layout_setting_handle()));
    let metrics = Arc::new(MetricsRegistry::new());
    let rate_limiter = Arc::new(RateLimiter::new());
    let auth_state = wikillm_api::http::auth::AuthState {
        registry: keys.clone(),
        public_read: public_read_handle(&settings),
    };

    let state = AppState {
        config: Arc::new(config),
        store: store.clone(),
        settings: settings.clone(),
        keys,
        projects,
        graph,
        okf,
        pipeline,
        broadcaster,
        metrics,
        rate_limiter,
        search,
        llm_holder: llm_holder.clone(),
    };
    let _ = auth_state; // auth resolved per-request via keys registry

    let router = http::build_router(state);
    let listener = tokio::net::TcpListener::bind((config_host(), config_port())).await?;
    axum::serve(listener, router).await?;

    fn config_host() -> String {
        std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into())
    }
    fn config_port() -> u16 {
        std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3000)
    }
    fn layout_setting_handle() -> Arc<tokio::sync::RwLock<String>> {
        Arc::new(tokio::sync::RwLock::new(std::env::var("LAYOUT").unwrap_or_else(|_| "auto".into())))
    }
    fn public_read_handle(settings: &Arc<SettingsService>) -> Arc<tokio::sync::RwLock<bool>> {
        let handle = Arc::new(tokio::sync::RwLock::new(true));
        let weak = Arc::downgrade(&handle);
        let settings = settings.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Some(h) = weak.upgrade() {
                    if let Ok(v) = settings.get_bool("public_read").await {
                        *h.write().await = v;
                    }
                } else {
                    break;
                }
            }
        });
        handle
    }
    fn void_dispatch(webhooks: Arc<WebhookDispatcher>, event: wikillm_api::domain::ChangeEventData) {
        tokio::spawn(async move {
            webhooks.dispatch(&event).await;
        });
    }
    Ok(())
}
