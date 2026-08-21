//! Registry of the 41 MCP tools exposed over stdio, ported from `src/mcp/tools.ts`.
//!
//! Each tool carries its JSON-Schema-ish `inputSchema` (as a raw `serde_json`
//! object mirroring the zod definition) plus an async handler taking the call
//! arguments and an [`HttpClient`] pointed at the running WikiLLM API.

use std::future::Future;
use std::pin::Pin;

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::{json, Map, Value};

/// Result of a `tools/call`: text content plus optional error flag.
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(text: String) -> Self {
        Self {
            text,
            is_error: false,
        }
    }

    fn err(text: String) -> Self {
        Self {
            text: format!("Error: {text}"),
            is_error: true,
        }
    }
}

type BoxHandler =
    fn(HttpClient, Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

/// A registered tool: name, description, input schema, and async handler.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    handler: BoxHandler,
}

/// Thin HTTP client for the running WikiLLM API.
#[derive(Clone)]
pub struct HttpClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl HttpClient {
    /// Build from environment: `WIKILLM_URL` (default `http://127.0.0.1:3000`)
    /// and `WIKILLM_API_KEY` (default empty).
    pub fn from_env() -> Self {
        let base_url =
            std::env::var("WIKILLM_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());
        let api_key = std::env::var("WIKILLM_API_KEY").unwrap_or_default();
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

/// Non-2xx API response; message mirrors the TS `ApiError` formatting.
struct ApiError {
    status: u16,
    status_text: String,
    body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WikiLLM API {} {}: {}",
            self.status, self.status_text, self.body
        )
    }
}

const QUERY_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const PATH_SET: &AsciiSet = &CONTROLS.add(b'/').add(b'%').add(b'?').add(b'#');

fn enc_path(path: &str) -> String {
    path.split('/')
        .map(|seg| utf8_percent_encode(seg, PATH_SET).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn enc_query(value: &str) -> String {
    utf8_percent_encode(value, QUERY_SET).to_string()
}

enum Body {
    Json(Value),
    None,
}

/// Perform one API request. Returns parsed JSON (or a string value when the
/// body is not valid JSON). Errors with [`ApiError`] on non-2xx.
async fn api(
    client: &HttpClient,
    method: reqwest::Method,
    path: &str,
    body: Body,
) -> Result<Value, ApiError> {
    let mut req = client.http.request(method, client.url(path));
    if !client.api_key.is_empty() {
        req = req.bearer_auth(&client.api_key);
    }
    match body {
        Body::Json(v) => {
            req = req.header("Content-Type", "application/json").json(&v);
        }
        Body::None => {}
    }
    finish(req).await
}

/// Send a prepared request and convert the response to a [`Value`].
async fn finish(req: reqwest::RequestBuilder) -> Result<Value, ApiError> {
    let res = req.send().await.map_err(|e| ApiError {
        status: 0,
        status_text: "request failed".to_string(),
        body: e.to_string(),
    })?;
    let status = res.status().as_u16();
    let status_text = res.status().canonical_reason().unwrap_or("").to_string();
    let text = res.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(ApiError {
            status,
            status_text,
            body: text.chars().take(800).collect(),
        });
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Argument extraction helpers
// ---------------------------------------------------------------------------

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)?.as_str()
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key)?.as_i64()
}

fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key)?.as_bool()
}

fn arg_value<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    arg_str(args, key)
        .map(str::to_string)
        .ok_or_else(|| format!("missing or invalid argument: {key}"))
}

/// Build `k=v&…` from present string args in the given order.
fn query_from(args: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|k| arg_str(args, k).map(|v| format!("{k}={}", enc_query(v))))
        .collect::<Vec<_>>()
        .join("&")
}

fn truncate(body: &str, max: usize) -> String {
    if body.chars().count() > max {
        let cut: String = body.chars().take(max).collect();
        format!("{cut}\n…[truncated]")
    } else {
        body.to_string()
    }
}

/// Format a unix timestamp (seconds or milliseconds) as RFC3339 UTC.
fn fmt_mtime(raw: Option<&Value>) -> String {
    use chrono::TimeZone;
    let Some(v) = raw else { return "(unknown)".into() };
    if let Some(n) = v.as_f64() {
        if n.is_finite() && n > 0.0 {
            let secs = if n < 1e12 { n } else { n / 1000.0 };
            if let Some(dt) = chrono::Utc.timestamp_opt(secs as i64, ((secs.fract()) * 1e9) as u32).single() {
                return dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            }
        }
        return "(unknown)".into();
    }
    match v.as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "(unknown)".into(),
    }
}

fn str_or_unknown(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(other) => other.to_string(),
        None => "(unknown)".to_string(),
    }
}

