//! Axum application assembly: every `/v1` route plus health/metrics, auth,
//! SSE/WebSocket change feeds.

pub mod auth;
pub mod rate_limit;
pub mod routes;

use std::future::Future;

use crate::config::Config;
use crate::domain::*;
use crate::error::{Error, Result};
use crate::http::auth::AuthInfo;
use crate::http::rate_limit::RateLimiter;
use crate::ingest::pipeline::IndexPipeline;
use crate::services::broadcaster::Broadcaster;
use crate::services::graph::GraphService;
use crate::services::keys::KeyRegistry;
use crate::services::metrics::MetricsRegistry;
use crate::services::okf_service::OkfService;
use crate::services::project::ProjectService;
use crate::services::search::SearchService;
use crate::services::settings::SettingsService;
use crate::store::Store;
use axum::{
    extract::{Path as AxumPath, Query, State},
    response::Response,
    http::{HeaderMap, StatusCode},
    response::{sse::Event, IntoResponse, Sse},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<dyn Store>,
    pub settings: Arc<SettingsService>,
    pub keys: Arc<KeyRegistry>,
    pub projects: Arc<ProjectService>,
    pub graph: Arc<GraphService>,
    pub okf: Arc<OkfService>,
    pub pipeline: Arc<IndexPipeline>,
    pub broadcaster: Arc<Broadcaster>,
    pub metrics: Arc<MetricsRegistry>,
    pub rate_limiter: Arc<RateLimiter>,
    pub search: Arc<SearchService>,
    pub llm_holder: Arc<std::sync::RwLock<Option<crate::llm::provider::DynLlmProvider>>>,
}

pub type HttpResult<T> = Result<T, (StatusCode, Json<Value>)>;

fn err_status(status: StatusCode, code: &str, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": code, "message": message })))
}

/// Map a service error to the HTTP contract used by the TS service.
fn map_err(e: &Error) -> (StatusCode, Json<Value>) {
    let message = e.to_string();
    if message.starts_with("not found:") || message.contains("Unknown setting") {
        return err_status(StatusCode::NOT_FOUND, "not_found", &message);
    }
    if message.starts_with("forbidden:") || message.contains("Project not allowed") {
        return err_status(StatusCode::FORBIDDEN, "forbidden", &message);
    }
    if message.starts_with("conflict:") || message.contains("already exists") {
        return err_status(StatusCode::CONFLICT, "conflict", &message);
    }
    if message.starts_with("validation:")
        || message.starts_with("Invalid value")
        || message.starts_with("setting is immutable")
        || message.starts_with("path:")
    {
        return err_status(StatusCode::BAD_REQUEST, "validation", &message);
    }
    if message.contains("No LLM provider configured") {
        return err_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "llm_not_configured",
            &message,
        );
    }
    err_status(StatusCode::INTERNAL_SERVER_ERROR, "internal", &message)
}

pub struct Ctx {
    pub state: AppState,
    pub auth: AuthInfo,
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    is_get: bool,
) -> Result<AuthInfo, (StatusCode, Json<Value>)> {
    let header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Some(secret) = header.strip_prefix("Bearer ") {
        if let Some(auth) = state.keys.verify(secret).await.map_err(|e| map_err(&e))? {
            return Ok(auth);
        }
        if !(state.settings_public_read().await) {
            return Err(err_status(StatusCode::UNAUTHORIZED, "unauthorized", "Invalid API key"));
        }
    }
    if is_get && state.settings_public_read().await {
        return Ok(AuthInfo { name: "anonymous".into(), role: "read".into(), projects: vec!["*".into()] });
    }
    Err(err_status(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Missing or invalid Authorization header",
    ))
}

