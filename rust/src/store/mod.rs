//! Persistence contract. Two implementations: `sqlite::SqliteStore`
//! (embedded, FTS5) and `pg::PostgresStore` (Postgres + pgvector).

use crate::domain::*;
use async_trait::async_trait;
use serde_json::Value;

use crate::error::Result;

#[async_trait]
pub trait Store: Send + Sync {
    fn backend(&self) -> &'static str;

    async fn migrate(&self) -> Result<()>;
    async fn close(&self) -> Result<()>;

    // Documents
    async fn upsert_document(&self, doc: &DocumentInput) -> Result<()>;
    async fn get_document(&self, rel_path: &str) -> Result<Option<DocumentRecord>>;
    async fn delete_document(&self, rel_path: &str) -> Result<()>;
    async fn list_documents(
        &self,
        opts: &ListOptions,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<PageList<DocumentRecord>>;
    async fn count_documents(&self, origin: Option<&str>) -> Result<i64>;

    // Chunks
    async fn replace_chunks(&self, document_id: &str, chunks: &[ChunkInput]) -> Result<()>;
    async fn get_chunks_for_document(&self, document_id: &str) -> Result<Vec<ChunkRecord>>;
    async fn get_chunk(&self, chunk_id: &str) -> Result<Option<ChunkRecord>>;
    async fn upsert_embeddings(
        &self,
        items: &[(String, Vec<f32>)],
        model: &str,
        embedded_at: &str,
    ) -> Result<()>;
    async fn list_unembedded_chunks(
        &self,
        limit: i64,
    ) -> Result<Vec<ChunkRecord>>;
    /// Drop all embedding vectors and clear embedded flags; on Postgres also
    /// resize the vector column when `dims` is provided.
    async fn reset_embeddings(&self, dims: Option<i32>) -> Result<()>;
    /// Persist distillation output for a single chunk.
    async fn replace_chunk_distilled(
        &self,
        chunk_id: &str,
        distilled: Option<crate::domain::Distilled>,
    ) -> Result<()>;

    // Link graph
    async fn replace_edges(&self, src_rel_path: &str, dst_rel_paths: &[String]) -> Result<()>;
    async fn backlinks(&self, rel_path: &str, limit: i64) -> Result<Vec<String>>;

    // Retrieval primitives
    async fn search_fts(
        &self,
        q: &str,
        limit: i64,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<ChunkHit>>;
    async fn search_vector(
        &self,
        vector: &[f32],
        limit: i64,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<ChunkHit>>;
    fn supports_vector(&self) -> bool;

    // Operations ledger
    async fn insert_operation(&self, op: &Operation) -> Result<()>;
    async fn get_operation(&self, id: &str) -> Result<Option<Operation>>;

    // Changes ledger
    async fn insert_change(&self, change: &ChangeEventData) -> Result<()>;
    async fn list_changes(
        &self,
        since: Option<&str>,
        path: Option<&str>,
        source: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ChangeEventData>>;

    // Connectors
    async fn put_connector(&self, c: &ConnectorConfig) -> Result<()>;
    async fn get_connector(&self, id: &str) -> Result<Option<ConnectorConfig>>;
    async fn list_connectors(&self) -> Result<Vec<ConnectorConfig>>;
    async fn delete_connector(&self, id: &str) -> Result<bool>;
    async fn get_connector_state(&self, id: &str) -> Result<Option<Value>>;
    async fn set_connector_state(&self, id: &str, watermark: &Value) -> Result<()>;

    // Projects
    async fn put_project(&self, p: &ProjectInput) -> Result<()>;
    async fn get_project(&self, name: &str) -> Result<Option<ProjectRecord>>;
    async fn list_projects(&self) -> Result<Vec<ProjectRecord>>;
    async fn delete_project(&self, name: &str) -> Result<bool>;

    // API keys (hashed)
    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>>;
    async fn get_api_key(&self, name: &str) -> Result<Option<ApiKeyRecord>>;
    async fn find_api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKeyRecord>>;
    async fn upsert_api_key(&self, input: &ApiKeyUpsert) -> Result<()>;
    async fn delete_api_key(&self, name: &str) -> Result<bool>;
    async fn count_api_keys(&self) -> Result<i64>;

    // Webhooks
    async fn list_webhooks(&self) -> Result<Vec<WebhookRecord>>;
    async fn get_webhook(&self, id: &str) -> Result<Option<WebhookRecord>>;
    async fn put_webhook(&self, w: &WebhookRecord) -> Result<()>;
    async fn delete_webhook(&self, id: &str) -> Result<bool>;
    async fn record_webhook_attempt(&self, id: &str, status: &str) -> Result<()>;

    // Analytics
    async fn record_query(&self, q: &QueryRecord) -> Result<()>;
    async fn record_feedback(
        &self,
        query_id: &str,
        helpful: bool,
        comment: Option<&str>,
    ) -> Result<()>;
    async fn stats_overview(&self) -> Result<StatsOverview>;

    /// Collection fingerprint for list ETags (count + max mtime).
    async fn collection_fingerprint(&self, prefix: Option<&str>) -> Result<(i64, i64)>;

    // Runtime settings
    async fn get_settings(&self) -> Result<Value>;
    async fn set_setting(&self, key: &str, value: &Value, updated_by: &str) -> Result<()>;
    async fn delete_setting(&self, key: &str) -> Result<bool>;

    // Agent memory ledger
    async fn insert_memory(&self, scope_key: &str, memory_type: &str, content: &str, content_hash: &str) -> Result<()>;
    async fn search_memories(&self, scope_key: &str, query: &str, limit: i64) -> Result<Vec<crate::services::memory::AgentMemory>>;
    async fn update_memory(&self, id: &str, new_content: &str, new_hash: &str) -> Result<()>;
    async fn delete_memory(&self, id: &str) -> Result<bool>;

    // Knowledge graph entities
    async fn upsert_entity(&self, id: &str, name: &str, entity_type: &str, source_doc: &str) -> Result<()>;
    async fn list_entities(&self) -> Result<Vec<crate::services::kg::Entity>>;

    // Maintenance
    async fn delete_derived_for_origin(&self, origin: &str) -> Result<()>;
}

pub mod pg;
pub mod sqlite;

/// Shape free-text user input into a safe FTS term list joined with OR
/// (recall-first); BM25/ts_rank provide IDF-style weighting on top.
/// Strips FTS operator characters, keeps at most 12 terms, empty input
/// yields an empty query (no results).
pub fn fts_query(q: &str) -> String {
    let terms: Vec<String> = q
        .split_whitespace()
        .map(|t| {
            t.chars()
                .map(|c| match c {
                    '"' | '\'' | '(' | ')' | '*' | ':' | '^' => ' ',
                    _ => c,
                })
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|t| !t.is_empty())
        .take(12)
        .collect();
    if terms.is_empty() {
        return String::new();
    }
    terms.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(" OR ")
}

