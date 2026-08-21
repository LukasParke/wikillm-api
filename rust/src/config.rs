//! Environment configuration. Mirrors the TypeScript `config.ts`.

use crate::domain::Source;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyEntry {
    pub name: String,
    pub key: String,
    pub projects: Vec<String>,
    pub role: String,
}

/// `name:key[:scope[:role]]` — scope is comma-separated project names or `*`;
/// role is admin|write|read (default write).
pub fn parse_api_keys(raw: &str) -> Result<HashMap<String, ApiKeyEntry>, String> {
    let mut map: HashMap<String, ApiKeyEntry> = HashMap::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let segments: Vec<String> = trimmed.split(':').map(|s| s.trim().to_string()).collect();
        if segments.len() < 2 {
            return Err(format!(
                "Invalid API_KEYS entry: {trimmed}. Expected name:key[:scope[:role]]"
            ));
        }
        let name = segments[0].clone();
        let key = segments[1].clone();
        if name.is_empty() || key.is_empty() {
            return Err(format!("Invalid API_KEYS entry: {trimmed}"));
        }
        if map.values().any(|e| e.key == key) {
            return Err(format!("Duplicate API key: {key}"));
        }
        let scope: Vec<String> = match segments.get(2).map(String::as_str) {
            None | Some("") => vec!["*".to_string()],
            Some("*") => vec!["*".to_string()],
            Some(s) => s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect(),
        };
        let scope = if scope.is_empty() { vec!["*".to_string()] } else { scope };
        let role = match segments.get(3).map(String::as_str) {
            Some(r) if r == "admin" || r == "read" => r.to_string(),
            _ => "write".to_string(),
        };
        map.insert(key.to_string(), ApiKeyEntry {
            name: name.to_string(),
            key: key.to_string(),
            projects: scope,
            role,
        });
    }
    Ok(map)
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[derive(Debug, Clone)]
pub struct Config {
    pub wiki_root: String,
    pub port: i32,
    pub host: String,
    /// env bootstrap keys; may be empty (bootstrap flow mints an admin key)
    pub api_keys: HashMap<String, ApiKeyEntry>,
    pub public_read: bool,
    pub db_path: String,
    pub log_level: String,
    pub db_backend: String,
    pub database_url: Option<String>,
    pub layout: String,
    pub okf_strict: bool,
    pub human_actors: Vec<String>,
    pub llm_base_url: Option<String>,
    pub llm_api_key: Option<String>,
    pub llm_model: String,
    pub llm_embed_model: Option<String>,
    pub embedding_dims: i64,
    pub llm_distill: bool,
    pub connector_poll_seconds: i64,
    pub rate_limit_rpm: i64,
}

pub fn bool_env(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => !v.eq_ignore_ascii_case("false"),
        Err(_) => default,
    }
}

pub fn load_config() -> Result<Config, String> {
    let wiki_root = std::env::var("WIKI_ROOT").map_err(|_| "config: WIKI_ROOT is required")?;
    let port: i32 = env_or("PORT", "3000")
        .parse()
        .map_err(|_| "config: PORT must be an integer")?;
    let api_keys = parse_api_keys(&std::env::var("API_KEYS").unwrap_or_default())
        .map_err(|e| format!("config: {e}"))?;
    let db_backend = env_or("DB_BACKEND", "auto");
    if !["auto", "sqlite", "postgres"].contains(&db_backend.as_str()) {
        return Err(format!("config: invalid DB_BACKEND {db_backend}"));
    }
    let layout = env_or("LAYOUT", "auto");
    if !["auto", "okf", "wikillm"].contains(&layout.as_str()) {
        return Err(format!("config: invalid LAYOUT {layout}"));
    }
    let embedding_dims: i64 = env_or("EMBEDDING_DIMS", "1536")
        .parse()
        .map_err(|_| "config: EMBEDDING_DIMS must be an integer")?;
    if !(64..=4096).contains(&embedding_dims) {
        return Err("config: EMBEDDING_DIMS out of range 64..4096".into());
    }
    let connector_poll_seconds: i64 = env_or("CONNECTOR_POLL_SECONDS", "300")
        .parse()
        .map_err(|_| "config: CONNECTOR_POLL_SECONDS must be an integer")?;
    let rate_limit_rpm: i64 = env_or("RATE_LIMIT_RPM", "0")
        .parse()
        .map_err(|_| "config: RATE_LIMIT_RPM must be an integer")?;

    Ok(Config {
        human_actors: std::env::var("HUMAN_ACTORS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        wiki_root,
        port,
        host: env_or("HOST", "0.0.0.0"),
        api_keys,
        public_read: bool_env("PUBLIC_READ", true),
        db_path: std::env::var("DB_PATH").unwrap_or_else(|_| "wikillm-api.db".into()),
        log_level: env_or("LOG_LEVEL", "info"),
        db_backend,
        database_url: std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()),
        layout,
        okf_strict: bool_env("OKF_STRICT", false),
        llm_base_url: std::env::var("LLM_BASE_URL").ok().filter(|s| !s.is_empty()),
        llm_api_key: std::env::var("LLM_API_KEY").ok(),
        llm_model: env_or("LLM_MODEL", "llama3.1"),
        llm_embed_model: std::env::var("LLM_EMBED_MODEL").ok().filter(|s| !s.is_empty()),
        embedding_dims,
        llm_distill: bool_env("LLM_DISTILL", false),
        connector_poll_seconds,
        rate_limit_rpm,
    })
}

/// Back-compat helper mirroring the TS shape (key -> source name).
pub fn key_to_source_map(keys: &HashMap<String, ApiKeyEntry>) -> HashMap<String, Source> {
    keys.iter().map(|(k, v)| (k.clone(), v.name.clone())).collect()
}