impl AppState {
    pub async fn public_read(&self) -> bool {
        self.settings.get_bool("public_read").await.unwrap_or(true)
    }
    pub fn settings_public_read(&self) -> impl Future<Output = bool> + '_ {
        self.public_read()
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1", get(self_description))
        .route(
            "/v1/pages",
            get(pages_list).post(batch_write),
        )
        .route(
            "/v1/pages/{*rel_path}",
            get(page_get).put(page_put).delete(page_delete),
        )
        .route(
            "/v1/sources",
            get(sources_list).post(source_upload),
        )
        .route(
            "/v1/sources/{*rel_path}",
            get(source_get).delete(source_delete),
        )
        .route("/v1/index", get(index_get))
        .route("/v1/index/refresh", post(index_refresh))
        .route("/v1/log", get(log_get))
        .route("/v1/log/append", post(log_append))
        .route("/v1/search", get(search_handler))
        .route("/v1/query", post(query_handler))
        .route("/v1/changes", get(changes_list))
        .route("/v1/events", get(events_sse))
        .route("/v1/ws", get(ws_handler))
        .route("/v1/ingest", post(ingest_handler))
        .route("/v1/graph/{*rel_path}", get(graph_handler))
        .route("/v1/graph", get(graph_by_query))
        .route("/v1/okf/validate", post(okf_validate))
        .route("/v1/okf/layout", get(okf_layout))
        .route("/v1/bundle/export", get(bundle_export))
        .route("/v1/bundle/import", post(bundle_import))
        .route("/v1/connectors", get(connectors_list).post(connectors_create))
        .route(
            "/v1/connectors/{id}",
            delete(connectors_delete),
        )
        .route("/v1/connectors/{id}/run", post(connectors_run))
        .route(
            "/v1/projects/{name}",
            get(project_get).put(project_put).delete(project_delete),
        )
        .route("/v1/projects", get(projects_list))
        .route(
            "/v1/settings",
            get(settings_list),
        )
        .route(
            "/v1/settings/{key}",
            get(setting_get).put(setting_put).delete(setting_reset),
        )
        .route("/v1/keys", get(keys_list).post(keys_create))
        .route("/v1/keys/{name}", delete(keys_delete))
        .route("/v1/admin/reindex", post(admin_reindex))
        .route("/v1/admin/stats", get(admin_stats))
        .route("/v1/feedback", post(feedback_handler))
        .route("/v1/webhooks", get(webhooks_list).post(webhook_create))
        .route("/v1/webhooks/{id}", delete(webhook_delete))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(state): State<AppState>) -> Json<Value> {
    let (sse, ws) = state.broadcaster.counts().await;
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "wiki_root": state.config.wiki_root,
        "public_read": state.public_read().await,
        "feeds": { "sse": sse, "ws": ws },
    }))
}

async fn metrics(State(state): State<AppState>) -> String {
    state.metrics.render()
}

async fn self_description(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({
        "service": "wikillm-api",
        "version": env!("CARGO_PKG_VERSION"),
        "runtime": "rust",
        "endpoints": [
            "/health", "/metrics", "/v1/pages", "/v1/sources", "/v1/index",
            "/v1/log", "/v1/search", "/v1/query", "/v1/changes", "/v1/events",
            "/v1/ws", "/v1/ingest", "/v1/graph", "/v1/okf", "/v1/bundle",
            "/v1/connectors", "/v1/projects", "/v1/settings", "/v1/keys",
            "/v1/admin", "/v1/feedback", "/v1/webhooks"
        ],
    }))
}

macro_rules! require_auth {
    ($state:expr, $headers:expr, $is_get:expr) => {
        match authenticate(&$state, &$headers, $is_get).await {
            Ok(a) => a,
            Err(e) => return Err(e),
        }
    };
}
#[allow(unused_imports)]
pub(crate) use require_auth;

// -- pages ------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct PagesQuery {
    folder: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

async fn pages_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PagesQuery>,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    let page = state
        .store
        .list_documents(
            &ListOptions {
                folder: q.folder.clone(),
                kind: Some(DocKind::Page),
                ..Default::default()
            },
            q.limit.unwrap_or(50),
            q.cursor.as_deref(),
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({
        "items": page.items.iter().map(document_summary).collect::<Vec<_>>(),
        "nextCursor": page.next_cursor,
    })))
}

fn document_summary(d: &DocumentRecord) -> Value {
    json!({
        "rel_path": d.rel_path,
        "title": d.title,
        "kind": d.kind.as_str(),
        "origin": d.origin,
        "okf_type": d.okf_type,
        "tags": d.tags,
        "status": d.status,
        "hash": d.hash,
        "mtime": d.mtime,
    })
}

#[derive(serde::Deserialize)]
struct PageWriteBody {
    content: String,
    #[serde(default)]
    frontmatter: Option<Value>,
    #[serde(default)]
    if_match: Option<String>,
}

async fn page_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
) -> HttpResult<Response> {
    require_auth!(state, headers, true);
    let raw_mode = rel_path.ends_with("/raw");
    let rel_path = if raw_mode {
        rel_path.trim_end_matches("/raw").to_string()
    } else {
        rel_path
    };
    match read_page_record(&state, &rel_path).await? {
        Some(page) if raw_mode => {
            let mut response = (StatusCode::OK, page.body).into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".parse().unwrap(),
            );
            Ok(response)
        }
        Some(page) => Ok(Json(serde_json::to_value(page).unwrap_or(Value::Null)).into_response()),
        None => Err(err_status(StatusCode::NOT_FOUND, "not_found", &format!("Page not found: {rel_path}"))),
    }
}

async fn read_page_record(
    state: &AppState,
    rel_path: &str,
) -> Result<Option<DocumentRecord>, (StatusCode, Json<Value>)> {
    crate::fs::paths::resolve_wiki_path(&state.config.wiki_root, rel_path)
        .map_err(|e| map_err(&e))?;
    state.store.get_document(rel_path).await.map_err(|e| map_err(&e))
}

async fn page_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
    Json(body): Json<PageWriteBody>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    write_page_via_pipeline(&state, &auth.name, &rel_path, body.content, body.frontmatter, body.if_match)
        .await?;
    let page = state.store.get_document(&rel_path).await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "success": true, "page": page })))
}

