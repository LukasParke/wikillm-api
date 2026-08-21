//! Runtime settings: DB override > env > default, with a short-TTL cache and
//! change hooks so hot-appliable knobs (LLM provider, rate limits, connector
//! polling) apply without restart.

use crate::config::Config;
use crate::store::Store;
use crate::error::{Error, Result};
use serde_json::Value;
use std::sync::RwLock;

#[derive(Clone, Copy, PartialEq)]
pub struct SettingMeta {
    pub key: &'static str,
    pub kind: &'static str,
    pub mutable: bool,
    pub requires_reindex: bool,
}

pub const SETTINGS: &[SettingMeta] = &[
    SettingMeta { key: "public_read", kind: "bool", mutable: true, requires_reindex: false },
    SettingMeta { key: "rate_limit_rpm", kind: "int", mutable: true, requires_reindex: false },
    SettingMeta { key: "connector_poll_seconds", kind: "int", mutable: true, requires_reindex: false },
    SettingMeta { key: "llm_base_url", kind: "string", mutable: true, requires_reindex: false },
    SettingMeta { key: "llm_api_key", kind: "secret", mutable: true, requires_reindex: false },
    SettingMeta { key: "llm_model", kind: "string", mutable: true, requires_reindex: false },
    SettingMeta { key: "llm_embed_model", kind: "string", mutable: true, requires_reindex: false },
    SettingMeta { key: "embedding_dims", kind: "int", mutable: true, requires_reindex: true },
    SettingMeta { key: "embedding_provider", kind: "enum", mutable: true, requires_reindex: true },
    SettingMeta { key: "onnx_model", kind: "string", mutable: true, requires_reindex: true },
    SettingMeta { key: "onnx_dtype", kind: "enum", mutable: true, requires_reindex: true },
    SettingMeta { key: "onnx_device", kind: "string", mutable: true, requires_reindex: true },
    SettingMeta { key: "llm_distill", kind: "bool", mutable: true, requires_reindex: false },
    SettingMeta { key: "okf_strict", kind: "bool", mutable: true, requires_reindex: false },
    SettingMeta { key: "human_actors", kind: "string", mutable: true, requires_reindex: false },
    SettingMeta { key: "max_upload_mb", kind: "int", mutable: true, requires_reindex: false },
    SettingMeta { key: "webhook_secret", kind: "secret", mutable: true, requires_reindex: false },
    SettingMeta { key: "layout", kind: "enum", mutable: true, requires_reindex: false },
];

const IMMUTABLE: &[&str] = &["wiki_root", "port", "host", "db_backend", "database_url"];

type Hooks = Vec<Box<dyn Fn(&str, &Value) + Send + Sync>>;

pub struct SettingsService {
    store: std::sync::Arc<dyn Store>,
    config: Config,
    cache: RwLock<Option<(std::time::Instant, Value)>>,
    hooks: RwLock<Hooks>,
    ttl: std::time::Duration,
}

impl SettingsService {
    pub fn new(store: std::sync::Arc<dyn Store>, config: Config) -> Self {
        Self {
            store,
            config,
            cache: RwLock::new(None),
            hooks: RwLock::new(Vec::new()),
            ttl: std::time::Duration::from_secs(1),
        }
    }

    pub fn on_change(&self, hook: impl Fn(&str, &Value) + Send + Sync + 'static) {
        self.hooks.write().expect("settings hooks lock").push(Box::new(hook));
    }

    fn env_value(&self, key: &str) -> Option<Value> {
        match key {
            "public_read" => Some(Value::Bool(self.config.public_read)),
            "rate_limit_rpm" => Some(Value::from(self.config.rate_limit_rpm)),
            "connector_poll_seconds" => Some(Value::from(self.config.connector_poll_seconds)),
            "llm_base_url" => self.config.llm_base_url.clone().map(Value::String).or(Some(Value::Null)),
            "llm_api_key" => self.config.llm_api_key.clone().map(Value::String),
            "llm_model" => Some(Value::String(self.config.llm_model.clone())),
            "llm_embed_model" => self.config.llm_embed_model.clone().map(Value::String),
            "embedding_dims" => Some(Value::from(self.config.embedding_dims)),
            "embedding_provider" => Some(Value::String(
                std::env::var("EMBEDDING_PROVIDER").unwrap_or_else(|_| "auto".into()),
            )),
            "onnx_model" => Some(Value::String(
                std::env::var("ONNX_MODEL")
                    .unwrap_or_else(|_| "Xenova/bge-small-en-v1.5".into()),
            )),
            "onnx_dtype" => Some(Value::String(std::env::var("ONNX_DTYPE").unwrap_or_else(|_| "q8".into()))),
            "onnx_device" => Some(Value::String(std::env::var("ONNX_DEVICE").unwrap_or_else(|_| "cpu".into()))),
            "llm_distill" => Some(Value::Bool(self.config.llm_distill)),
            "okf_strict" => Some(Value::Bool(self.config.okf_strict)),
            "human_actors" => Some(Value::String(self.config.human_actors.join(","))),
            "max_upload_mb" => Some(Value::from(100i64)),
            "webhook_secret" => Some(Value::Null),
            "layout" => Some(Value::String(self.config.layout.clone())),
            _ => None,
        }
    }