/// Helper for OCC tools: resolve expected hash, either from args or fetched
/// from the current page.
async fn resolve_hash(client: &HttpClient, args: &Value) -> Result<String, String> {
    if let Some(h) = arg_str(args, "expected_hash") {
        if !h.is_empty() {
            return Ok(h.to_string());
        }
    }
    let path = required_str(args, "path")?;
    let current = api(
        client,
        reqwest::Method::GET,
        &format!("/v1/pages/{}", enc_path(&path)),
        Body::None,
    )
    .await
    .map_err(|e| e.to_string())?;
    match current.get("hash").and_then(Value::as_str) {
        Some(h) if !h.is_empty() => Ok(h.to_string()),
        _ => Err("could not determine current hash for the page; pass expected_hash explicitly.".to_string()),
    }
}

/// Uniform handling of HTTP 409 conflicts as readable non-error text.
fn conflict_text(err: ApiError, explanation: &[&str]) -> String {
    [
        explanation
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        String::new(),
        "Conflict payload:".to_string(),
        err.body,
    ]
    .join("\n")
}

macro_rules! handler {
    ($client:ident, $args:ident, $body:expr) => {
        move |$client: HttpClient, $args: Value| {
            Box::pin(async move { $body })
                as Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        }
    };
}

// ---------------------------------------------------------------------------
// Schema helpers — mirror the zod definitions as plain JSON objects
// ---------------------------------------------------------------------------

fn schema(properties: Value) -> Value {
    json!({ "type": "object", "properties": properties })
}

