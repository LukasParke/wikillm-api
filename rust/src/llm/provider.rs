//! OpenAI-compatible LLM provider: `/chat/completions` + `/embeddings`,
//! ported from TypeScript `src/llm/provider.ts`.
//!
//! Retry policy: network failures, HTTP 429 and 5xx are retried up to two
//! times with 250ms/1000ms backoff. Chat calls time out at 30s, embeddings
//! at 60s. All failures surface as [`Error::Provider`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// A single chat message as `(role, content)`.
pub type ChatMessage<'a> = (&'a str, &'a str);

const CHAT_TIMEOUT: Duration = Duration::from_secs(30);
const EMBED_TIMEOUT: Duration = Duration::from_secs(60);
/// Backoff between retries; `RETRY_DELAYS_MS.len()` retries total.
const RETRY_DELAYS_MS: [u64; 2] = [250, 1000];

/// Thread-safe shared handle to the active provider. The container can
/// hot-swap it behind `Arc<std::sync::RwLock<Option<DynLlmProvider>>>`.
pub type DynLlmProvider = Arc<dyn LlmProvider>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn model(&self) -> &str;
    fn embed_model(&self) -> Option<String>;
    fn embed_dims(&self) -> Option<i64>;
    async fn chat(
        &self,
        messages: &[ChatMessage<'_>],
        temperature: f32,
        max_tokens: i64,
    ) -> Result<String>;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

pub struct OpenAiCompatible {
    /// Built once per provider so connections are reused across calls.
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    embed_model: Option<String>,
    embed_dims: Option<i64>,
}

/// Build an OpenAI-compatible provider hitting `POST {base}/chat/completions`
/// and `POST {base}/embeddings`.
pub fn openai_compatible(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    embed_model: Option<&str>,
    embed_dims: Option<i64>,
) -> OpenAiCompatible {
    OpenAiCompatible {
        client: reqwest::Client::new(),
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key: api_key.filter(|k| !k.is_empty()).map(str::to_string),
        model: model.to_string(),
        embed_model: embed_model
            .filter(|m| !m.trim().is_empty())
            .map(str::to_string),
        embed_dims,
    }
}

/// Container-facing constructor taking plain strings from the environment
/// snapshot. Empty `api_key` means anonymous; empty `embed_model` disables
/// embeddings; `dims <= 0` means unset.
pub fn create_from_env_snapshot(
    base_url: &str,
    api_key: &str,
    model: &str,
    embed_model: &str,
    dims: i64,
) -> Option<DynLlmProvider> {
    // No valid base_url → no provider → rerank/embed/query all skip instantly
    if base_url.trim().is_empty() || !base_url.starts_with("http") {
        return None;
    }
    let embed_model = if embed_model.trim().is_empty() {
        None
    } else {
        Some(embed_model)
    };
    let dims = if dims > 0 { Some(dims) } else { None };
    Some(Arc::new(openai_compatible(
        base_url,
        Some(api_key),
        model,
        embed_model,
        dims,
    )))
}

impl OpenAiCompatible {
    async fn post_json(&self, url: &str, body: &Value, timeout: Duration) -> Result<Value> {
        let mut last_err = Error::Provider("Request failed".into());
        for attempt in 0..=RETRY_DELAYS_MS.len() {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[attempt - 1])).await;
            }
            let mut req = self.client.post(url).timeout(timeout).json(body);
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }
            match req.send().await {
                Err(err) => {
                    // Network-level failure (including request timeout).
                    last_err = Error::Provider(format!("Request failed: {err}"));
                }
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp.json::<Value>().await.map_err(|_| {
                            Error::Provider(format!("Invalid JSON response from {url}"))
                        });
                    }
                    last_err =
                        Error::Provider(format!("Request failed with status {}", status.as_u16()));
                    let retryable = status.as_u16() == 429
                        || (500..600).contains(&status.as_u16());
                    if !retryable {
                        return Err(last_err);
                    }
                }
            }
        }
        Err(last_err)
    }
}

/// Parse an OpenAI `/embeddings` response: sort by `index` when every entry
/// carries one; verify the count matches the request.
pub(crate) fn parse_embeddings_response(
    payload: &Value,
    expected_len: usize,
) -> Result<Vec<Vec<f32>>> {
    let data = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Provider("Malformed embeddings response".into()))?;
    if data.len() != expected_len {
        return Err(Error::Provider(format!(
            "Embedding count mismatch: {} for {}",
            data.len(),
            expected_len
        )));
    }
    let mut entries: Vec<(Option<i64>, Vec<f32>)> = Vec::with_capacity(data.len());
    for entry in data {
        let embedding = entry
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Provider("Malformed embeddings response".into()))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();
        let index = entry.get("index").and_then(Value::as_i64);
        entries.push((index, embedding));
    }
    if entries.iter().all(|(index, _)| index.is_some()) {
        entries.sort_by_key(|(index, _)| *index);
    }
    Ok(entries.into_iter().map(|(_, embedding)| embedding).collect())
}

#[async_trait]
impl LlmProvider for OpenAiCompatible {
    fn model(&self) -> &str {
        &self.model
    }

    fn embed_model(&self) -> Option<String> {
        self.embed_model.clone()
    }

    fn embed_dims(&self) -> Option<i64> {
        self.embed_dims
    }

    async fn chat(
        &self,
        messages: &[ChatMessage<'_>],
        temperature: f32,
        max_tokens: i64,
    ) -> Result<String> {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|(role, content)| json!({ "role": role, "content": content }))
            .collect();
        let body = json!({
            "model": self.model,
            "messages": msgs,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        let url = format!("{}/chat/completions", self.base_url);
        let payload = self.post_json(&url, &body, CHAT_TIMEOUT).await?;
        payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Provider("Malformed chat completion response".into()))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let Some(embed_model) = &self.embed_model else {
            return Err(Error::Provider("No embedding model configured".into()));
        };
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = json!({ "model": embed_model, "input": texts });
        let url = format!("{}/embeddings", self.base_url);
        let payload = self.post_json(&url, &body, EMBED_TIMEOUT).await?;
        parse_embeddings_response(&payload, texts.len())
    }
}