    async fn resolved(&self) -> Result<serde_json::Map<String, Value>> {
        {
            let cache = self.cache.read().expect("cache lock");
            if let Some((at, values)) = cache.as_ref() {
                if at.elapsed() < self.ttl {
                    return Ok(values.as_object().cloned().unwrap_or_default());
                }
            }
        }
        let overrides = self.store.get_settings().await?;
        let mut merged = serde_json::Map::new();
        for meta in SETTINGS {
            let value = overrides.get(meta.key).cloned().or_else(|| self.env_value(meta.key)).unwrap_or(Value::Null);
            merged.insert(meta.key.to_string(), value);
        }
        *self.cache.write().expect("cache lock") =
            Some((std::time::Instant::now(), Value::Object(merged.clone())));
        Ok(merged)
    }

    /// Repopulate the cache immediately (used after writes so change hooks
    /// observe fresh values).
    pub async fn warm(&self) -> Result<()> {
        self.resolved().await.map(|_| ())
    }

    pub async fn get_string(&self, key: &str) -> Result<String> {
        let resolved = self.resolved().await?;
        Ok(match resolved.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        })
    }

    pub async fn get_bool(&self, key: &str) -> Result<bool> {
        Ok(matches!(self.resolved().await?.get(key), Some(Value::Bool(true))))
    }

    pub async fn get_i64(&self, key: &str) -> Result<i64> {
        Ok(match self.resolved().await?.get(key) {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            _ => 0,
        })
    }

    /// Masked view for API responses. Secrets render as `<set>`/`<unset>`.
    pub async fn describe(&self) -> Result<Vec<serde_json::Value>> {
        let resolved = self.resolved().await?;
        let overrides = self.store.get_settings().await?;
        let has_override =
            |key: &str| overrides.get(key).map(|v| !v.is_null()).unwrap_or(false);
        let mut out = Vec::new();
        for meta in SETTINGS {
            let raw = resolved.get(meta.key).cloned().unwrap_or(Value::Null);
            let masked = if meta.kind == "secret" {
                match &raw {
                    Value::String(s) if !s.is_empty() => Value::String("<set>".into()),
                    _ => Value::String("<unset>".into()),
                }
            } else {
                raw.clone()
            };
            out.push(serde_json::json!({
                "key": meta.key,
                "type": meta.kind,
                "value": masked,
                "overridden": has_override(meta.key),
                "mutable": meta.mutable,
                "requires_reindex": meta.requires_reindex,
            }));
        }
        for key in IMMUTABLE {
            out.push(serde_json::json!({ "key": key, "mutable": false }));
        }
        Ok(out)
    }

    pub async fn set(&self, key: &str, value: Value, updated_by: &str) -> Result<bool> {
        let meta = SETTINGS.iter().find(|m| m.key == key);
        let meta = match meta {
            Some(m) => *m,
            None => {
                if IMMUTABLE.contains(&key) {
                    return Err(Error::Validation(format!("setting is immutable (deployment-level): {key}")));
                }
                return Err(Error::NotFound(format!("Unknown setting: {key}")));
            }
        };
        validate_setting(key, &value)?;
        let previous = self.resolved().await?.get(key).cloned();
        self.store.set_setting(key, &value, updated_by).await?;
        self.cache.write().expect("cache lock").take();
        let reindex_required = meta.requires_reindex && previous.as_ref() != Some(&value);
        if reindex_required && key == "embedding_dims" {
            let dims = value.as_i64().map(|n| n as i32);
            self.store.reset_embeddings(dims).await?;
        }
        {
            let hooks = self.hooks.read().expect("hooks lock");
            for hook in hooks.iter() {
                hook(key, &value);
            }
        }
        Ok(reindex_required)
    }

    pub async fn reset(&self, key: &str) -> Result<bool> {
        if !SETTINGS.iter().any(|m| m.key == key) {
            if IMMUTABLE.contains(&key) {
                return Err(Error::Validation(format!("setting is immutable (deployment-level): {key}")));
            }
            return Err(Error::NotFound(format!("Unknown setting: {key}")));
        }
        let removed = self.store.delete_setting(key).await?;
        if removed {
            self.cache.write().expect("cache lock").take();
            let env = self.env_value(key).unwrap_or(Value::Null);
            let hooks = self.hooks.read().expect("hooks lock");
            for hook in hooks.iter() {
                hook(key, &env);
            }
        }
        Ok(removed)
    }
}

fn validate_setting(key: &str, value: &Value) -> Result<()> {
    let ok = match (key, value) {
        ("public_read", Value::Bool(_))
        | ("llm_distill", Value::Bool(_))
        | ("okf_strict", Value::Bool(_)) => true,
        ("rate_limit_rpm", Value::Number(n)) => (0..=1_000_000).contains(&n.as_i64().unwrap_or(-1)),
        ("connector_poll_seconds", Value::Number(n)) => (5..=86_400).contains(&n.as_i64().unwrap_or(-1)),
        ("max_upload_mb", Value::Number(n)) => (1..=4096).contains(&n.as_i64().unwrap_or(-1)),
        ("embedding_dims", Value::Number(n)) => (64..=4096).contains(&n.as_i64().unwrap_or(-1)),
        ("embedding_provider", Value::String(s)) => ["none", "api", "onnx", "auto"].contains(&s.as_str()),
        ("onnx_dtype", Value::String(s)) => ["q8", "fp16", "fp32", "quantized", "auto"].contains(&s.as_str()),
        ("layout", Value::String(s)) => ["auto", "okf", "wikillm"].contains(&s.as_str()),
        ("llm_model", Value::String(s)) => !s.is_empty(),
        ("onnx_model", Value::String(s)) => !s.is_empty(),
        ("onnx_device", Value::String(s)) => !s.is_empty(),
        (_, Value::Null) => true,
        (_, Value::String(_)) => true,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(format!("Invalid value for setting {key}")))
    }
}
