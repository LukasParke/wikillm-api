//! Bearer auth: env bootstrap keys + DB-managed hashed keys via KeyRegistry.
//! When public read is enabled (runtime setting), unauthenticated GETs pass
//! through as anonymous.

use crate::services::keys::KeyRegistry;
use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct AuthInfo {
    pub name: String,
    pub role: String,
    pub projects: Vec<String>,
}

pub const ANONYMOUS: &str = "anonymous";

#[derive(Clone)]
pub struct AuthState {
    pub registry: std::sync::Arc<KeyRegistry>,
    pub public_read: std::sync::Arc<tokio::sync::RwLock<bool>>,
}

pub async fn resolve_auth(
    state: &AuthState,
    headers: &HeaderMap,
    method_get: bool,
) -> Result<AuthInfo, (StatusCode, Json<serde_json::Value>)> {
    let header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Some(secret) = header.strip_prefix("Bearer ").or_else(|| header.strip_prefix("bearer ")) {
        if let Some(auth) = state
            .registry
            .verify(secret)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal","message":e.to_string()}))))?
        {
            return Ok(auth);
        }
        if !*state.public_read.read().await {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized", "message": "Invalid API key"})),
            ));
        }
    }
    if method_get && *state.public_read.read().await {
        return Ok(AuthInfo {
            name: ANONYMOUS.into(),
            role: "read".into(),
            projects: vec!["*".into()],
        });
    }
    Err((
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized", "message": "Missing or invalid Authorization header"})),
    ))
}

/// Helper used by admin-gated routes.
pub fn require_admin(auth: &AuthInfo) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if auth.role == "admin" {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, Json(json!({"error": "forbidden"}))))
    }
}

pub fn require_write(auth: &AuthInfo) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if auth.role == "admin" || auth.role == "write" {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, Json(json!({"error": "forbidden"}))))
    }
}

// Re-export for route handlers returning IntoResponse tuples.
#[allow(dead_code)]
pub fn internal_error(message: String) -> impl IntoResponse {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "internal", "message": message})),
    )
}
