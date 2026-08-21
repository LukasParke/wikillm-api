//! Pluggable text-embedding backends, ported from TypeScript
//! `src/llm/embedder.ts`.
//!
//! Two providers:
//!  - api  : any OpenAI-compatible `/embeddings` endpoint
//!  - onnx : in-process ONNX inference (stub; real implementation deferred)

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::llm::provider::parse_embeddings_response;

use serde_json::json;

#[async_trait]
pub trait EmbedderLike: Send + Sync {
    fn model(&self) -> &str;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// OpenAI-compatible `/embeddings` embedder.
pub struct ApiEmbedder {
    /// Built once so connections are reused.
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    #[allow(dead_code)]
    dims: i64,
}

const EMBED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl ApiEmbedder {
    pub fn new(base_url: &str, api_key: Option<&str>, model: &str, dims: i64) -> Self {
        ApiEmbedder {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.filter(|k| !k.is_empty()).map(str::to_string),
            model: model.to_string(),
            dims,
        }
    }
}

#[async_trait]
impl EmbedderLike for ApiEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut req = self
            .client
            .post(format!("{}/embeddings", self.base_url))
            .timeout(EMBED_TIMEOUT)
            .json(&json!({ "model": self.model, "input": texts }));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|err| Error::Provider(format!("Embedding request failed: {err}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!(
                "Embedding endpoint {}: {body}",
                status.as_u16()
            )));
        }
        let payload: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| Error::Provider("Malformed embeddings response".into()))?;
        parse_embeddings_response(&payload, texts.len())
    }
}

/// In-process ONNX embedder. The real inference implementation is deferred;
/// the constructor signature is kept stable for the container.
pub struct OnnxEmbedder {
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    dtype: String,
    #[allow(dead_code)]
    device: String,
}

pub fn create_onnx(model: &str, dtype: &str, device: &str) -> OnnxEmbedder {
    OnnxEmbedder {
        model: model.to_string(),
        dtype: dtype.to_string(),
        device: device.to_string(),
    }
}

#[async_trait]
impl EmbedderLike for OnnxEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        #[cfg(not(feature = "onnx"))]
        {
            let _ = self;
            Err(Error::Provider("onnx feature not enabled".into()))
        }
        #[cfg(feature = "onnx")]
        {
            let _ = self;
            Err(Error::Provider("onnx embedder implementation deferred".into()))
        }
    }
}

/// Settings-driven selection, mirroring TS `resolveEmbedder`.
///
/// - `none`            → no embedder
/// - `onnx`            → ONNX stub
/// - `api`             → API embedder when `api_base` is set
/// - `auto`            → API embedder iff `api_base` AND `api_model` are set
pub fn resolve_embedder(
    provider: &str,
    api_base: &str,
    api_key: Option<&str>,
    api_model: &str,
    dims: i64,
) -> Option<Box<dyn EmbedderLike>> {
    match provider {
        "none" => None,
        "onnx" => Some(Box::new(create_onnx("", "", ""))),
        "api" => {
            if api_base.is_empty() {
                None
            } else {
                Some(Box::new(ApiEmbedder::new(
                    api_base,
                    api_key,
                    api_model,
                    dims,
                )))
            }
        }
        "auto" => {
            if api_base.is_empty() || api_model.trim().is_empty() {
                None
            } else {
                Some(Box::new(ApiEmbedder::new(
                    api_base,
                    api_key,
                    api_model,
                    dims,
                )))
            }
        }
        _ => None,
    }
}