async fn page_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    let doc = state.store.get_document(&rel_path).await.map_err(|e| map_err(&e))?;
    if doc.is_none() {
        return Err(err_status(StatusCode::NOT_FOUND, "not_found", "Page not found"));
    }
    let abs = std::path::Path::new(&state.config.wiki_root).join(&rel_path);
    std::fs::remove_file(abs).map_err(|e| map_err(&Error::Io(e)))?;
    state
        .pipeline
        .handle_file_change(
            &rel_path,
            crate::ingest::pipeline::FileAttribution {
                source: Some("api".into()),
                operation_id: None,
            },
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "success": true })))
}

/// Shared OCC write used by PUT pages and batch operations.
pub(crate) async fn write_page_via_pipeline(
    state: &AppState,
    source: &str,
    rel_path: &str,
    content: String,
    frontmatter: Option<Value>,
    if_match: Option<String>,
) -> Result<(), (StatusCode, Json<Value>)> {
    crate::fs::paths::resolve_wiki_path(&state.config.wiki_root, rel_path)
        .map_err(|e| map_err(&e))?;
    let abs = std::path::Path::new(&state.config.wiki_root).join(rel_path);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| map_err(&Error::Io(e)))?;
    }
    let existing_hash = if abs.exists() {
        Some(crate::fs::atomic::hash_content(
            &std::fs::read_to_string(&abs).unwrap_or_default(),
        ))
    } else {
        None
    };
    if let Some(expected) = &if_match {
        match &existing_hash {
            Some(h) if h != expected => {
                return Err(err_status(
                    StatusCode::CONFLICT,
                    "conflict",
                    "content changed since read",
                ));
            }
            None => {
                return Err(err_status(StatusCode::CONFLICT, "conflict", "target does not exist"));
            }
            _ => {}
        }
    }

    // strict mode: bundle declaring okf_version requires a type
    if state.settings.get_bool("okf_strict").await.unwrap_or(false) {
        let root_index = std::path::Path::new(&state.config.wiki_root).join("index.md");
        if root_index.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&root_index) {
                if raw.contains("okf_version") {
                    let has_type = frontmatter
                        .as_ref()
                        .and_then(|f| f.get("type"))
                        .and_then(|t| t.as_str())
                        .map(|t| !t.trim().is_empty())
                        .unwrap_or(false);
                    if !has_type {
                        return Err(err_status(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "okf_strict",
                            "missing frontmatter 'type'",
                        ));
                    }
                }
            }
        }
    }

    let actor = actor_for(state, source).await;
    let mut fm = frontmatter.unwrap_or_else(|| json!({}));
    if let Some(obj) = fm.as_object_mut() {
        let now = chrono::Utc::now().to_rfc3339();
        obj.entry("updated_at").or_insert(json!(now));
        obj.entry("updated_by").or_insert(json!(source));
        obj.entry("generated").or_insert(json!({ "by": actor, "at": now }));
    }

    let fm_yaml = if fm.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        String::new()
    } else {
        serde_yaml::to_string(&fm).unwrap_or_default()
    };
    let file_body = if fm_yaml.is_empty() {
        content.clone()
    } else {
        format!("---\n{fm_yaml}---\n\n{content}")
    };
    crate::fs::atomic::atomic_write(&abs, file_body.as_bytes()).map_err(|e| map_err(&e))?;

    state
        .pipeline
        .handle_file_change(
            rel_path,
            crate::ingest::pipeline::FileAttribution {
                source: Some("api".into()),
                operation_id: None,
            },
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(())
}

async fn actor_for(state: &AppState, source: &str) -> String {
    let humans = state.settings.get_string("human_actors").await.unwrap_or_default();
    let human_list: Vec<&str> = humans.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if human_list.iter().any(|h| *h == source)
        || source.starts_with("user-")
        || source.starts_with("human-")
    {
        format!("human:{source}")
    } else {
        format!("{source}/wikillm-api")
    }
}

// -- batch ------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct BatchOperation {
    rel_path: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    frontmatter: Option<Value>,
    #[serde(default)]
    if_match: Option<String>,
    #[serde(default)]
    delete: bool,
}

#[derive(serde::Deserialize)]
struct BatchBody {
    operations: Vec<BatchOperation>,
}

async fn batch_write(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchBody>,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    let mut results = Vec::new();
    for op in &body.operations {
        if op.delete {
            let abs = std::path::Path::new(&state.config.wiki_root).join(&op.rel_path);
            let success = abs.exists();
            if success {
                std::fs::remove_file(&abs).map_err(|e| map_err(&Error::Io(e)))?;
                state
                    .pipeline
                    .handle_file_change(
                        &op.rel_path,
                        crate::ingest::pipeline::FileAttribution {
                            source: Some("api".into()),
                            operation_id: None,
                        },
                    )
                    .await
                    .map_err(|e| map_err(&e))?;
            }
            results.push(json!({ "rel_path": op.rel_path, "success": success }));
        } else if let Some(content) = &op.content {
            write_page_via_pipeline(&state, "batch-agent", &op.rel_path, content.clone(), op.frontmatter.clone(), op.if_match.clone())
                .await?;
            results.push(json!({ "rel_path": op.rel_path, "success": true }));
        } else {
            results.push(json!({ "rel_path": op.rel_path, "success": false }));
        }
    }
    Ok(Json(json!({ "success": true, "results": results })))
}