fn s(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn s_plain() -> Value {
    json!({ "type": "string" })
}

fn i(min: i64, max: i64, default: Value) -> Value {
    json!({ "type": "integer", "minimum": min, "maximum": max, "default": default })
}

fn b() -> Value {
    json!({ "type": "boolean" })
}

fn arr(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// All 41 tools in registration order matching the TS reference.
pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search",
            description: "Full-text search across the WikiLLM knowledge base. Returns matching chunks with their page path, heading, snippet, and content hash.",
            input_schema: schema(json!({
                "q": s("Query text"),
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
                "project": s_plain(),
            })),
            handler: handler!(client, args, {
                let q = required_str(&args, "q")?;
                let limit = arg_i64(&args, "limit").unwrap_or(10);
                let project = arg_str(&args, "project");
                let mut qs = format!("q={}&limit={limit}", enc_query(&q));
                if let Some(p) = project {
                    qs.push_str(&format!("&project={}", enc_query(p)));
                }
                let data = api(&client, reqwest::Method::GET, &format!("/v1/search?{qs}"), Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                let results: Vec<&Value> = match &data {
                    Value::Array(a) => a.iter().collect(),
                    Value::Object(o) => o
                        .get("results")
                        .and_then(Value::as_array)
                        .map(|r| r.iter().collect())
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                if results.is_empty() {
                    return Ok(format!("No results for {}.", serde_json::to_string(&q).unwrap_or_default()));
                }
                let lines: Vec<String> = results
                    .iter()
                    .map(|hit| {
                        let heading = hit.get("heading_path").and_then(Value::as_str).unwrap_or("");
                        let hash = hit
                            .get("hash")
                            .and_then(Value::as_str)
                            .filter(|h| !h.is_empty())
                            .unwrap_or("(no hash)");
                        format!(
                            "{} :: {}\n{}\n({hash})",
                            hit.get("rel_path").map(|v| v.to_string()).unwrap_or_else(||
                                "null".to_string()),
                            heading,
                            hit.get("snippet").and_then(Value::as_str).unwrap_or("")
                        )
                    })
                    .collect();
                Ok(lines.join("\n\n"))
            }),
        },
        Tool {
            name: "get_concept",
            description: "Fetch a wiki page by path: frontmatter summary plus markdown body (truncated to 4000 chars).",
            input_schema: schema(json!({
                "path": s("Page path, e.g. 'concepts/occ.md'"),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let page = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/pages/{}", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                let mut lines: Vec<String> = Vec::new();
                for key in ["rel_path", "title", "summary", "hash", "mtime", "word_count"] {
                    if let Some(v) = page.get(key).filter(|v| !v.is_null()) {
                        lines.push(format!("{key}: {v}"));
                    }
                }
                if let Some(links) = page.get("outgoing_links").and_then(Value::as_array) {
                    if !links.is_empty() {
                        let joined: Vec<String> = links.iter().map(|l| l.to_string()).collect();
                        lines.push(format!("links: {}", joined.join(", ")));
                    }
                }
                if let Some(fm) = page.get("frontmatter").filter(|v| !v.is_null()) {
                    if fm.as_object().is_some_and(|m| !m.is_empty()) {
                        lines.push(format!("\n--- frontmatter ---\n{}", pretty(fm)));
                    }
                }
                let raw_body = page
                    .get("body")
                    .or_else(|| page.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !raw_body.is_empty() {
                    let body = truncate(raw_body, 4000);
                    lines.push(format!("\n--- body ---\n{body}"));
                }
                Ok(lines.join("\n"))
            }),
        },
        Tool {
            name: "read_source",
            description: "Read source-document metadata (path, size, hash, content type, mtime) by path.",
            input_schema: schema(json!({
                "path": s("Source path within the wiki root"),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let meta = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/sources/{}", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                let mtime_raw = meta.get("mtime").or_else(|| meta.get("mtimeMs")).or_else(|| meta.get("modified_at"));
                Ok([
                    format!(
                        "path: {}",
                        meta.get("path")
                            .or_else(|| meta.get("rel_path"))
                            .and_then(Value::as_str)
                            .unwrap_or(&path)
                    ),
                    format!("size: {}", str_or_unknown(meta.get("size"))),
                    format!("hash: {}", str_or_unknown(meta.get("hash"))),
                    format!("content_type: {}", str_or_unknown(meta.get("content_type"))),
                    format!("mtime: {}", fmt_mtime(mtime_raw)),
                ]
                .join("\n"))
            }),
        },
        Tool {
            name: "list_changes",
            description: "List recent changes (writes, ingests, deletes) recorded by the WikiLLM API.",
            input_schema: schema(json!({
                "limit": i(1, 1000, json!(20)),
            })),
            handler: handler!(client, args, {
                let limit = arg_i64(&args, "limit").unwrap_or(20);
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/changes?limit={limit}"),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "graph_neighbors",
            description: "Traverse the wiki link graph around a page up to the given depth.",
            input_schema: schema(json!({
                "path": s("Page path to start from"),
                "depth": i(1, 5, json!(1)),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let depth = arg_i64(&args, "depth").unwrap_or(1);
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/graph/{}?depth={depth}", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "propose_edit",
            description: "Optimistically write a page. If expected_hash is omitted, the current page hash is fetched first; a stale hash yields a 409 conflict explaining OCC.",
            input_schema: schema(json!({
                "path": s("Page path to write"),
                "content": s("New markdown body"),
                "frontmatter": { "type": "object", "additionalProperties": true },
                "expected_hash": { "type": "string", "description": "Hash the edit is based on (OCC guard)" },
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let content = required_str(&args, "content")?;
                let hash = resolve_hash(&client, &args).await?;
                let mut body = Map::new();
                body.insert("content".into(), Value::String(content));
                if let Some(fm) = arg_value(&args, "frontmatter") {
                    body.insert("frontmatter".into(), fm.clone());
                }
                body.insert("ifMatch".into(), Value::String(hash));
                match api(
                    &client,
                    reqwest::Method::PUT,
                    &format!("/v1/pages/{}", enc_path(&path)),
                    Body::Json(Value::Object(body)),
                )
                .await
                {
                    Ok(result) => Ok(pretty(&result)),
                    Err(e) if e.status == 409 => Ok(conflict_text(
                        e,
                        &[
                            "Conflict (HTTP 409): the page changed since your expected_hash was taken.",
                            "This is optimistic concurrency control (OCC): re-read the page, merge your",
                            "changes with the newer content, and retry with a fresh expected_hash.",
                        ],
                    )),
                    Err(e) => Err(e.to_string()),
                }
            }),
        },
        Tool {
            name: "append_log",
            description: "Append an entry to the knowledge-base log/journal.",
            input_schema: schema(json!({
                "message": s("Log message"),
                "kind": s_plain(),
            })),
            handler: handler!(client, args, {
                let message = required_str(&args, "message")?;
                let kind = arg_str(&args, "kind");
                let mut body = Map::new();
                body.insert("message".into(), Value::String(message));
                if let Some(k) = kind {
                    body.insert("kind".into(), Value::String(k.to_string()));
                }
                let data = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/log/append",
                    Body::Json(Value::Object(body)),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "query",
            description: "Ask a natural-language question against the knowledge base (LLM-backed; returns llm_not_configured errors verbatim if no LLM provider is set up).",
            input_schema: schema(json!({
                "question": s("Question to answer from the knowledge base"),
            })),
            handler: handler!(client, args, {
                let question = required_str(&args, "question")?;
                let data = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/query",
                    Body::Json(json!({ "question": question })),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "refresh_index",
            description: "Trigger a re-index of the knowledge base.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::POST, "/v1/index/refresh", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "settings_list",
            description: "List all WikiLLM settings with type, default, current value, and override state. Secret values are masked.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::GET, "/v1/settings", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                let empty = Vec::new();
                let settings = data
                    .get("settings")
                    .and_then(Value::as_array)
                    .unwrap_or(&empty);
                if settings.is_empty() {
                    return Ok("No settings found.".to_string());
                }
                let lines: Vec<String> = settings
                    .iter()
                    .map(|st| {
                        let key = st.get("key").map(|v| v.to_string()).unwrap_or_else(|| "(unknown)".into());
                        let typ = st.get("type").and_then(Value::as_str).unwrap_or("?");
                        let value = st.get("value");
                        let masked = matches!(
                            value,
                            None | Some(Value::Null)
                        ) || value
                            .and_then(Value::as_str)
                            .is_some_and(|sv| !sv.is_empty() && sv.chars().all(|c| c == '*'));
                        let value_s = if masked {
                            "<masked>".to_string()
                        } else {
                            value.map(pretty).unwrap_or_else(|| "null".into())
                        };
                        let def = st.get("default").map(pretty).unwrap_or_else(|| "null".into());
                        let overridden = if st.get("overridden").and_then(Value::as_bool).unwrap_or(false) {
                            ", overridden"
                        } else {
                            ""
                        };
                        format!("{key} ({typ}, default={def}, value={value_s}{overridden})")
                    })
                    .collect();
                Ok(lines.join("\n"))
            }),
        },
        Tool {
            name: "settings_get",
            description: "Fetch a single setting view by key.",
            input_schema: schema(json!({
                "key": s("Setting key"),
            })),
            handler: handler!(client, args, {
                let key = required_str(&args, "key")?;
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/settings/{}", enc_query(&key)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "settings_set",
            description: "Set a setting value. Value may be a string, number, boolean, array, or object; it is sent as-is. Notes when a reindex is required.",
            input_schema: schema(json!({
                "key": s("Setting key"),
                "value": {
                    "description": "New value",
                    "anyOf": [
                        { "type": "string" },
                        { "type": "number" },
                        { "type": "boolean" },
                        { "type": "array" },
                        { "type": "object" },
                    ],
                },
            })),
            handler: handler!(client, args, {
                let key = required_str(&args, "key")?;
                let value = arg_value(&args, "value").cloned().ok_or("missing argument: value")?;
                let result = api(
                    &client,
                    reqwest::Method::PUT,
                    &format!("/v1/settings/{}", enc_query(&key)),
                    Body::Json(json!({ "value": value })),
                )
                .await
                .map_err(|e| e.to_string())?;
                if result.get("reindex_required").and_then(Value::as_bool) == Some(true) {
                    return Ok(format!("Set {key}.\nNOTE: reindex required for this change to take effect — call admin_reindex."));
                }
                Ok(format!("Set {key}.\n{}", pretty(&result)))
            }),
        },
        Tool {
            name: "settings_reset",
            description: "Reset a setting back to its env/default value.",
            input_schema: schema(json!({
                "key": s("Setting key"),
            })),
            handler: handler!(client, args, {
                let key = required_str(&args, "key")?;
                let result = api(
                    &client,
                    reqwest::Method::DELETE,
                    &format!("/v1/settings/{}", enc_query(&key)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(format!("Reset {key}.\n{}", pretty(&result)))
            }),
        },
        Tool {
            name: "keys_list",
            description: "List API keys (prefix only — plaintext keys are never returned after creation).",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::GET, "/v1/keys", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                let empty = Vec::new();
                let keys = data.get("keys").and_then(Value::as_array).unwrap_or(&empty);
                if keys.is_empty() {
                    return Ok("No API keys.".to_string());
                }
                let lines: Vec<String> = keys
                    .iter()
                    .map(|k| {
                        let mut parts = vec![
                            k.get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("(unnamed)")
                                .to_string(),
                            k.get("key_prefix").and_then(Value::as_str).unwrap_or("").to_string(),
                            format!("role={}", k.get("role").and_then(Value::as_str).unwrap_or("?")),
                            format!("scope={}", k.get("scope").map(pretty).unwrap_or_else(|| "null".into())),
                        ];
                        if let Some(created) = k.get("created_at") {
                            parts.push(format!("created={created}"));
                        }
                        if let Some(by) = k.get("created_by") {
                            parts.push(format!("by={by}"));
                        }
                        parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
                    })
                    .collect();
                Ok(lines.join("\n"))
            }),
        },
        Tool {
            name: "key_create",
            description: "Create an API key. The plaintext key is shown ONCE in the output — store it immediately.",
            input_schema: schema(json!({
                "name": s_plain(),
                "role": { "type": "string", "enum": ["admin", "write", "read"], "default": "write" },
                "scope": { "type": "array", "items": { "type": "string" }, "default": ["*"] },
            })),
            handler: handler!(client, args, {
                let mut body = Map::new();
                if let Some(name) = arg_str(&args, "name") {
                    body.insert("name".into(), Value::String(name.into()));
                }
                if let Some(role) = arg_str(&args, "role") {
                    body.insert("role".into(), Value::String(role.into()));
                }
                if let Some(scope) = arg_value(&args, "scope") {
                    body.insert("scope".into(), scope.clone());
                }
                let result = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/keys",
                    Body::Json(Value::Object(body)),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok([
                    "WARNING: this plaintext key is shown ONLY once — save it now.".to_string(),
                    format!("key: {}", str_or_unknown(result.get("key"))),
                    format!("prefix: {}", str_or_unknown(result.get("key_prefix"))),
                    format!("role: {}", str_or_unknown(result.get("role"))),
                    format!("scope: {}", result.get("scope").map(pretty).unwrap_or_else(|| "null".into())),
                ]
                .join("\n"))
            }),
        },
        Tool {
            name: "key_delete",
            description: "Delete an API key by name.",
            input_schema: schema(json!({
                "name": s("Key name"),
            })),
            handler: handler!(client, args, {
                let name = required_str(&args, "name")?;
                let data = api(
                    &client,
                    reqwest::Method::DELETE,
                    &format!("/v1/keys/{}", enc_query(&name)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "projects_list",
            description: "List configured wiki projects.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::GET, "/v1/projects", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "project_put",
            description: "Create or update a project (path prefixes are required).",
            input_schema: schema(json!({
                "name": s("Project name"),
                "prefixes": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Path prefixes served by this project" },
                "description": s_plain(),
                "connectors": arr(s_plain()),
            })),
            handler: handler!(client, args, {
                let name = required_str(&args, "name")?;
                let prefixes = arg_value(&args, "prefixes")
                    .and_then(Value::as_array)
                    .ok_or("missing argument: prefixes")?
                    .clone();
                let mut body = Map::new();
                body.insert("prefixes".into(), Value::Array(prefixes));
                if let Some(d) = arg_str(&args, "description") {
                    body.insert("description".into(), Value::String(d.into()));
                }
                if let Some(c) = arg_value(&args, "connectors") {
                    body.insert("connectors".into(), c.clone());
                }
                let data = api(
                    &client,
                    reqwest::Method::PUT,
                    &format!("/v1/projects/{}", enc_query(&name)),
                    Body::Json(Value::Object(body)),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "project_delete",
            description: "Delete a project by name.",
            input_schema: schema(json!({
                "name": s("Project name"),
            })),
            handler: handler!(client, args, {
                let name = required_str(&args, "name")?;
                let data = api(
                    &client,
                    reqwest::Method::DELETE,
                    &format!("/v1/projects/{}", enc_query(&name)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "connectors_list",
            description: "List configured connectors (admin).",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::GET, "/v1/connectors", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "connector_create",
            description: "Create a connector of kind git, web, or github.",
            input_schema: schema(json!({
                "kind": { "type": "string", "enum": ["git", "web", "github"] },
                "config": { "type": "object", "additionalProperties": true, "description": "Connector-specific configuration" },
                "id": s_plain(),
                "enabled": b(),
            })),
            handler: handler!(client, args, {
                let kind = required_str(&args, "kind")?;
                let config = arg_value(&args, "config").cloned().ok_or("missing argument: config")?;
                let mut body = Map::new();
                body.insert("kind".into(), Value::String(kind));
                body.insert("config".into(), config);
                if let Some(id) = arg_str(&args, "id") {
                    body.insert("id".into(), Value::String(id.into()));
                }
                if let Some(enabled) = arg_bool(&args, "enabled") {
                    body.insert("enabled".into(), Value::Bool(enabled));
                }
                let data = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/connectors",
                    Body::Json(Value::Object(body)),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "connector_delete",
            description: "Delete a connector by id.",
            input_schema: schema(json!({
                "id": s("Connector id"),
            })),
            handler: handler!(client, args, {
                let id = required_str(&args, "id")?;
                let data = api(
                    &client,
                    reqwest::Method::DELETE,
                    &format!("/v1/connectors/{}", enc_query(&id)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "connector_run",
            description: "Trigger a connector run and report how many documents were ingested.",
            input_schema: schema(json!({
                "id": s("Connector id"),
            })),
            handler: handler!(client, args, {
                let id = required_str(&args, "id")?;
                let result = api(
                    &client,
                    reqwest::Method::POST,
                    &format!("/v1/connectors/{}/run", enc_query(&id)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                if let Some(docs) = result.get("docs").and_then(Value::as_array) {
                    return Ok(format!(
                        "Ingested {} document(s).\n{}",
                        docs.len(),
                        pretty(&result)
                    ));
                }
                Ok(pretty(&result))
            }),
        },
        Tool {
            name: "admin_reindex",
            description: "Trigger a full administrative reindex.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::POST, "/v1/admin/reindex", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "admin_stats",
            description: "Get an overview of index/document counts and other admin stats.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let stats = api(&client, reqwest::Method::GET, "/v1/admin/stats", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut lines: Vec<String> = Vec::new();
                if let Some(obj) = stats.as_object() {
                    for (key, value) in obj {
                        match value {
                            Value::Object(inner) => {
                                for (k, v) in inner {
                                    lines.push(format!("{key}.{k}: {v}"));
                                }
                            }
                            Value::Array(a) => lines.push(format!("{key}: {}", a.len())),
                            other => lines.push(format!("{key}: {other}")),
                        }
                    }
                }
                if lines.is_empty() {
                    Ok(pretty(&stats))
                } else {
                    Ok(lines.join("\n"))
                }
            }),
        },
        Tool {
            name: "okf_validate",
            description: "Validate the knowledge base against the OKF bundle spec; reports validity, errors, warnings, and stats.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let report = api(&client, reqwest::Method::POST, "/v1/okf/validate", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                let valid = if report.get("valid").and_then(Value::as_bool) == Some(true) {
                    "valid"
                } else {
                    "INVALID"
                };
                let errors = report
                    .get("errors")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                let warnings = report
                    .get("warnings")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                Ok(format!(
                    "bundle {valid} ({errors} error(s), {warnings} warning(s))\n{}",
                    pretty(&report)
                ))
            }),
        },
        Tool {
            name: "delete_page",
            description: "Delete a page. Like propose_edit, the current hash is fetched first unless expected_hash is given and sent via If-Match.",
            input_schema: schema(json!({
                "path": s("Page path to delete"),
                "expected_hash": { "type": "string", "description": "Hash the delete is based on (OCC guard)" },
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let hash = resolve_hash(&client, &args).await?;
                let mut req = client
                    .http
                    .request(reqwest::Method::DELETE, client.url(&format!("/v1/pages/{}", enc_path(&path))));
                if !client.api_key.is_empty() {
                    req = req.bearer_auth(&client.api_key);
                }
                match finish(req.header("If-Match", &hash)).await {
                    Ok(result) => Ok(pretty(&result)),
                    Err(e) if e.status == 409 => Ok(conflict_text(
                        e,
                        &[
                            "Conflict (HTTP 409): the page changed since your expected_hash was taken.",
                            "Re-read the page and retry with a fresh expected_hash.",
                        ],
                    )),
                    Err(e) => Err(e.to_string()),
                }
            }),
        },
        Tool {
            name: "put_source",
            description: "Create or overwrite a raw source document. Set force to bypass conflict checks.",
            input_schema: schema(json!({
                "path": s("Source path within the wiki root"),
                "content": s("Raw source content"),
                "force": b(),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let content = required_str(&args, "content")?;
                let suffix = if arg_bool(&args, "force").unwrap_or(false) {
                    "?force=true"
                } else {
                    ""
                };
                let data = api(
                    &client,
                    reqwest::Method::POST,
                    &format!("/v1/sources/{}{suffix}", enc_path(&path)),
                    Body::Json(json!({ "content": content })),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "add_feedback",
            description: "Submit feedback on a query result.",
            input_schema: schema(json!({
                "query_id": s("Query id the feedback refers to"),
                "helpful": b(),
                "comment": s_plain(),
            })),
            handler: handler!(client, args, {
                let query_id = required_str(&args, "query_id")?;
                let helpful = arg_bool(&args, "helpful").ok_or("missing or invalid argument: helpful")?;
                let mut body = Map::new();
                body.insert("query_id".into(), Value::String(query_id));
                body.insert("helpful".into(), Value::Bool(helpful));
                if let Some(comment) = arg_str(&args, "comment") {
                    body.insert("comment".into(), Value::String(comment.into()));
                }
                let data = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/feedback",
                    Body::Json(Value::Object(body)),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "documents_list",
            description: "List documents in the knowledge base with kind, origin, type, tags, status, trust, and staleness metadata.",
            input_schema: schema(json!({
                "kind": s_plain(),
                "origin": s_plain(),
                "folder": s_plain(),
                "type": s_plain(),
                "tags": { "type": "string", "description": "Comma-separated tag filter, e.g. 'a,b'" },
                "status": s_plain(),
                "trust": { "type": "string", "enum": ["low", "medium", "high"] },
                "fresh": b(),
                "project": s_plain(),
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 },
                "cursor": s_plain(),
            })),
            handler: handler!(client, args, {
                let mut parts: Vec<String> = query_from(&args, &["kind", "origin", "folder", "type", "tags", "status", "project", "cursor"])
                    .split('&')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect();
                if let Some(t) = arg_str(&args, "trust") {
                    parts.push(format!("trust={}", enc_query(t)));
                }
                if arg_bool(&args, "fresh") == Some(true) {
                    parts.push("fresh=true".into());
                }
                if let Some(limit) = arg_i64(&args, "limit") {
                    parts.push(format!("limit={limit}"));
                }
                let qs = parts.join("&");
                let path = if qs.is_empty() {
                    "/v1/documents".to_string()
                } else {
                    format!("/v1/documents?{qs}")
                };
                let data = api(&client, reqwest::Method::GET, &path, Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                let empty = Vec::new();
                let items = data.get("items").and_then(Value::as_array).unwrap_or(&empty);
                if items.is_empty() {
                    return Ok("No documents match the given filters.".to_string());
                }
                let mut lines: Vec<String> = items
                    .iter()
                    .map(|doc| {
                        let rel_path = doc.get("rel_path").and_then(Value::as_str).unwrap_or("(unknown)");
                        let kind = doc.get("kind").map(|v| v.to_string()).unwrap_or_else(|| "?".into());
                        let title = doc.get("title").and_then(Value::as_str).unwrap_or("");
                        let okf_type = doc
                            .get("okf_type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let tags = doc
                            .get("tags")
                            .and_then(Value::as_array)
                            .map(|t| t.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","))
                            .unwrap_or_default();
                        let meta = [okf_type, tags]
                            .into_iter()
                            .filter(|p| !p.is_empty())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let meta = if meta.is_empty() { "-".to_string() } else { meta };
                        format!("{rel_path} [{kind}] {title} ({meta})")
                    })
                    .collect();
                if let Some(next) = data.get("nextCursor").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    lines.push(format!("(more results; nextCursor: {next})"));
                }
                Ok(lines.join("\n"))
            }),
        },
        Tool {
            name: "download_document",
            description: "Fetch a document's content by path as text (page, source, or connector document; truncated to 4000 chars).",
            input_schema: schema(json!({
                "path": s("Document path relative to the wiki root"),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/documents/{}/content", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                let body = match data {
                    Value::String(s) => s,
                    other => pretty(&other),
                };
                Ok(truncate(&body, 4000))
            }),
        },
        Tool {
            name: "pages_batch",
            description: "Apply a batch of page writes/deletes atomically-preflighted by the API: create, update (with optional frontmatter and If-Match OCC guard), or delete up to 100 pages in one call.",
            input_schema: schema(json!({
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "description": "Batch operations, applied in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "rel_path": { "type": "string", "description": "Page path relative to the wiki root" },
                            "content": { "type": "string", "description": "Markdown body (omit for delete)" },
                            "frontmatter": { "type": "object", "additionalProperties": true },
                            "ifMatch": { "type": "string", "description": "Expected hash (If-Match) for optimistic concurrency" },
                            "delete": { "type": "boolean" },
                        },
                        "required": ["rel_path"],
                    },
                },
            })),
            handler: handler!(client, args, {
                let operations = arg_value(&args, "operations").cloned().ok_or("missing argument: operations")?;
                match api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/pages/batch",
                    Body::Json(json!({ "operations": operations })),
                )
                .await
                {
                    Ok(result) => {
                        let success = result
                            .get("success")
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "true".into());
                        let mut lines = vec![format!("batch success={success}")];
                        if let Some(results) = result.get("results").and_then(Value::as_array) {
                            for (idx, op) in results.iter().enumerate() {
                                lines.push(format!("#{} {}", idx + 1, op));
                            }
                        }
                        Ok(lines.join("\n"))
                    }
                    Err(e) if e.status == 409 => Ok(conflict_text(
                        e,
                        &[
                            "Conflict preflight failed (HTTP 409): one or more operations hit a stale If-Match hash.",
                        ],
                    )),
                    Err(e) => Err(e.to_string()),
                }
            }),
        },
        Tool {
            name: "documents_delete",
            description: "Delete multiple documents by path; reports per-path results.",
            input_schema: schema(json!({
                "rel_paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Document paths relative to the wiki root" },
            })),
            handler: handler!(client, args, {
                let rel_paths = arg_value(&args, "rel_paths").cloned().ok_or("missing argument: rel_paths")?;
                let result = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/documents/delete",
                    Body::Json(json!({ "rel_paths": rel_paths })),
                )
                .await
                .map_err(|e| e.to_string())?;
                let empty = Vec::new();
                let results = result.get("results").and_then(Value::as_array).unwrap_or(&empty);
                if results.is_empty() {
                    return Ok(pretty(&result));
                }
                let lines: Vec<String> = results
                    .iter()
                    .map(|row| {
                        let rel_path = row.get("rel_path").and_then(Value::as_str).unwrap_or("(unknown)");
                        if row.get("success").and_then(Value::as_bool) == Some(true) {
                            format!("{rel_path}: deleted")
                        } else {
                            match row.get("error") {
                                Some(err) => format!("{rel_path}: FAILED — {err}"),
                                None => format!("{rel_path}: FAILED"),
                            }
                        }
                    })
                    .collect();
                Ok(lines.join("\n"))
            }),
        },
        Tool {
            name: "export_bundle",
            description: "Export documents as a tar.gz bundle filtered by prefix/kind/origin/since/project. Reports byte size and exported file count; does not dump the archive contents.",
            input_schema: schema(json!({
                "prefix": s_plain(),
                "kind": s_plain(),
                "origin": s_plain(),
                "since": { "type": "string", "description": "Only include documents modified since this timestamp" },
                "project": s_plain(),
            })),
            handler: handler!(client, args, {
                let qs = query_from(&args, &["prefix", "kind", "origin", "since", "project"]);
                let path = if qs.is_empty() {
                    "/v1/bundle/export".to_string()
                } else {
                    format!("/v1/bundle/export?{qs}")
                };
                let mut req = client.http.get(client.url(&path));
                if !client.api_key.is_empty() {
                    req = req.bearer_auth(&client.api_key);
                }
                let res = req.send().await.map_err(|e| e.to_string())?;
                let status = res.status().as_u16();
                if !(200..300).contains(&status) {
                    let reason = res.status().canonical_reason().unwrap_or("").to_string();
                    let body = res.text().await.unwrap_or_default();
                    let cut: String = body.chars().take(800).collect();
                    return Err(ApiError { status, status_text: reason, body: cut }.to_string());
                }
                let files = res
                    .headers()
                    .get("X-Exported-Files")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("(unknown)")
                    .to_string();
                let bytes = res.bytes().await.map_err(|e| e.to_string())?;
                Ok(format!(
                    "bundle exported: {} bytes\nX-Exported-Files: {files}",
                    bytes.len()
                ))
            }),
        },
        Tool {
            name: "graph_export",
            description: "Export the link graph around a page as DOT (default) or compact JSON neighbor lines.",
            input_schema: schema(json!({
                "path": s("Page path to root the graph at"),
                "depth": { "type": "integer", "minimum": 1 },
                "format": { "type": "string", "enum": ["json", "dot"], "default": "dot" },
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let format = arg_str(&args, "format").unwrap_or("dot").to_string();
                let mut qs = format!("format={}", enc_query(&format));
                if let Some(depth) = arg_i64(&args, "depth") {
                    qs.push_str(&format!("&depth={depth}"));
                }
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/graph/{}?{qs}", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                if format == "dot" {
                    return Ok(match data {
                        Value::String(s) => s,
                        other => pretty(&other),
                    });
                }
                let nodes_len = data.get("nodes").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
                let edges = data.get("edges").and_then(Value::as_array);
                let mut lines = vec![format!("nodes: {nodes_len}, edges: {}", edges.map(|e| e.len()).unwrap_or(0))];
                if let Some(edges) = edges {
                    for edge in edges {
                        let src = edge.get("source").cloned().unwrap_or(Value::String(String::new()));
                        let tgt = edge.get("target").cloned().unwrap_or(Value::String(String::new()));
                        lines.push(format!("{} -> {}", src, tgt));
                    }
                }
                Ok(lines.join("\n"))
            }),
        },
        Tool {
            name: "webhooks_list",
            description: "List registered webhooks (admin).",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::GET, "/v1/webhooks", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "webhook_create",
            description: "Create a webhook that fires on knowledge-base change events under the given path prefixes.",
            input_schema: schema(json!({
                "url": { "type": "string", "format": "uri", "description": "Webhook target URL" },
                "prefixes": { "type": "array", "items": { "type": "string" }, "default": ["*"], "description": "Path prefixes the webhook applies to (default ['*'])" },
                "enabled": b(),
            })),
            handler: handler!(client, args, {
                let url = required_str(&args, "url")?;
                let prefixes = arg_value(&args, "prefixes")
                    .cloned()
                    .unwrap_or_else(|| json!(["*"]));
                let mut body = Map::new();
                body.insert("url".into(), Value::String(url));
                body.insert("events".into(), json!(["change"]));
                body.insert("prefixes".into(), prefixes);
                if let Some(enabled) = arg_bool(&args, "enabled") {
                    body.insert("enabled".into(), Value::Bool(enabled));
                }
                let data = api(
                    &client,
                    reqwest::Method::POST,
                    "/v1/webhooks",
                    Body::Json(Value::Object(body)),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "webhook_delete",
            description: "Delete a webhook by id (admin).",
            input_schema: schema(json!({
                "id": s("Webhook id"),
            })),
            handler: handler!(client, args, {
                let id = required_str(&args, "id")?;
                let data = api(
                    &client,
                    reqwest::Method::DELETE,
                    &format!("/v1/webhooks/{}", enc_query(&id)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
        Tool {
            name: "get_page_raw",
            description: "Fetch a page's raw markdown body without frontmatter processing (truncated to 6000 chars).",
            input_schema: schema(json!({
                "path": s("Page path, e.g. 'concepts/occ.md'"),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/pages/{}/raw", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                let body = match data {
                    Value::String(s) => s,
                    other => pretty(&other),
                };
                Ok(truncate(&body, 6000))
            }),
        },
        Tool {
            name: "read_source_content",
            description: "Fetch a source document's original bytes decoded as utf8 text (truncated to 4000 chars).",
            input_schema: schema(json!({
                "path": s("Source path within the wiki root"),
            })),
            handler: handler!(client, args, {
                let path = required_str(&args, "path")?;
                let data = api(
                    &client,
                    reqwest::Method::GET,
                    &format!("/v1/sources/{}/content", enc_path(&path)),
                    Body::None,
                )
                .await
                .map_err(|e| e.to_string())?;
                let body = match data {
                    Value::String(s) => s,
                    other => pretty(&other),
                };
                Ok(truncate(&body, 4000))
            }),
        },
        Tool {
            name: "okf_layout",
            description: "Fetch the OKF layout specification served by the API.",
            input_schema: schema(json!({})),
            handler: handler!(client, _args, {
                let data = api(&client, reqwest::Method::GET, "/v1/okf/layout", Body::None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pretty(&data))
            }),
        },
    ]
}

/// Dispatch a `tools/call`. Never fails: transport-level failures surface as
/// `is_error: true` output, mirroring the TS `run()` wrapper.
pub async fn call_tool(client: &HttpClient, name: &str, args: Value) -> ToolOutput {
    let tool = match tools().into_iter().find(|t| t.name == name) {
        Some(t) => t,
        None => {
            return ToolOutput::err(format!("Unknown tool: {name}"));
        }
    };
    match (tool.handler)(client.clone(), args).await {
        Ok(text) => ToolOutput::ok(text),
        Err(message) => ToolOutput::err(message),
    }
}
