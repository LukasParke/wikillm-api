//! Shared domain types. Field names intentionally mirror the TypeScript
//! service's JSON contract (snake_case) so clients work against either.

use serde::{Deserialize, Serialize};

pub type Source = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DocKind {
    #[default]
    Page,
    Source,
    Doc,
}

impl DocKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocKind::Page => "page",
            DocKind::Source => "source",
            DocKind::Doc => "doc",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "page" => Some(DocKind::Page),
            "source" => Some(DocKind::Source),
            "doc" => Some(DocKind::Doc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedEntry {
    pub by: String,
    #[serde(default)]
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRecord {
    pub id: String,
    pub rel_path: String,
    pub kind: DocKind,
    pub origin: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default = "default_object")]
    pub frontmatter: serde_json::Value,
    #[serde(default)]
    pub word_count: i64,
    #[serde(default)]
    pub outgoing_links: Vec<String>,
    pub hash: String,
    pub mtime: i64,
    #[serde(default)]
    pub content_type: Option<String>,
    pub okf_type: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub stale_after: Option<String>,
    pub resource: Option<String>,
    pub generated_by: Option<String>,
    pub generated_at: Option<String>,
    #[serde(default)]
    pub verified: Option<Vec<VerifiedEntry>>,
    #[serde(default)]
    pub provenance: Option<Vec<serde_json::Value>>,
    pub updated_at: Option<String>,
    pub updated_by: Option<String>,
    pub indexed_at: String,
}

fn default_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Default)]
pub struct DocumentInput {
    pub rel_path: String,
    pub kind: DocKind,
    pub origin: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub body: String,
    pub frontmatter: serde_json::Value,
    pub word_count: i64,
    pub outgoing_links: Vec<String>,
    pub hash: String,
    pub mtime: i64,
    pub content_type: Option<String>,
    pub okf_type: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub stale_after: Option<String>,
    pub resource: Option<String>,
    pub generated_by: Option<String>,
    pub generated_at: Option<String>,
    pub verified: Option<Vec<VerifiedEntry>>,
    pub provenance: Option<Vec<serde_json::Value>>,
    pub updated_at: Option<String>,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Distilled {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_refs: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkRecord {
    pub id: String,
    pub document_id: String,
    pub ordinal: i64,
    pub heading_path: Option<String>,
    pub content: String,
    pub distilled: Option<Distilled>,
    pub embedded_at: Option<String>,
    pub embed_model: Option<String>,
    /// joined from documents for queue processing
    #[serde(default)]
    pub rel_path: String,
}

#[derive(Debug, Clone)]
pub struct ChunkInput {
    pub ordinal: i64,
    pub heading_path: Option<String>,
    pub content: String,
    pub distilled: Option<Distilled>,
}

/// A scored chunk hit returned by retrieval primitives.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkHit {
    pub chunk_id: String,
    pub document_id: String,
    pub rel_path: String,
    pub kind: String,
    pub origin: String,
    pub title: Option<String>,
    pub okf_type: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub stale_after: Option<String>,
    pub verified: Option<Vec<VerifiedEntry>>,
    pub hash: String,
    pub mtime: i64,
    pub heading_path: Option<String>,
    pub content: String,
    pub score: f64,
}

/// Filters shared by retrieval and listing. `None` fields are ignored;
/// `path_prefixes: ["*"]` disables prefix scoping.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchFilters {
    #[serde(default, rename = "kinds")]
    pub kinds: Option<Vec<String>>,
    pub origins: Option<Vec<String>>,
    pub okf_types: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub statuses: Option<Vec<String>>,
    pub trust_min: Option<String>,
    pub fresh_only: Option<bool>,
    pub path_prefixes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub folder: Option<String>,
    pub kind: Option<DocKind>,
    pub origin: Option<String>,
    pub limit: i64,
    pub cursor: Option<String>,
    pub filters: Option<SearchFilters>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageList<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub created_at: String,
    pub source: Source,
    pub action: String,
    pub paths: Vec<String>,
    pub metadata: Option<serde_json::Value>,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeEventData {
    pub id: String,
    pub rel_path: String,
    pub change_type: String,
    pub old_hash: Option<String>,
    pub new_hash: Option<String>,
    pub source: Option<String>,
    pub operation_id: Option<String>,
    pub detected_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: ChangeEventData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    pub id: String,
    pub kind: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub name: String,
    pub description: Option<String>,
    pub prefixes: Vec<String>,
    pub connectors: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectInput {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyRecord {
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    pub scope: Vec<String>,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: String,
}

#[derive(Debug, Clone)]
pub struct ApiKeyUpsert {
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    pub scope: Vec<String>,
    pub role: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookRecord {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    pub prefixes: Vec<String>,
    pub enabled: bool,
    pub last_status: Option<String>,
    pub last_attempt_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryRecord {
    pub id: String,
    pub created_at: String,
    pub query: String,
    pub mode: String,
    pub project: Option<String>,
    pub latency_ms: f64,
    pub result_count: i64,
    pub zero_hit: bool,
    pub top_paths: Vec<String>,
    pub source: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackInput {
    pub query_id: String,
    pub helpful: bool,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsOverview {
    pub documents: i64,
    pub chunks: i64,
    pub embedded_chunks: i64,
    pub queries: i64,
    pub zero_hit_queries: i64,
    pub feedback_helpful: i64,
    pub feedback_total: i64,
}

pub const TRUST_ORDER: [(&str, u8); 3] = [
    ("unverified", 0),
    ("machine-confirmed", 1),
    ("human-reviewed", 2),
];

pub fn trust_tier(verified: Option<&Vec<VerifiedEntry>>) -> &'static str {
    match verified {
        None => "unverified",
        Some(v) if v.is_empty() => "unverified",
        Some(v) => {
            if v.iter().any(|e| e.by.starts_with("human:")) {
                "human-reviewed"
            } else {
                "machine-confirmed"
            }
        }
    }
}