// -- sources ----------------------------------------------------------------

async fn sources_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    let docs = state
        .store
        .list_documents(
            &ListOptions { kind: Some(DocKind::Source), ..Default::default() },
            500,
            None,
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({
        "items": docs.items.iter().map(document_summary).collect::<Vec<_>>(),
    })))
}

async fn source_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
    body: axum::body::Bytes,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    let max_bytes = state.settings.get_i64("max_upload_mb").await.unwrap_or(100) * 1024 * 1024;
    if body.len() as i64 > max_bytes {
        return Err(err_status(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "Upload exceeds cap"));
    }
    if !rel_path.starts_with("raw/") && !rel_path.starts_with("raw") {
        return Err(err_status(StatusCode::BAD_REQUEST, "validation", "Sources must be inside raw/"));
    }
    let abs = std::path::Path::new(&state.config.wiki_root).join(&rel_path);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| map_err(&Error::Io(e)))?;
    }
    std::fs::write(&abs, &body).map_err(|e| map_err(&Error::Io(e)))?;
    state
        .pipeline
        .handle_file_change(
            &rel_path,
            crate::ingest::pipeline::FileAttribution {
                source: Some("api".into()),
                operation_id: None,
            },
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "success": true })))
}

async fn source_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    match state.store.get_document(&rel_path).await.map_err(|e| map_err(&e))? {
        Some(doc) => Ok(Json(serde_json::to_value(doc).unwrap_or(Value::Null))),
        None => Err(err_status(StatusCode::NOT_FOUND, "not_found", "Source not found")),
    }
}

async fn source_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    let abs = std::path::Path::new(&state.config.wiki_root).join(&rel_path);
    if !abs.is_file() {
        return Err(err_status(StatusCode::NOT_FOUND, "not_found", "Source not found"));
    }
    std::fs::remove_file(&abs).map_err(|e| map_err(&Error::Io(e)))?;
    state
        .pipeline
        .handle_file_change(
            &rel_path,
            crate::ingest::pipeline::FileAttribution {
                source: Some("api".into()),
                operation_id: None,
            },
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "success": true })))
}

// -- documents --------------------------------------------------------------

#[derive(serde::Deserialize)]
struct DocumentsQuery {
    kind: Option<String>,
    origin: Option<String>,
    folder: Option<String>,
    #[serde(rename = "type")]
    okf_type: Option<String>,
    tags: Option<String>,
    status: Option<String>,
    trust: Option<String>,
    fresh: Option<bool>,
    project: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

async fn documents_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DocumentsQuery>,
) -> HttpResult<Response> {
    let auth = require_auth!(state, headers, true);
    let prefixes = state
        .projects
        .resolve_scope_prefixes(&auth.projects, q.project.as_deref())
        .await
        .map_err(|e| map_err(&e))?;
    if prefixes.first().map(String::as_str) == Some("__none__") {
        return Ok((StatusCode::OK, Json(json!({"items": []}))).into_response());
    }
    let filters = SearchFilters {
        okf_types: q.okf_type.map(|t| t.split(',').map(String::from).collect()),
        tags: q.tags.map(|t| t.split(',').map(String::from).collect()),
        statuses: q.status.map(|t| t.split(',').map(String::from).collect()),
        trust_min: q.trust.clone(),
        fresh_only: q.fresh,
        path_prefixes: if prefixes.first().map(String::as_str) == Some("*") { None } else { Some(prefixes) },
        kinds: q.kind.clone().map(|k| vec![k]),
        origins: q.origin.clone().map(|o| vec![o]),
        ..Default::default()
    };
    let result = state
        .store
        .list_documents(
            &ListOptions { folder: q.folder.clone(), filters: Some(filters), ..Default::default() },
            q.limit.unwrap_or(50),
            q.cursor.as_deref(),
        )
        .await
        .map_err(|e| map_err(&e))?;
    let fingerprint = state.store.collection_fingerprint(q.folder.as_deref()).await.map_err(|e| map_err(&e))?;
    let etag = format!("W/\"{}-{}\"", fingerprint.0, fingerprint.1);
    if headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == etag)
        .unwrap_or(false)
    {
        return Ok((StatusCode::NOT_MODIFIED, [("ETag", etag)]).into_response());
    }
    let items: Vec<Value> = result
        .items
        .iter()
        .map(|d| {
            json!({
                "rel_path": d.rel_path,
                "kind": d.kind.as_str(),
                "origin": d.origin,
                "title": d.title,
                "okf_type": d.okf_type,
                "tags": d.tags,
                "status": d.status,
                "stale_after": d.stale_after,
                "trust": crate::domain::trust_tier(d.verified.as_ref()),
                "hash": d.hash,
                "mtime": d.mtime,
            })
        })
        .collect();
    let mut response = Json(json!({ "items": items, "nextCursor": result.next_cursor })).into_response();
    response.headers_mut().insert(axum::http::header::ETAG, etag.parse().unwrap());
    Ok(response)
}
async fn index_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    let content = std::fs::read_to_string(std::path::Path::new(&state.config.wiki_root).join("index.md"))
        .unwrap_or_default();
    Ok(Json(json!({ "content": content })))
}

async fn index_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    let count = state.pipeline.reindex_all().await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "operationId": ulid::Ulid::new().to_string(), "pageCount": count })))
}

async fn log_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    let content = std::fs::read_to_string(std::path::Path::new(&state.config.wiki_root).join("log.md"))
        .unwrap_or_default();
    Ok(Json(json!({ "content": content })))
}

#[derive(serde::Deserialize)]
struct LogAppendBody {
    message: String,
    #[serde(default)]
    kind: Option<String>,
}

async fn log_append(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LogAppendBody>,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    let log_path = std::path::Path::new(&state.config.wiki_root).join("log.md");
    let existing = std::fs::read_to_string(&log_path).unwrap_or_default();
    let today = chrono::Utc::now().format("%Y-%m-%d");
    let entry = format!("* **{}**: {}", body.kind.as_deref().unwrap_or("Update"), body.message);
    let mut out = String::new();
    if existing.contains("## ") {
        out.push_str(&existing);
        out.push_str(&entry);
        out.push('\n');
    } else {
        out.push_str(&existing);
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("## {today}\n{entry}\n"));
    }
    crate::fs::atomic::atomic_write(&log_path, out.as_bytes())
        .map_err(|e| map_err(&Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e))))?;
    state
        .pipeline
        .handle_file_change("log.md", crate::ingest::pipeline::FileAttribution { source: Some("api".into()), operation_id: None })
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "success": true, "entry": entry })))
}

#[derive(serde::Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default)]
    limit: Option<i64>,
}

async fn search_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    let _started = std::time::Instant::now();
    let filters = SearchFilters {
        kinds: Some(vec!["page".into(), "doc".into()]),
        ..Default::default()
    };
    let result = state
        .search
        .search(crate::services::search::SearchOptions {
            q: q.q.clone(),
            limit: q.limit.unwrap_or(20) as usize,
            filters: Some(filters),
            rerank: false,
            expand_context: false,
        })
        .await
        .map_err(|e| map_err(&e))?;
    state.metrics.counter(
        "wikillm_search_total",
        &[],
        1.0,
    );
    Ok(Json(json!({
        "query": q.q,
        "mode": result.mode,
        "latency_ms": result.latency_ms,
        "results": serde_json::to_value(result.results).unwrap_or_default(),
    })))
}

#[derive(serde::Deserialize)]
struct QueryBody {
    question: String,
    #[serde(default)]
    project: Option<String>,
}

async fn query_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<QueryBody>,
) -> HttpResult<Response> {
    let auth = require_auth!(state, headers, false);
    let prefixes = state
        .projects
        .resolve_scope_prefixes(&auth.projects, body.project.as_deref())
        .await
        .map_err(|e| map_err(&e))?;
    let filters = SearchFilters {
        path_prefixes: if prefixes.first().map(String::as_str) == Some("*") { None } else { Some(prefixes) },
        ..Default::default()
    };
    let _llm = state
        .llm_holder
        .read()
        .ok()
        .and_then(|g| g.clone());
    let state_for_query = state.clone();
    let llm_getter: crate::services::query::LlmGetter = Box::new(move || {
        state_for_query
            .llm_holder
            .read()
            .ok()
            .and_then(|g| g.clone())
    });
    let query = crate::services::query::QueryService::new(
        state.store.clone(),
        llm_getter,
        state.search.clone(),
    );
    match query
        .answer(&body.question, Some(&filters), Some(auth.name.as_str()))
        .await
    {
        Ok(answer) => Ok(Json(serde_json::to_value(answer).unwrap_or(Value::Null)).into_response()),
        Err(Error::Provider(msg)) if msg.contains("No LLM provider") => Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "llm_not_configured", "message": msg })),
        )
            .into_response()),
        Err(e) => Err(map_err(&e)),
    }
}

async fn changes_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    let changes = state
        .store
        .list_changes(
            q.get("since").map(String::as_str),
            q.get("path").map(String::as_str),
            q.get("source").map(String::as_str),
            q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(100),
        )
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "changes": changes })))
}

// -- events / ws -------------------------------------------------------------

async fn events_sse(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> > {
    require_auth!(state, headers, true);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    state.broadcaster.add(crate::services::broadcaster::ClientSink::Sse(tx)).await;
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|payload| {
            let event = Event::default().event("change").data(payload);
            (Ok::<Event, std::convert::Infallible>(event), rx)
        })
    });
    Ok(Sse::new(stream))
}

async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> HttpResult<axum::response::Response> {
    require_auth!(state, headers, true);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state.broadcaster.add(crate::services::broadcaster::ClientSink::Ws(tx)).await;
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(payload) => {
                            if socket.send(axum::extract::ws::Message::Text(payload.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                frame = socket.recv() => {
                    match frame {
                        Some(Ok(_)) => continue,
                        _ => break,
                    }
                }
            }
        }

    }))
}

// -- ingest / graph / okf / bundle ------------------------------------------

#[derive(serde::Deserialize)]
struct IngestBody {
    source: IngestSource,
    #[serde(default)]
    operations: Vec<BatchOperation>,
    #[serde(default)]
    log_entry: Option<String>,
}

#[derive(serde::Deserialize)]
struct IngestSource {
    title: String,
    rel_path: String,
    #[serde(default)]
    content: Option<String>,
}

async fn ingest_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IngestBody>,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    if let Some(content) = &body.source.content {
        let rel = format!("raw/{}", body.source.rel_path.trim_start_matches('/'));
        let abs = std::path::Path::new(&state.config.wiki_root).join(&rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| map_err(&Error::Io(e)))?;
        }
        std::fs::write(&abs, content).map_err(|e| map_err(&Error::Io(e)))?;
        state
            .pipeline
            .index_external_content(&rel, content, "wiki", Some(&body.source.title), None, None)
            .await
            .map_err(|e| map_err(&e))?;
    }
    for op in &body.operations {
        if let Some(content) = &op.content {
            write_page_via_pipeline(&state, "ingest", &op.rel_path, content.clone(), op.frontmatter.clone(), None)
                .await?;
        }
    }
    Ok(Json(json!({ "success": true })))
}

async fn graph_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(rel_path): AxumPath<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> HttpResult<Response> {
    require_auth!(state, headers, true);
    let depth = q.get("depth").and_then(|d| d.parse().ok()).unwrap_or(1);
    respond_graph(&state, &rel_path, depth, q.get("format").map(String::as_str)).await
}

async fn graph_by_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> HttpResult<Response> {
    require_auth!(state, headers, true);
    let path = q.get("path").cloned().unwrap_or_default();
    let depth = q.get("depth").and_then(|d| d.parse().ok()).unwrap_or(1);
    respond_graph(&state, &path, depth, q.get("format").map(String::as_str)).await
}

async fn respond_graph(
    state: &AppState,
    rel_path: &str,
    depth: i64,
    format: Option<&str>,
) -> HttpResult<Response> {
    let view = state.graph.neighbors(rel_path, depth).await.map_err(|e| map_err(&e))?;
    if format == Some("dot") {
        let mut lines = vec!["digraph knowledge {".to_string()];
        for node in &view.nodes {
            let label = node.title.clone().unwrap_or_else(|| node.rel_path.clone()).replace('"', "'");
            lines.push(format!("  \"{}\" [label=\"{label}\"];", node.rel_path));
        }
        for edge in &view.edges {
            lines.push(format!("  \"{}\" -> \"{}\";", edge.src, edge.dst));
        }
        lines.push("}".into());
        return Ok((
            [(axum::http::header::CONTENT_TYPE, "text/vnd.graphviz; charset=utf-8")],
            lines.join("\n"),
        )
            .into_response());
    }
    Ok(Json(serde_json::to_value(view).unwrap_or(Value::Null)).into_response())
}

#[derive(serde::Deserialize)]
struct OkfValidateBody {
    #[serde(default)]
    content: Option<String>,
}

async fn okf_validate(
    State(state): State<AppState>,
    Json(body): Json<OkfValidateBody>,
) -> HttpResult<Json<Value>> {
    match body.content {
        Some(content) => {
            let issues = crate::okf::validate::validate_concept_file("concept.md", &content);
            Ok(Json(json!({ "valid": issues.is_empty(), "issues": issues })))
        }
        None => {
            let report = state.okf.validate_bundle().await.map_err(|e| map_err(&e))?;
            Ok(Json(serde_json::to_value(report).unwrap_or(Value::Null)))
        }
    }
}

async fn okf_layout(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "profile": state.okf.layout_profile().await }))
}

async fn bundle_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> HttpResult<impl IntoResponse> {
    let auth = require_auth!(state, headers, false);
    if auth.role == "read" {
        return Err(err_status(StatusCode::FORBIDDEN, "forbidden", "read role cannot export"));
    }
    let paths: Option<Vec<String>> =
        if q.values().all(|v| v.is_empty()) || q.is_empty() {
            None
        } else {
            let mut set = std::collections::BTreeSet::new();
            let mut cursor: Option<String> = None;
            loop {
                let filters = SearchFilters {
                    kinds: q.get("kind").map(|k| vec![k.clone()]),
                    origins: q.get("origin").map(|o| vec![o.clone()]),
                    path_prefixes: q.get("prefix").map(|p| vec![p.clone()]),
                    ..Default::default()
                };
                let page = state
                    .store
                    .list_documents(
                        &ListOptions { filters: Some(filters), ..Default::default() },
                        1000,
                        cursor.as_deref(),
                    )
                    .await
                    .map_err(|e| map_err(&e))?;
                for doc in &page.items {
                    if doc.origin == "wiki" {
                        set.insert(doc.rel_path.clone());
                    }
                }
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            if let Some(since) = q.get("since") {
                let changed: std::collections::BTreeSet<String> = state
                    .store
                    .list_changes(Some(since), None, None, 10_000)
                    .await
                    .map_err(|e| map_err(&e))?
                    .into_iter()
                    .map(|c| c.rel_path)
                    .collect();
                set.retain(|p| changed.contains(p));
            }
            Some(set.into_iter().collect())
        };

    let bytes = crate::services::bundle::export_files(&state.config.wiki_root, paths.as_deref())
        .map_err(|e| map_err(&e))?;
    let count = paths.as_ref().map(|p| p.len()).unwrap_or(0);
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/gzip"),
            (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"wikillm-bundle.tar.gz\""),
        ],
        [("X-Exported-Files", count.to_string())],
        bytes,
    ))
}

async fn bundle_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    if auth.role != "admin" {
        return Err(err_status(StatusCode::FORBIDDEN, "forbidden", "admin only"));
    }
    let force = q.get("force").map(|v| v == "true").unwrap_or(false);
    let (imported, conflicts) =
        crate::services::bundle::import_bytes(&state.config.wiki_root, &body, force)
            .map_err(|e| map_err(&e))?;
    if !conflicts.is_empty() {
        return Err(err_status(
            StatusCode::CONFLICT,
            "exists",
            &format!("conflicts: {}", conflicts.join(", ")),
        ));
    }
    Ok(Json(json!({ "imported": imported })))
}

// -- connectors ---------------------------------------------------------------

async fn connectors_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, true);
    require_admin_role(&auth)?;
    let list = state.store.list_connectors().await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "connectors": list })))
}

#[derive(serde::Deserialize)]
struct ConnectorBody {
    kind: String,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    id: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

async fn connectors_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ConnectorBody>,
) -> HttpResult<(StatusCode, Json<Value>)> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let now = chrono::Utc::now().to_rfc3339();
    let connector = ConnectorConfig {
        id: body.id.unwrap_or_else(|| format!("{}-{}", body.kind, &ulid::Ulid::new().to_string()[..8].to_lowercase())),
        kind: body.kind.clone(),
        config: body.config,
        enabled: body.enabled,
        created_at: now.clone(),
        updated_at: now,
    };
    state.store.put_connector(&connector).await.map_err(|e| map_err(&e))?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(connector).unwrap_or(Value::Null))))
}

async fn connectors_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let removed = state.store.delete_connector(&id).await.map_err(|e| map_err(&e))?;
    if removed {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(err_status(StatusCode::NOT_FOUND, "not_found", "Unknown connector"))
    }
}

async fn connectors_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let connector = state
        .store
        .get_connector(&id)
        .await
        .map_err(|e| map_err(&e))?
        .ok_or_else(|| err_status(StatusCode::NOT_FOUND, "not_found", "Unknown connector"))?;
    let state_value = state.store.get_connector_state(&id).await.map_err(|e| map_err(&e))?;
    let (docs, new_state) = match connector.kind.as_str() {
        "git" => crate::services::connectors::git::poll(&connector.config, &state_value.unwrap_or(Value::Null)).await.map_err(|e| map_err(&e))?,
        "web" => crate::services::connectors::web::poll(&connector.config, &state_value.unwrap_or(Value::Null)).await.map_err(|e| map_err(&e))?,
        "github" => crate::services::connectors::github::poll(&connector.config, &state_value.unwrap_or(Value::Null)).await.map_err(|e| map_err(&e))?,
        other => return Err(err_status(StatusCode::BAD_REQUEST, "validation", &format!("Unknown kind: {other}"))),
    };
    let count = docs.len();
    for (path, content, title, mtime) in docs {
        state
            .pipeline
            .index_external_content(&format!("{id}/{path}"), &content, &id, Some(&title), None, Some(mtime))
            .await
            .map_err(|e| map_err(&e))?;
    }
    state.store.set_connector_state(&id, &new_state).await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "docs": count })))
}

// -- projects ----------------------------------------------------------------

async fn projects_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    Ok(Json(json!({ "projects": state.projects.list().await.map_err(|e| map_err(&e))? })))
}

async fn project_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    require_auth!(state, headers, true);
    match state.projects.get(&name).await.map_err(|e| map_err(&e))? {
        Some(p) => Ok(Json(serde_json::to_value(p).unwrap_or(Value::Null))),
        None => Err(err_status(StatusCode::NOT_FOUND, "not_found", "Project not found")),
    }
}

#[derive(serde::Deserialize)]
struct ProjectBody {
    prefixes: Vec<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    connectors: Vec<String>,
}

async fn project_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<ProjectBody>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let input = ProjectInput {
        name: name.clone(),
        description: body.description,
        prefixes: body.prefixes,
        connectors: body.connectors,
    };
    state.projects.put(&input).await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "name": name, "saved": true })))
}

async fn project_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let removed = state.projects.delete(&name).await.map_err(|e| map_err(&e))?;
    if removed {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(err_status(StatusCode::NOT_FOUND, "not_found", "Project not found"))
    }
}

// -- settings / keys ----------------------------------------------------------

async fn settings_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, true);
    require_admin_role(&auth)?;
    Ok(Json(json!({ "settings": state.settings.describe().await.map_err(|e| map_err(&e))? })))
}

async fn setting_get(
    State(state): State<AppState>,
    _headers: HeaderMap,
    AxumPath(key): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let all = state.settings.describe().await.map_err(|e| map_err(&e))?;
    all.into_iter()
        .find(|s| s.get("key").and_then(|k| k.as_str()) == Some(&key))
        .map(Json)
        .ok_or_else(|| err_status(StatusCode::NOT_FOUND, "not_found", &format!("Unknown setting: {key}")))
}

async fn setting_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(key): AxumPath<String>,
    Json(body): Json<Value>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let value = body.get("value").cloned().unwrap_or(Value::Null);
    let reindex_required = state
        .settings
        .set(&key, value, &auth.name)
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({
        "key": key,
        "applied": true,
        "reindex_required": reindex_required,
    })))
}

async fn setting_reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(key): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let removed = state.settings.reset(&key).await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "key": key, "reset": removed })))
}

async fn keys_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, true);
    require_admin_role(&auth)?;
    let keys = state.store.list_api_keys().await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "keys": keys })))
}

#[derive(serde::Deserialize)]
struct KeyCreateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default = "default_scope")]
    scope: Vec<String>,
}
fn default_role() -> String {
    "write".into()
}
fn default_scope() -> Vec<String> {
    vec!["*".into()]
}

async fn keys_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeyCreateBody>,
) -> HttpResult<(StatusCode, Json<Value>)> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let created = state
        .keys
        .create_key(body.name.as_deref(), None, &body.role, &body.scope, &auth.name)
        .await
        .map_err(|e| map_err(&e))?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(created).unwrap_or(Value::Null))))
}

async fn keys_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let removed = state.keys.delete_key(&name).await.map_err(|e| map_err(&e))?;
    if removed {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(err_status(StatusCode::NOT_FOUND, "not_found", "Unknown key name"))
    }
}

// -- admin / feedback / webhooks ---------------------------------------------

async fn admin_reindex(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let documents = state.pipeline.reindex_all().await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "documents": documents })))
}

async fn admin_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, true);
    require_admin_role(&auth)?;
    let stats = state.store.stats_overview().await.map_err(|e| map_err(&e))?;
    Ok(Json(serde_json::to_value(stats).unwrap_or(Value::Null)))
}

#[derive(serde::Deserialize)]
struct FeedbackBody {
    query_id: String,
    helpful: bool,
    #[serde(default)]
    comment: Option<String>,
}

async fn feedback_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FeedbackBody>,
) -> HttpResult<Json<Value>> {
    let _ = require_auth!(state, headers, false);
    state
        .store
        .record_feedback(&body.query_id, body.helpful, body.comment.as_deref())
        .await
        .map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "success": true })))
}

async fn webhooks_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, true);
    require_admin_role(&auth)?;
    let hooks = state.store.list_webhooks().await.map_err(|e| map_err(&e))?;
    Ok(Json(json!({ "webhooks": hooks })))
}

#[derive(serde::Deserialize)]
struct WebhookBody {
    url: String,
    #[serde(default = "default_events")]
    events: Vec<String>,
    #[serde(default = "default_scope_vec")]
    prefixes: Vec<String>,
    #[serde(default = "default_true2")]
    enabled: bool,
}
fn default_events() -> Vec<String> {
    vec!["change".into()]
}
fn default_scope_vec() -> Vec<String> {
    vec!["*".into()]
}
fn default_true2() -> bool {
    true
}

async fn webhook_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WebhookBody>,
) -> HttpResult<(StatusCode, Json<Value>)> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let hook = WebhookRecord {
        id: format!("wh-{}", &ulid::Ulid::new().to_string()[..8].to_lowercase()),
        url: body.url,
        events: body.events,
        prefixes: body.prefixes,
        enabled: body.enabled,
        last_status: None,
        last_attempt_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.store.put_webhook(&hook).await.map_err(|e| map_err(&e))?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(hook).unwrap_or(Value::Null))))
}

async fn webhook_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> HttpResult<Json<Value>> {
    let auth = require_auth!(state, headers, false);
    require_admin_role(&auth)?;
    let removed = state.store.delete_webhook(&id).await.map_err(|e| map_err(&e))?;
    if removed {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(err_status(StatusCode::NOT_FOUND, "not_found", "Unknown webhook"))
    }
}

fn require_admin_role(auth: &AuthInfo) -> Result<(), (StatusCode, Json<Value>)> {
    if auth.role != "admin" {
        Err(err_status(StatusCode::FORBIDDEN, "forbidden", "admin role required"))
    } else {
        Ok(())
    }
}
