//! SQLite backend: rusqlite (bundled, FTS5) behind a Mutex. Port of the
//! TypeScript `src/store/sqlite.ts` + `schema.sql`.

use crate::domain::*;
use crate::error::{Error, Result};
use crate::store::{fts_query, DocumentRevision, MemoryMutation, Store, TranscriptWatermark, ZeroHitQuery};
use async_trait::async_trait;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::Value;
use std::sync::{Arc, Mutex, MutexGuard};

pub struct SqliteStore {
    writer: Arc<Mutex<Connection>>,
    readers: Vec<Arc<Mutex<Connection>>>,
    next_reader: std::sync::atomic::AtomicUsize,
}

type ConnGuard<'a> = MutexGuard<'a, Connection>;

/// Run a blocking rusqlite closure on the blocking thread pool.
async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Store(format!("spawn_blocking: {e}")))?
}

fn jstr(v: &Value) -> String {
    v.to_string()
}

fn pj<T: serde::de::DeserializeOwned>(raw: Option<&str>, fallback: T) -> T {
    match raw {
        Some(s) if !s.is_empty() => serde_json::from_str(s).unwrap_or(fallback),
        _ => fallback,
    }
}

fn s(v: Option<SqlValue>) -> Option<String> {
    match v {
        Some(SqlValue::Text(t)) => Some(t),
        Some(SqlValue::Integer(i)) => Some(i.to_string()),
        Some(SqlValue::Real(f)) => Some(f.to_string()),
        Some(SqlValue::Blob(b)) => Some(String::from_utf8_lossy(&b).to_string()),
        _ => None,
    }
}

fn num(v: Option<SqlValue>) -> i64 {
    match v {
        Some(SqlValue::Integer(i)) => i,
        Some(SqlValue::Real(f)) => f as i64,
        Some(SqlValue::Text(t)) => t.parse().unwrap_or(0),
        _ => 0,
    }
}

fn bool_v(v: Option<SqlValue>) -> bool {
    matches!(v, Some(SqlValue::Integer(1)))
}

pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS migrations (
  id INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS operations (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  source TEXT NOT NULL,
  action TEXT NOT NULL,
  paths TEXT NOT NULL,
  metadata TEXT,
  parent_id TEXT REFERENCES operations(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_operations_created_at ON operations(created_at);
CREATE INDEX IF NOT EXISTS idx_operations_parent ON operations(parent_id);

CREATE TABLE IF NOT EXISTS changes (
  id TEXT PRIMARY KEY,
  detected_at TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  change_type TEXT NOT NULL,
  old_hash TEXT, new_hash TEXT, source TEXT, operation_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_changes_path ON changes(rel_path);
CREATE INDEX IF NOT EXISTS idx_changes_detected ON changes(detected_at);

CREATE TABLE IF NOT EXISTS documents (
  id TEXT PRIMARY KEY,
  rel_path TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL DEFAULT 'page',
  origin TEXT NOT NULL DEFAULT 'wiki',
  title TEXT, summary TEXT,
  body TEXT NOT NULL DEFAULT '',
  frontmatter TEXT NOT NULL DEFAULT '{}',
  word_count INTEGER NOT NULL DEFAULT 0,
  outgoing_links TEXT NOT NULL DEFAULT '[]',
  hash TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  content_type TEXT,
  okf_type TEXT, tags TEXT NOT NULL DEFAULT '[]',
  status TEXT, stale_after TEXT, resource TEXT,
  generated_by TEXT, generated_at TEXT,
  verified TEXT, provenance TEXT,
  updated_at TEXT, updated_by TEXT,
  indexed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_documents_kind ON documents(kind);
CREATE INDEX IF NOT EXISTS idx_documents_origin ON documents(origin);
CREATE INDEX IF NOT EXISTS idx_documents_okf_type ON documents(okf_type);

CREATE TABLE IF NOT EXISTS chunks (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  heading_path TEXT,
  content TEXT NOT NULL,
  distilled TEXT,
  embedded_at TEXT,
  embed_model TEXT,
  UNIQUE(document_id, ordinal)
);
CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  content, heading_path, chunk_id UNINDEXED
);

-- Full-text index over agent memory content so paraphrased multi-word
-- queries match without substring semantics.
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
  content, memory_id UNINDEXED
);

CREATE TABLE IF NOT EXISTS embeddings (
  chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
  embedding BLOB,
  model TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edges (
  src TEXT NOT NULL, dst TEXT NOT NULL,
  PRIMARY KEY (src, dst)
);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);

CREATE TABLE IF NOT EXISTS connectors (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  config TEXT NOT NULL DEFAULT '{}',
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS connector_state (
  connector_id TEXT PRIMARY KEY REFERENCES connectors(id) ON DELETE CASCADE,
  watermark TEXT, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
  name TEXT PRIMARY KEY,
  description TEXT,
  prefixes TEXT NOT NULL DEFAULT '["*"]',
  connectors TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS queries (
  id TEXT PRIMARY KEY, created_at TEXT NOT NULL, query TEXT NOT NULL,
  mode TEXT NOT NULL, project TEXT, latency_ms REAL NOT NULL DEFAULT 0,
  result_count INTEGER NOT NULL DEFAULT 0, zero_hit INTEGER NOT NULL DEFAULT 0,
  top_paths TEXT NOT NULL DEFAULT '[]', source TEXT, error TEXT
);
CREATE INDEX IF NOT EXISTS idx_queries_created ON queries(created_at);
CREATE TABLE IF NOT EXISTS feedback (
  id TEXT PRIMARY KEY, query_id TEXT NOT NULL, helpful INTEGER NOT NULL,
  comment TEXT, created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS webhooks (
  id TEXT PRIMARY KEY, url TEXT NOT NULL,
  events TEXT NOT NULL DEFAULT '["change"]',
  prefixes TEXT NOT NULL DEFAULT '["*"]',
  enabled INTEGER NOT NULL DEFAULT 1,
  last_status TEXT, last_attempt_at TEXT, created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY, value TEXT NOT NULL,
  updated_at TEXT NOT NULL, updated_by TEXT
);
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY,
  scope_key TEXT NOT NULL,
  memory_type TEXT NOT NULL DEFAULT 'semantic',
  content TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  created_at TEXT NOT NULL,
  accessed_at TEXT NOT NULL,
  access_count INTEGER NOT NULL DEFAULT 0,
  source_session_id TEXT,
  source_ref TEXT,
  promote_candidate INTEGER NOT NULL DEFAULT 0,
  promoted_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_memories_scope ON memories(scope_key);
CREATE INDEX IF NOT EXISTS idx_memories_hash ON memories(content_hash);

CREATE TABLE IF NOT EXISTS entities (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  entity_type TEXT NOT NULL,
  summary TEXT,
  first_seen TEXT NOT NULL,
  source_doc TEXT
);

CREATE TABLE IF NOT EXISTS relation_edges (
  id TEXT PRIMARY KEY,
  src_entity TEXT NOT NULL,
  dst_entity TEXT NOT NULL,
  relation_type TEXT NOT NULL DEFAULT 'REFERENCES',
  fact TEXT NOT NULL DEFAULT '',
  source_doc TEXT NOT NULL,
  valid_at TEXT,
  invalid_at TEXT,
  expired_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_rel_src ON relation_edges(src_entity);
CREATE INDEX IF NOT EXISTS idx_rel_dst ON relation_edges(dst_entity);

CREATE TABLE IF NOT EXISTS wiki_sessions (
  id TEXT PRIMARY KEY,
  agent_name TEXT NOT NULL,
  user_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  context_summary TEXT
);

CREATE TABLE IF NOT EXISTS api_keys (
  name TEXT PRIMARY KEY,
  key_hash TEXT NOT NULL UNIQUE,
  key_prefix TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT '["*"]',
  role TEXT NOT NULL DEFAULT 'write',
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, created_by TEXT
);

CREATE TABLE IF NOT EXISTS document_revisions (
  id TEXT PRIMARY KEY,              -- 'rev-' + 12-char ulid-style
  rel_path TEXT NOT NULL,
  seq INTEGER NOT NULL,             -- monotonic per rel_path
  hash TEXT NOT NULL,               -- sha256 of body
  body TEXT NOT NULL,
  source TEXT,                      -- actor string ('human:luke', 'agent-x/wikillm-api', ...)
  operation TEXT NOT NULL,          -- 'create'|'update'|'delete'
  created_at TEXT NOT NULL          -- RFC3339
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_rev_path_seq ON document_revisions(rel_path, seq);
CREATE INDEX IF NOT EXISTS idx_rev_path ON document_revisions(rel_path);

CREATE TABLE IF NOT EXISTS memory_mutations (
  id TEXT PRIMARY KEY,
  memory_id TEXT NOT NULL,
  action TEXT NOT NULL,             -- 'add'|'update'|'delete'|'noop'
  old_content TEXT,
  new_content TEXT,
  timestamp TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mm_memory ON memory_mutations(memory_id);

-- Coding-agent transcript watermarks (consumed by the transcript sync loop)
CREATE TABLE IF NOT EXISTS transcript_watermarks (
  tool TEXT NOT NULL,               -- 'claude'|'codex'|'cursor'
  transcript_path TEXT NOT NULL,
  last_line INTEGER NOT NULL DEFAULT 0,
  prefix_hash TEXT,                 -- sha256 of first line at last sync
  last_synced_at TEXT,
  PRIMARY KEY (tool, transcript_path)
);

"#;

const HIT_COLUMNS: &str = "c.id AS chunk_id, c.document_id AS document_id, c.heading_path AS heading_path, c.content AS content, d.rel_path AS rel_path, d.kind AS kind, d.origin AS origin, d.title AS title, d.okf_type AS okf_type, d.tags AS tags, d.status AS status, d.stale_after AS stale_after, d.verified AS verified, d.hash AS hash, d.mtime AS mtime";

impl SqliteStore {
    pub fn open(db_path: &str) -> Result<Self> {
        Self::open_with_readers(db_path, 4)
    }

    pub fn open_with_readers(db_path: &str, reader_count: usize) -> Result<Self> {
        let writer = Connection::open(db_path)
            .map_err(|e| Error::Store(format!("open sqlite writer: {e}")))?;
        // Enable WAL mode for concurrent readers
        writer
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut readers = Vec::new();
        for _ in 0..reader_count.max(1) {
            let r = Connection::open(db_path)
                .map_err(|e| Error::Store(format!("open sqlite reader: {e}")))?;
            r.execute_batch("PRAGMA busy_timeout = 5000;").ok();
            readers.push(Arc::new(Mutex::new(r)));
        }
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            readers,
            next_reader: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn writer_conn(&self) -> MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn read_conn(&self) -> Arc<Mutex<Connection>> {
        let idx = self
            .next_reader
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.readers[idx % self.readers.len()].clone()
    }

    fn row_to_document(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentRecord> {
        Ok(DocumentRecord {
            id: row.get("id")?,
            rel_path: row.get("rel_path")?,
            kind: DocKind::from_str(&row.get::<_, String>("kind")?).unwrap_or(DocKind::Page),
            origin: row.get("origin")?,
            title: row.get("title")?,
            summary: row.get("summary")?,
            body: row.get::<_, Option<String>>("body")?.unwrap_or_default(),
            frontmatter: pj(Some(&row.get::<_, String>("frontmatter")?), serde_json::json!({})),
            word_count: row.get("word_count")?,
            outgoing_links: pj(Some(&row.get::<_, String>("outgoing_links")?), Vec::new()),
            hash: row.get("hash")?,
            mtime: row.get("mtime")?,
            content_type: row.get("content_type")?,
            okf_type: row.get("okf_type")?,
            tags: pj(Some(&row.get::<_, String>("tags")?), Vec::new()),
            status: row.get("status")?,
            stale_after: row.get("stale_after")?,
            resource: row.get("resource")?,
            generated_by: row.get("generated_by")?,
            generated_at: row.get("generated_at")?,
            verified: pj(row.get::<_, Option<String>>("verified")?.as_deref(), None),
            provenance: pj(row.get::<_, Option<String>>("provenance")?.as_deref(), None),
            updated_at: row.get("updated_at")?,
            updated_by: row.get("updated_by")?,
            indexed_at: row.get("indexed_at")?,
        })
    }

    fn row_to_chunk(row: &rusqlite::Row<'_>, rel_path: Option<String>) -> rusqlite::Result<ChunkRecord> {
        Ok(ChunkRecord {
            id: row.get("id")?,
            document_id: row.get("document_id")?,
            ordinal: row.get("ordinal")?,
            heading_path: row.get("heading_path")?,
            content: row.get("content")?,
            distilled: pj(row.get::<_, Option<String>>("distilled")?.as_deref(), None),
            embedded_at: row.get("embedded_at")?,
            embed_model: row.get("embed_model")?,
            rel_path: rel_path.unwrap_or_default(),
        })
    }

    fn row_to_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkHit> {
        Ok(ChunkHit {
            chunk_id: row.get("chunk_id")?,
            document_id: row.get("document_id")?,
            rel_path: row.get("rel_path")?,
            kind: row.get("kind")?,
            origin: row.get("origin")?,
            title: row.get("title")?,
            okf_type: row.get("okf_type")?,
            tags: pj(Some(&row.get::<_, String>("tags")?), Vec::new()),
            status: row.get("status")?,
            stale_after: row.get("stale_after")?,
            verified: pj(row.get::<_, Option<String>>("verified")?.as_deref(), None),
            hash: row.get("hash")?,
            mtime: row.get("mtime")?,
            heading_path: row.get("heading_path")?,
            content: row.get("content")?,
            score: row.get("score")?,
        })
    }

    /// Filter fragment referencing bare document columns (no alias).
    fn filter_clause(filters: Option<&SearchFilters>, params: &mut Vec<SqlValue>) -> String {
        let mut conds: Vec<String> = Vec::new();
        let push = |params: &mut Vec<SqlValue>, v: SqlValue| -> String {
            params.push(v);
            "?".to_string()
        };
        if let Some(f) = filters {
            if let Some(kinds) = &f.kinds {
                if !kinds.is_empty() {
                    let placeholders: Vec<String> = kinds
                        .iter()
                        .map(|k| push(params, SqlValue::Text(k.clone())))
                        .collect();
                    conds.push(format!("kind IN ({})", placeholders.join(",")));
                }
            }
            if let Some(origins) = &f.origins {
                if !origins.is_empty() {
                    let placeholders: Vec<String> = origins
                        .iter()
                        .map(|o| push(params, SqlValue::Text(o.clone())))
                        .collect();
                    conds.push(format!("origin IN ({})", placeholders.join(",")));
                }
            }
            if let Some(types) = &f.okf_types {
                if !types.is_empty() {
                    let placeholders: Vec<String> = types
                        .iter()
                        .map(|t| push(params, SqlValue::Text(t.clone())))
                        .collect();
                    conds.push(format!("okf_type IN ({})", placeholders.join(",")));
                }
            }
            for tag in f.tags.clone().unwrap_or_default() {
                let p = push(params, SqlValue::Text(format!("%\"{tag}\"%")));
                conds.push(format!("tags LIKE {p}"));
            }
            if let Some(statuses) = &f.statuses {
                if !statuses.is_empty() {
                    let placeholders: Vec<String> = statuses
                        .iter()
                        .map(|st| push(params, SqlValue::Text(st.clone())))
                        .collect();
                    conds.push(format!("status IN ({})", placeholders.join(",")));
                }
            }
            if let Some(trust) = &f.trust_min {
                if let Some((_, min)) = TRUST_ORDER.iter().find(|(k, _)| k == trust) {
                    if *min >= 1 {
                        conds.push("verified IS NOT NULL AND verified != '[]'".into());
                    }
                    if *min >= 2 {
                        conds.push("verified LIKE '%\"human:%'".into());
                    }
                }
            }
            if f.fresh_only.unwrap_or(false) {
                let p = push(params, SqlValue::Text(chrono::Utc::now().to_rfc3339()));
                conds.push(format!("(stale_after IS NULL OR stale_after > {p})"));
            }
            let prefixes: Vec<String> = f
                .path_prefixes
                .clone()
                .unwrap_or_else(|| vec!["*".into()])
                .into_iter()
                .filter(|p| p != "*")
                .collect();
            if !prefixes.is_empty() {
                let parts: Vec<String> = prefixes
                    .iter()
                    .flat_map(|p| {
                        vec![
                            push(params, SqlValue::Text(p.clone())),
                            push(params, SqlValue::Text(format!("{p}/%"))),
                            push(params, SqlValue::Text(format!("{p}/%/%"))),
                        ]
                    })
                    .collect();
                conds.push(format!("({})", parts.join(" OR ")));
            }
        }
        // Draft exclusion: promoter-generated drafts stay out of results
        // unless the caller explicitly filters for that status.
        let statuses_given = filters
            .and_then(|f| f.statuses.as_ref())
            .map_or(false, |s| !s.is_empty());
        if !statuses_given {
            conds.push("(status IS NULL OR status != 'draft')".into());
        }
        if conds.is_empty() {
            String::new()
        } else {
            format!(" AND {}", conds.join(" AND "))
        }
    }
}

fn row_to_agent_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::services::memory::AgentMemory> {
    Ok(crate::services::memory::AgentMemory {
        id: row.get("id")?,
        scope_key: row.get("scope_key")?,
        memory_type: match row.get::<_, String>("memory_type")?.as_str() {
            "episodic" => crate::services::memory::MemoryType::Episodic,
            "procedural" => crate::services::memory::MemoryType::Procedural,
            "preference" => crate::services::memory::MemoryType::Preference,
            _ => crate::services::memory::MemoryType::Semantic,
        },
        content: row.get("content")?,
        created_at: row.get("created_at")?,
        accessed_at: row.get("accessed_at")?,
        access_count: row.get("access_count")?,
        source_session_id: row.get("source_session_id")?,
        source_ref: row.get("source_ref")?,
    })
}

/// Record an access for every returned memory; returned rows reflect
/// post-bump state. Ids bind to ?1..?k, accessed_at to trailing ?k+1.
fn bump_memory_access(conn: &rusqlite::Connection, results: &mut [crate::services::memory::AgentMemory]) -> Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = results.iter().map(|m| m.id.clone()).collect();
    let placeholders = (1..=ids.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE memories SET access_count = access_count + 1, accessed_at = ?{n} WHERE id IN ({placeholders})",
        n = ids.len() + 1
    );
    let now = chrono::Utc::now().to_rfc3339();
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
    for id in &ids {
        params.push(id);
    }
    params.push(&now);
    conn.execute(&sql, params.as_slice()).map_err(|e| Error::Store(e.to_string()))?;
    for m in results.iter_mut() {
        m.access_count += 1;
    }
    Ok(())
}

#[async_trait]
impl Store for SqliteStore {
    fn backend(&self) -> &'static str {
        "sqlite"
    }

    async fn migrate(&self) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Store(e.to_string()))?;
        // Best-effort column additions for pre-existing databases;
        // duplicate-column errors on fresh schemas are ignored.
        for col in [
            "source_session_id TEXT",
            "source_ref TEXT",
            "promote_candidate INTEGER NOT NULL DEFAULT 0",
            "promoted_at TEXT",
        ] {
            let sql = format!("ALTER TABLE memories ADD COLUMN {col}");
            let _ = conn.execute(&sql, []);
        }
        // Idempotent backfill: index any memory rows missing from the FTS
        // table (pre-existing databases, or rows written before this index).
        conn.execute_batch(
            "INSERT INTO memories_fts(content, memory_id)
             SELECT content, id FROM memories
             WHERE id NOT IN (SELECT memory_id FROM memories_fts)",
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO migrations (id, applied_at) VALUES (2, ?)",
            [chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    async fn upsert_document(&self, doc: &DocumentInput) -> Result<()> {
        let conn = self.writer_conn();
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM documents WHERE rel_path = ?1",
                [&doc.rel_path],
                |r| r.get(0),
            )
            .ok();
        let id = existing.unwrap_or_else(|| ulid::Ulid::new().to_string());
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO documents
             (id, rel_path, kind, origin, title, summary, body, frontmatter, word_count,
              outgoing_links, hash, mtime, content_type, okf_type, tags, status, stale_after,
              resource, generated_by, generated_at, verified, provenance, updated_at, updated_by, indexed_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)
             ON CONFLICT(rel_path) DO UPDATE SET
               kind=excluded.kind, origin=excluded.origin, title=excluded.title,
               summary=excluded.summary, body=excluded.body, frontmatter=excluded.frontmatter,
               word_count=excluded.word_count, outgoing_links=excluded.outgoing_links,
               hash=excluded.hash, mtime=excluded.mtime, content_type=excluded.content_type,
               okf_type=excluded.okf_type, tags=excluded.tags, status=excluded.status,
               stale_after=excluded.stale_after, resource=excluded.resource,
               generated_by=excluded.generated_by, generated_at=excluded.generated_at,
               verified=excluded.verified, provenance=excluded.provenance,
               updated_at=excluded.updated_at, updated_by=excluded.updated_by,
               indexed_at=excluded.indexed_at",
            rusqlite::params![
                id,
                doc.rel_path,
                doc.kind.as_str(),
                doc.origin,
                doc.title,
                doc.summary,
                doc.body,
                jstr(&doc.frontmatter),
                doc.word_count,
                serde_json::to_string(&doc.outgoing_links).unwrap_or_else(|_| "[]".into()),
                doc.hash,
                doc.mtime,
                doc.content_type,
                doc.okf_type,
                serde_json::to_string(&doc.tags).unwrap_or_else(|_| "[]".into()),
                doc.status,
                doc.stale_after,
                doc.resource,
                doc.generated_by,
                doc.generated_at,
                doc.verified.as_ref().map(|v| serde_json::to_string(v).unwrap()),
                doc.provenance.as_ref().map(|v| serde_json::to_string(v).unwrap()),
                doc.updated_at,
                doc.updated_by,
                now,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn get_document(&self, rel_path: &str) -> Result<Option<DocumentRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM documents WHERE rel_path = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query([rel_path])
            .map_err(|e| Error::Store(e.to_string()))?;
        match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => Ok(Some(Self::row_to_document(row).map_err(|e| Error::Store(e.to_string()))?)),
            None => Ok(None),
        }
    }

    async fn delete_document(&self, rel_path: &str) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "DELETE FROM chunks_fts WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id = c.document_id WHERE d.rel_path = ?1)",
            [rel_path],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        conn.execute("DELETE FROM documents WHERE rel_path = ?1", [rel_path])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn list_documents(
        &self,
        opts: &ListOptions,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<PageList<DocumentRecord>> {
        let conn = self.writer_conn();
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();
        if let Some(folder) = &opts.folder {
            conds.push("(rel_path LIKE ? OR rel_path LIKE ?)".into());
            params.push(SqlValue::Text(format!("{folder}/%")));
            params.push(SqlValue::Text(format!("{folder}/%/%")));
        }
        if let Some(kind) = &opts.kind {
            conds.push("kind = ?".into());
            params.push(SqlValue::Text(kind.as_str().into()));
        }
        if let Some(origin) = &opts.origin {
            conds.push("origin = ?".into());
            params.push(SqlValue::Text(origin.clone()));
        }
        if let Some(cursor) = cursor {
            conds.push("rel_path > ?".into());
            params.push(SqlValue::Text(cursor.to_string()));
        }
        let filter_sql = Self::filter_clause(opts.filters.as_ref(), &mut params);
        if !filter_sql.is_empty() {
            conds.push(filter_sql.trim_start_matches(" AND ").to_string());
        }
        let sql = format!(
            "SELECT * FROM documents{} ORDER BY rel_path LIMIT {}",
            if conds.is_empty() { String::new() } else { format!(" WHERE {}", conds.join(" AND ")) },
            limit + 1
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params.iter()))
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().map_err(|e| Error::Store(e.to_string()))? {
            items.push(Self::row_to_document(row).map_err(|e| Error::Store(e.to_string()))?);
        }
        let next_cursor = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|d| d.rel_path.clone())
        } else {
            None
        };
        Ok(PageList { items, next_cursor })
    }

    async fn count_documents(&self, origin: Option<&str>) -> Result<i64> {
        let conn = self.writer_conn();
        let n = match origin {
            Some(o) => conn
                .query_row("SELECT COUNT(*) FROM documents WHERE origin = ?1", [o], |r| r.get::<_, i64>(0)),
            None => conn.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0)),
        }
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(n)
    }

    async fn replace_chunks(&self, document_id: &str, chunks: &[ChunkInput]) -> Result<()> {
        let mut conn = self.writer_conn();
        let tx = conn
            .transaction()
            .map_err(|e| Error::Store(e.to_string()))?;
        tx.execute(
            "DELETE FROM chunks_fts WHERE chunk_id IN (SELECT id FROM chunks WHERE document_id = ?1)",
            [document_id],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM chunks WHERE document_id = ?1", [document_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        for ch in chunks {
            let cid = ulid::Ulid::new().to_string();
            tx.execute(
                "INSERT INTO chunks (id, document_id, ordinal, heading_path, content, distilled) VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    cid,
                    document_id,
                    ch.ordinal,
                    ch.heading_path,
                    ch.content,
                    ch.distilled.as_ref().map(|d| serde_json::to_string(d).unwrap()),
                ],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
            tx.execute(
                "INSERT INTO chunks_fts (content, heading_path, chunk_id) VALUES (?1,?2,?3)",
                rusqlite::params![ch.content, ch.heading_path.clone().unwrap_or_default(), cid],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        tx.execute("DELETE FROM embeddings WHERE chunk_id NOT IN (SELECT id FROM chunks)", [])
            .map_err(|e| Error::Store(e.to_string()))?;
        tx.commit().map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn get_chunks_for_document(&self, document_id: &str) -> Result<Vec<ChunkRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM chunks WHERE document_id = ?1 ORDER BY ordinal")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([document_id], |row| Self::row_to_chunk(row, None))
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    #[allow(clippy::let_and_return)]
    async fn get_chunk(&self, chunk_id: &str) -> Result<Option<ChunkRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM chunks WHERE id = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query([chunk_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => Ok(Some(Self::row_to_chunk(row, None).map_err(|e| Error::Store(e.to_string()))?)),
            None => Ok(None),
        }
    }

    async fn upsert_embeddings(
        &self,
        items: &[(String, Vec<f32>)],
        model: &str,
        embedded_at: &str,
    ) -> Result<()> {
        let conn = self.writer_conn();
        for (chunk_id, vector) in items {
            let blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT INTO embeddings (chunk_id, embedding, model, created_at) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(chunk_id) DO UPDATE SET embedding=excluded.embedding, model=excluded.model, created_at=excluded.created_at",
                rusqlite::params![chunk_id, blob, model, embedded_at],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
        for chunk_id in &ids {
            conn.execute(
                "UPDATE chunks SET embedded_at = ?1, embed_model = ?2 WHERE id = ?3",
                rusqlite::params![embedded_at, model, chunk_id],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    async fn list_unembedded_chunks(&self, limit: i64) -> Result<Vec<ChunkRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare(
                "SELECT c.*, d.rel_path AS rel_path FROM chunks c
                 JOIN documents d ON d.id = c.document_id
                 WHERE c.embedded_at IS NULL ORDER BY d.indexed_at, c.ordinal LIMIT ?1",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([limit], |row| {
                let rel: String = row.get("rel_path")?;
                Self::row_to_chunk(row, Some(rel))
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn reset_embeddings(&self, _dims: Option<i32>) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute("DELETE FROM embeddings", [])
            .map_err(|e| Error::Store(e.to_string()))?;
        conn.execute("UPDATE chunks SET embedded_at = NULL, embed_model = NULL", [])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn replace_edges(&self, src: &str, dsts: &[String]) -> Result<()> {
        let mut conn = self.writer_conn();
        let tx = conn.transaction().map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM edges WHERE src = ?1", [src])
            .map_err(|e| Error::Store(e.to_string()))?;
        for dst in dsts {
            tx.execute("INSERT OR IGNORE INTO edges (src, dst) VALUES (?1,?2)", [src, dst])
                .map_err(|e| Error::Store(e.to_string()))?;
        }
        tx.commit().map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn backlinks(&self, rel_path: &str, limit: i64) -> Result<Vec<String>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT src FROM edges WHERE dst = ?1 LIMIT ?2")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![rel_path, limit], |r| r.get::<_, String>(0))
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    #[allow(clippy::let_and_return)]
    async fn search_fts(
        &self,
        q: &str,
        limit: i64,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<ChunkHit>> {
        let match_expr = fts_query(q);
        if match_expr.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.writer_conn();
        let mut params: Vec<SqlValue> = vec![SqlValue::Text(match_expr)];
        let filter_sql = Self::filter_clause(filters, &mut params);
        params.push(SqlValue::Integer(limit));
        let sql = format!(
            "SELECT {HIT_COLUMNS}, -bm25(chunks_fts) AS score
             FROM chunks_fts
             JOIN chunks c ON c.id = chunks_fts.chunk_id
             JOIN documents d ON d.id = c.document_id
             WHERE chunks_fts MATCH ?1{filter_sql}
             ORDER BY score DESC LIMIT ?{limit_placeholder}",
            limit_placeholder = params.len()
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params.iter()))
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| Error::Store(e.to_string()))? {
            out.push(Self::row_to_hit(row).map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    #[allow(clippy::let_and_return)]
    async fn search_vector(&self, vector: &[f32], limit: i64, filters: Option<&SearchFilters>) -> Result<Vec<ChunkHit>> {
        let conn = self.read_conn();
        let conn = conn.lock().unwrap_or_else(|p| p.into_inner());
        let vector_f32: Vec<f32> = vector.to_vec();

        let sql = format!(
            "SELECT e.embedding, 0.0 AS score, {HIT_COLUMNS}\
             FROM embeddings e\
             JOIN chunks c ON c.id = e.chunk_id\
             JOIN documents d ON d.id = c.document_id\
             WHERE e.embedding IS NOT NULL"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| Error::Store(e.to_string()))?;

        let mut rows = stmt.query([]).map_err(|e| Error::Store(e.to_string()))?;
        let mut scored: Vec<ChunkHit> = Vec::new();

        while let Some(row) = rows.next().map_err(|e| Error::Store(e.to_string()))? {
            let blob: Vec<u8> = row.get::<_, Option<Vec<u8>>>("embedding").unwrap_or_default().unwrap_or_default();
            if blob.len() != vector_f32.len() * 4 { continue; }

            let stored: Vec<f32> = blob.chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();

            let dot: f32 = vector.iter().zip(stored.iter()).map(|(a, b)| a * b).sum();
            let norm_a: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            let norm_b: f32 = stored.iter().map(|x| x * x).sum::<f32>().sqrt();
            let denom = norm_a * norm_b;
            if denom == 0.0 { continue; }

            let mut hit = Self::row_to_hit(row).map_err(|e| Error::Store(e.to_string()))?;

            // Apply kind filter
            if let Some(f) = filters {
                if let Some(kinds) = &f.kinds {
                    if !kinds.iter().any(|k| *k == hit.kind) { continue; }
                }
                if let Some(prefixes) = &f.path_prefixes {
                    if !prefixes.iter().any(|p| p == "*" || hit.rel_path.starts_with(p.as_str())) { continue; }
                }
            }

            hit.score = (dot / denom) as f64;
            scored.push(hit);
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit as usize);
        Ok(scored)
    }

    fn supports_vector(&self) -> bool {
        true
    }

    async fn insert_operation(&self, op: &Operation) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO operations (id, created_at, source, action, paths, metadata, parent_id) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                op.id,
                op.created_at,
                op.source,
                op.action,
                serde_json::to_string(&op.paths).unwrap_or_else(|_| "[]".into()),
                op.metadata.as_ref().map(|m| serde_json::to_string(m).unwrap()),
                op.parent_id,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn get_operation(&self, id: &str) -> Result<Option<Operation>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM operations WHERE id = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt.query([id]).map_err(|e| Error::Store(e.to_string()))?;
        match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => {
                let get = |c: &str| -> Result<String> {
                    row.get(c).map_err(|e| Error::Store(e.to_string()))
                };
                Ok(Some(Operation {
                    id: get("id")?,
                    created_at: get("created_at")?,
                    source: get("source")?,
                    action: get("action")?,
                    paths: pj(Some(&get("paths")?), Vec::new()),
                    metadata: pj(get("metadata").ok().as_deref(), None),
                    parent_id: get("parent_id").ok(),
                }))
            }
            None => Ok(None),
        }
    }

    async fn insert_change(&self, change: &ChangeEventData) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO changes (id, detected_at, rel_path, change_type, old_hash, new_hash, source, operation_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                change.id, change.detected_at, change.rel_path, change.change_type,
                change.old_hash, change.new_hash, change.source, change.operation_id,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn list_changes(
        &self,
        since: Option<&str>,
        path: Option<&str>,
        source: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ChangeEventData>> {
        let conn = self.writer_conn();
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();
        if let Some(s) = since {
            conds.push("detected_at > ?".into());
            params.push(SqlValue::Text(s.to_string()));
        }
        if let Some(p) = path {
            conds.push("rel_path = ?".into());
            params.push(SqlValue::Text(p.to_string()));
        }
        if let Some(s) = source {
            conds.push("source = ?".into());
            params.push(SqlValue::Text(s.to_string()));
        }
        let where_clause = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        params.push(SqlValue::Integer(limit));
        let sql = format!(
            "SELECT * FROM changes{where_clause} ORDER BY detected_at DESC LIMIT ?"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params.iter()))
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| Error::Store(e.to_string()))? {
            let get = |c: &str| -> Result<String> {
                row.get(c).map_err(|e| Error::Store(e.to_string()))
            };
            let opt = |c: &str| -> Result<Option<String>> {
                row.get(c).map_err(|e| Error::Store(e.to_string()))
            };
            out.push(ChangeEventData {
                id: get("id")?,
                rel_path: get("rel_path")?,
                change_type: get("change_type")?,
                old_hash: opt("old_hash")?,
                new_hash: opt("new_hash")?,
                source: opt("source")?,
                operation_id: opt("operation_id")?,
                detected_at: get("detected_at")?,
            });
        }
        Ok(out)
    }

    async fn put_connector(&self, c: &ConnectorConfig) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO connectors (id, kind, config, enabled, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, config=excluded.config, enabled=excluded.enabled, updated_at=excluded.updated_at",
            rusqlite::params![c.id, c.kind, jstr(&c.config), c.enabled as i64, c.created_at, c.updated_at],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn get_connector(&self, id: &str) -> Result<Option<ConnectorConfig>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM connectors WHERE id = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt.query([id]).map_err(|e| Error::Store(e.to_string()))?;
        match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => {
                let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ConnectorConfig> {
                    Ok(ConnectorConfig {
                        id: row.get("id")?,
                        kind: row.get("kind")?,
                        config: pj(Some(&row.get::<_, String>("config")?), serde_json::json!({})),
                        enabled: bool_v(row.get("enabled")?),
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                };
                Ok(map(&row).map_err(|e| Error::Store(e.to_string())).ok())
            }
            None => Ok(None),
        }
    }

    #[allow(clippy::let_and_return)]
    async fn list_connectors(&self) -> Result<Vec<ConnectorConfig>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM connectors ORDER BY id")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ConnectorConfig {
                    id: row.get("id")?,
                    kind: row.get("kind")?,
                    config: pj(Some(&row.get::<_, String>("config")?), serde_json::json!({})),
                    enabled: bool_v(row.get("enabled")?),
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn delete_connector(&self, id: &str) -> Result<bool> {
        let conn = self.writer_conn();
        let n = conn
            .execute("DELETE FROM connectors WHERE id = ?1", [id])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(n > 0)
    }

    #[allow(clippy::let_and_return)]
    async fn get_connector_state(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT watermark FROM connector_state WHERE connector_id = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt.query([id]).map_err(|e| Error::Store(e.to_string()))?;
        match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => {
                let wm: Option<String> = row.get("watermark").map_err(|e| Error::Store(e.to_string()))?;
                Ok(pj(wm.as_deref(), None))
            }
            None => Ok(None),
        }
    }

    async fn set_connector_state(&self, id: &str, watermark: &Value) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO connector_state (connector_id, watermark, updated_at) VALUES (?1,?2,?3)
             ON CONFLICT(connector_id) DO UPDATE SET watermark=excluded.watermark, updated_at=excluded.updated_at",
            rusqlite::params![id, jstr(watermark), chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn put_project(&self, p: &ProjectInput) -> Result<()> {
        let conn = self.writer_conn();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO projects (name, description, prefixes, connectors, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(name) DO UPDATE SET description=excluded.description, prefixes=excluded.prefixes, connectors=excluded.connectors, updated_at=excluded.updated_at",
            rusqlite::params![
                p.name,
                p.description,
                serde_json::to_string(&p.prefixes).unwrap_or_else(|_| "[\"*\"]".into()),
                serde_json::to_string(&p.connectors).unwrap_or_else(|_| "[]".into()),
                now, now,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn get_project(&self, name: &str) -> Result<Option<ProjectRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM projects WHERE name = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt.query([name]).map_err(|e| Error::Store(e.to_string()))?;
        match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => {
                let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ProjectRecord> {
                    Ok(ProjectRecord {
                        name: row.get("name")?,
                        description: row.get("description")?,
                        prefixes: pj(Some(&row.get::<_, String>("prefixes")?), vec!["*".into()]),
                        connectors: pj(Some(&row.get::<_, String>("connectors")?), Vec::new()),
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                };
                Ok(map(&row).map_err(|e| Error::Store(e.to_string())).ok())
            }
            None => Ok(None),
        }
    }

    #[allow(clippy::let_and_return)]
    async fn list_projects(&self) -> Result<Vec<ProjectRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM projects ORDER BY name")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ProjectRecord {
                    name: row.get("name")?,
                    description: row.get("description")?,
                    prefixes: pj(Some(&row.get::<_, String>("prefixes")?), vec!["*".into()]),
                    connectors: pj(Some(&row.get::<_, String>("connectors")?), Vec::new()),
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn delete_project(&self, name: &str) -> Result<bool> {
        let conn = self.writer_conn();
        let n = conn
            .execute("DELETE FROM projects WHERE name = ?1", [name])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(n > 0)
    }

    #[allow(clippy::let_and_return)]
    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM api_keys ORDER BY name")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ApiKeyRecord {
                    name: row.get("name")?,
                    key_hash: row.get("key_hash")?,
                    key_prefix: row.get("key_prefix")?,
                    scope: pj(Some(&row.get::<_, String>("scope")?), vec!["*".into()]),
                    role: row.get("role")?,
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                    created_by: row.get("created_by")?,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    #[allow(clippy::let_and_return)]
    async fn get_api_key(&self, name: &str) -> Result<Option<ApiKeyRecord>> {
        let all = self.list_api_keys().await?;
        Ok(all.into_iter().find(|k| k.name == name))
    }

    #[allow(clippy::let_and_return)]
    async fn find_api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKeyRecord>> {
        let all = self.list_api_keys().await?;
        Ok(all.into_iter().find(|k| k.key_hash == key_hash))
    }

    async fn upsert_api_key(&self, input: &ApiKeyUpsert) -> Result<()> {
        let conn = self.writer_conn();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO api_keys (name, key_hash, key_prefix, scope, role, created_at, updated_at, created_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(name) DO UPDATE SET key_hash=excluded.key_hash, key_prefix=excluded.key_prefix, scope=excluded.scope, role=excluded.role, updated_at=excluded.updated_at, created_by=excluded.created_by",
            rusqlite::params![
                input.name, input.key_hash, input.key_prefix,
                serde_json::to_string(&input.scope).unwrap_or_else(|_| "[\"*\"]".into()),
                input.role, now, now, input.created_by,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn delete_api_key(&self, name: &str) -> Result<bool> {
        let conn = self.writer_conn();
        let n = conn
            .execute("DELETE FROM api_keys WHERE name = ?1", [name])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(n > 0)
    }

    #[allow(clippy::let_and_return)]
    async fn count_api_keys(&self) -> Result<i64> {
        let conn = self.writer_conn();
        conn.query_row("SELECT COUNT(*) FROM api_keys", [], |r| r.get::<_, i64>(0))
            .map_err(|e| Error::Store(e.to_string()))
    }

    #[allow(clippy::let_and_return)]
    async fn list_webhooks(&self) -> Result<Vec<WebhookRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT * FROM webhooks ORDER BY id")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(WebhookRecord {
                    id: row.get("id")?,
                    url: row.get("url")?,
                    events: pj(Some(&row.get::<_, String>("events")?), vec!["change".into()]),
                    prefixes: pj(Some(&row.get::<_, String>("prefixes")?), vec!["*".into()]),
                    enabled: bool_v(row.get("enabled")?),
                    last_status: row.get("last_status")?,
                    last_attempt_at: row.get("last_attempt_at")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    #[allow(clippy::let_and_return)]
    async fn get_webhook(&self, id: &str) -> Result<Option<WebhookRecord>> {
        let all = self.list_webhooks().await?;
        Ok(all.into_iter().find(|w| w.id == id))
    }

    async fn put_webhook(&self, w: &WebhookRecord) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO webhooks (id, url, events, prefixes, enabled, created_at) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET url=excluded.url, events=excluded.events, prefixes=excluded.prefixes, enabled=excluded.enabled",
            rusqlite::params![
                w.id, w.url,
                serde_json::to_string(&w.events).unwrap_or_else(|_| "[\"change\"]".into()),
                serde_json::to_string(&w.prefixes).unwrap_or_else(|_| "[\"*\"]".into()),
                w.enabled as i64, w.created_at,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn delete_webhook(&self, id: &str) -> Result<bool> {
        let conn = self.writer_conn();
        let n = conn
            .execute("DELETE FROM webhooks WHERE id = ?1", [id])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(n > 0)
    }

    async fn record_webhook_attempt(&self, id: &str, status: &str) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "UPDATE webhooks SET last_status = ?1, last_attempt_at = ?2 WHERE id = ?3",
            rusqlite::params![status, chrono::Utc::now().to_rfc3339(), id],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn record_query(&self, q: &QueryRecord) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO queries (id, created_at, query, mode, project, latency_ms, result_count, zero_hit, top_paths, source, error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![
                q.id, q.created_at, q.query, q.mode, q.project, q.latency_ms,
                q.result_count, q.zero_hit as i64,
                serde_json::to_string(&q.top_paths).unwrap_or_else(|_| "[]".into()),
                q.source, q.error,
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn record_feedback(&self, query_id: &str, helpful: bool, comment: Option<&str>) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO feedback (id, query_id, helpful, comment, created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![ulid::Ulid::new().to_string(), query_id, helpful as i64, comment, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::let_and_return)]
    async fn stats_overview(&self) -> Result<StatsOverview> {
        let conn = self.writer_conn();
        let one = |sql: &str| -> Result<i64> {
            conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map_err(|e| Error::Store(e.to_string()))
        };
        Ok(StatsOverview {
            documents: one("SELECT COUNT(*) FROM documents")?,
            chunks: one("SELECT COUNT(*) FROM chunks")?,
            embedded_chunks: one("SELECT COUNT(*) FROM chunks WHERE embedded_at IS NOT NULL")?,
            queries: one("SELECT COUNT(*) FROM queries")?,
            zero_hit_queries: one("SELECT COUNT(*) FROM queries WHERE zero_hit = 1")?,
            feedback_helpful: one("SELECT COUNT(*) FROM feedback WHERE helpful = 1")?,
            feedback_total: one("SELECT COUNT(*) FROM feedback")?,
        })
    }

    #[allow(clippy::let_and_return)]
    async fn get_settings(&self) -> Result<Value> {
        let conn = self.writer_conn();
        let mut stmt = conn
            .prepare("SELECT key, value FROM settings")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get("key")?;
                let value: String = row.get("value")?;
                Ok((key, value))
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let (key, value) = row.map_err(|e| Error::Store(e.to_string()))?;
            if let Ok(v) = serde_json::from_str::<Value>(&value) {
                map.insert(key, v);
            }
        }
        Ok(Value::Object(map))
    }

    async fn set_setting(&self, key: &str, value: &Value, updated_by: &str) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO settings (key, value, updated_at, updated_by) VALUES (?1,?2,?3,?4)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at, updated_by=excluded.updated_by",
            rusqlite::params![key, jstr(value), chrono::Utc::now().to_rfc3339(), updated_by],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn delete_setting(&self, key: &str) -> Result<bool> {
        let conn = self.writer_conn();
        let n = conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(n > 0)
    }

    async fn delete_derived_for_origin(&self, origin: &str) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "DELETE FROM chunks_fts WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id = c.document_id WHERE d.origin = ?1)",
            [origin],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        conn.execute("DELETE FROM documents WHERE origin = ?1", [origin])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn collection_fingerprint(&self, prefix: Option<&str>) -> Result<(i64, i64)> {
        let conn = self.writer_conn();
        let (count, max_mtime) = match prefix {
            Some(p) => conn
                .query_row(
                    "SELECT COUNT(*) AS n, COALESCE(MAX(mtime),0) AS m FROM documents WHERE rel_path LIKE ?1 OR rel_path LIKE ?2",
                    rusqlite::params![format!("{p}/%"), format!("{p}/%/%")],
                    |r| Ok((r.get::<_, i64>("n")?, r.get::<_, i64>("m")?)),
                )
                .map_err(|e| Error::Store(e.to_string()))?,
            None => conn
                .query_row(
                    "SELECT COUNT(*) AS n, COALESCE(MAX(mtime),0) AS m FROM documents",
                    [],
                    |r| Ok((r.get::<_, i64>("n")?, r.get::<_, i64>("m")?)),
                )
                .map_err(|e| Error::Store(e.to_string()))?,
        };
        Ok((count, max_mtime))
    }

    async fn insert_memory(
        &self,
        scope_key: &str,
        memory_type: &str,
        content: &str,
        content_hash: &str,
        source_session_id: Option<&str>,
        source_ref: Option<&str>,
        promote_candidate: Option<bool>,
    ) -> Result<String> {
        let conn = self.writer_conn();
        let now = chrono::Utc::now().to_rfc3339();
        let id = ulid::Ulid::new().to_string();
        conn.execute(
            "INSERT INTO memories (id, scope_key, memory_type, content, content_hash, created_at, accessed_at, source_session_id, source_ref, promote_candidate) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                id.clone(),
                scope_key,
                memory_type,
                content,
                content_hash,
                now,
                now,
                source_session_id,
                source_ref,
                promote_candidate.unwrap_or(false) as i64,
            ],
        ).map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT INTO memories_fts (content, memory_id) VALUES (?1, ?2)",
            rusqlite::params![content, id],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(id)
    }

    async fn update_memory(&self, id: &str, new_content: &str, new_hash: &str) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "UPDATE memories SET content = ?1, content_hash = ?2, accessed_at = ?3 WHERE id = ?4",
            rusqlite::params![new_content, new_hash, chrono::Utc::now().to_rfc3339(), id],
        ).map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "DELETE FROM memories_fts WHERE memory_id = ?1",
            [id],
        ).map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT INTO memories_fts (content, memory_id) VALUES (?1, ?2)",
            rusqlite::params![new_content, id],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn delete_memory(&self, id: &str) -> Result<bool> {
        let conn = self.writer_conn();
        let n = conn.execute("DELETE FROM memories WHERE id = ?1", [id]).map_err(|e| Error::Store(e.to_string()))?;
        if n > 0 {
            conn.execute(
                "DELETE FROM memories_fts WHERE memory_id = ?1",
                [id],
            ).map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(n > 0)
    }

    async fn search_memories(&self, scope_key: &str, query: &str, limit: i64) -> Result<Vec<crate::services::memory::AgentMemory>> {
        let conn = self.writer_conn();
        // Term-based FTS first (paraphrased multi-word queries match on
        // shared terms, not substrings); fall back to substring LIKE when
        // the query is empty or FTS surfaces nothing.
        if !query.trim().is_empty() {
            let match_expr = crate::store::fts_query(query);
            if !match_expr.is_empty() {
                let mut stmt = conn.prepare(
                    "SELECT m.* FROM memories_fts f JOIN memories m ON m.id = f.memory_id \
                     WHERE memories_fts MATCH ?1 AND m.scope_key = ?2 \
                     ORDER BY -bm25(memories_fts), m.access_count DESC, m.created_at DESC LIMIT ?3"
                ).map_err(|e| Error::Store(e.to_string()))?;
                let rows = stmt.query_map(rusqlite::params![match_expr, scope_key, limit], |row| {
                    Ok(row_to_agent_memory(row)?)
                }).map_err(|e| Error::Store(e.to_string()))?;
                let mut results = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))?;
                if !results.is_empty() {
                    bump_memory_access(&conn, &mut results)?;
                    return Ok(results);
                }
                // FTS found nothing — fall through to LIKE.
            }
        }
        let mut stmt = conn.prepare(
            "SELECT * FROM memories WHERE scope_key = ?1 AND content LIKE ?2 ESCAPE '\\' ORDER BY access_count DESC, created_at DESC LIMIT ?3"
        ).map_err(|e| Error::Store(e.to_string()))?;
        let pattern = format!("%{}%", crate::store::like_escape(query));
        let rows = stmt.query_map(rusqlite::params![scope_key, pattern, limit], |row| {
            Ok(crate::services::memory::AgentMemory {
                id: row.get("id")?,
                scope_key: row.get("scope_key")?,
                memory_type: match row.get::<_, String>("memory_type")?.as_str() {
                    "episodic" => crate::services::memory::MemoryType::Episodic,
                    "procedural" => crate::services::memory::MemoryType::Procedural,
                    "preference" => crate::services::memory::MemoryType::Preference,
                    _ => crate::services::memory::MemoryType::Semantic,
                },
                content: row.get("content")?,
                created_at: row.get("created_at")?,
                accessed_at: row.get("accessed_at")?,
                access_count: row.get("access_count")?,
                source_session_id: row.get("source_session_id")?,
                source_ref: row.get("source_ref")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        let mut results = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))?;
        bump_memory_access(&conn, &mut results)?;
        Ok(results)
    }

    async fn list_promotable_memories(&self, limit: i64) -> Result<Vec<crate::services::memory::AgentMemory>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM memories WHERE promote_candidate = 1 AND promoted_at IS NULL ORDER BY created_at ASC LIMIT ?1"
        ).map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok(crate::services::memory::AgentMemory {
                id: row.get("id")?,
                scope_key: row.get("scope_key")?,
                memory_type: match row.get::<_, String>("memory_type")?.as_str() {
                    "episodic" => crate::services::memory::MemoryType::Episodic,
                    "procedural" => crate::services::memory::MemoryType::Procedural,
                    "preference" => crate::services::memory::MemoryType::Preference,
                    _ => crate::services::memory::MemoryType::Semantic,
                },
                content: row.get("content")?,
                created_at: row.get("created_at")?,
                accessed_at: row.get("accessed_at")?,
                access_count: row.get("access_count")?,
                source_session_id: row.get("source_session_id")?,
                source_ref: row.get("source_ref")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn mark_memory_promoted(&self, id: &str, promoted_at: &str) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "UPDATE memories SET promoted_at = ?1 WHERE id = ?2",
            rusqlite::params![promoted_at, id],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn record_memory_mutation(&self, m: &MemoryMutation) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO memory_mutations (id, memory_id, action, old_content, new_content, timestamp) VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![m.id, m.memory_id, m.action, m.old_content, m.new_content, m.timestamp],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn list_memory_mutations(&self, memory_id: &str, limit: i64) -> Result<Vec<MemoryMutation>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare(
            "SELECT id, memory_id, action, old_content, new_content, timestamp FROM memory_mutations WHERE memory_id = ?1 ORDER BY timestamp DESC, rowid DESC LIMIT ?2"
        ).map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt.query_map(rusqlite::params![memory_id, limit], |row| {
            Ok(MemoryMutation {
                id: row.get("id")?,
                memory_id: row.get("memory_id")?,
                action: row.get("action")?,
                old_content: row.get("old_content")?,
                new_content: row.get("new_content")?,
                timestamp: row.get("timestamp")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn insert_revision(&self, rel_path: &str, hash: &str, body: &str, source: &str, operation: &str) -> Result<i64> {
        let conn = self.writer_conn();
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM document_revisions WHERE rel_path = ?1",
            [rel_path],
            |row| row.get(0),
        ).map_err(|e| Error::Store(e.to_string()))?;
        let id = format!("rev-{}", ulid::Ulid::new());
        conn.execute(
            "INSERT INTO document_revisions (id, rel_path, seq, hash, body, source, operation, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![id, rel_path, seq, hash, body, Some(source), operation, chrono::Utc::now().to_rfc3339()],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(seq)
    }

    async fn list_revisions(&self, rel_path: &str, limit: i64) -> Result<Vec<DocumentRevision>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare(
            "SELECT id, rel_path, seq, hash, source, operation, created_at FROM document_revisions WHERE rel_path = ?1 ORDER BY seq DESC LIMIT ?2"
        ).map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt.query_map(rusqlite::params![rel_path, limit], |row| {
            Ok(DocumentRevision {
                id: row.get("id")?,
                rel_path: row.get("rel_path")?,
                seq: row.get("seq")?,
                hash: row.get("hash")?,
                // Metadata only; the body is loaded via get_revision.
                body: String::new(),
                source: row.get("source")?,
                operation: row.get("operation")?,
                created_at: row.get("created_at")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn get_revision(&self, rel_path: &str, seq: i64) -> Result<Option<DocumentRevision>> {
        let conn = self.writer_conn();
        let res = conn.query_row(
            "SELECT id, rel_path, seq, hash, body, source, operation, created_at FROM document_revisions WHERE rel_path = ?1 AND seq = ?2",
            rusqlite::params![rel_path, seq],
            |row| Ok(DocumentRevision {
                id: row.get("id")?,
                rel_path: row.get("rel_path")?,
                seq: row.get("seq")?,
                hash: row.get("hash")?,
                body: row.get("body")?,
                source: row.get("source")?,
                operation: row.get("operation")?,
                created_at: row.get("created_at")?,
            }),
        )
        .map_err(|e| Error::Store(e.to_string()))
        .ok();
        Ok(res)
    }

    async fn get_revision_by_hash(&self, rel_path: &str, hash: &str) -> Result<Option<DocumentRevision>> {
        let conn = self.writer_conn();
        let res = conn.query_row(
            "SELECT id, rel_path, seq, hash, body, source, operation, created_at FROM document_revisions WHERE rel_path = ?1 AND hash = ?2 ORDER BY seq DESC LIMIT 1",
            rusqlite::params![rel_path, hash],
            |row| Ok(DocumentRevision {
                id: row.get("id")?,
                rel_path: row.get("rel_path")?,
                seq: row.get("seq")?,
                hash: row.get("hash")?,
                body: row.get("body")?,
                source: row.get("source")?,
                operation: row.get("operation")?,
                created_at: row.get("created_at")?,
            }),
        )
        .ok();
        Ok(res)
    }

    async fn get_watermark(&self, tool: &str, path: &str) -> Result<Option<TranscriptWatermark>> {
        let conn = self.writer_conn();
        let res = conn.query_row(
            "SELECT tool, transcript_path, last_line, prefix_hash, last_synced_at FROM transcript_watermarks WHERE tool = ?1 AND transcript_path = ?2",
            [tool, path],
            |row| Ok(TranscriptWatermark {
                tool: row.get("tool")?,
                transcript_path: row.get("transcript_path")?,
                last_line: row.get("last_line")?,
                prefix_hash: row.get("prefix_hash")?,
                last_synced_at: row.get("last_synced_at")?,
            }),
        )
        .ok();
        Ok(res)
    }

    async fn upsert_watermark(&self, w: &TranscriptWatermark) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO transcript_watermarks (tool, transcript_path, last_line, prefix_hash, last_synced_at) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(tool, transcript_path) DO UPDATE SET last_line=excluded.last_line, prefix_hash=excluded.prefix_hash, last_synced_at=excluded.last_synced_at",
            rusqlite::params![w.tool, w.transcript_path, w.last_line, w.prefix_hash, w.last_synced_at],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn zero_hit_queries(&self, limit: i64) -> Result<Vec<ZeroHitQuery>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare(
            "SELECT query, COUNT(*) AS hits, MAX(created_at) AS last_seen FROM queries WHERE zero_hit = 1 GROUP BY query ORDER BY last_seen DESC LIMIT ?1"
        ).map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt.query_map([limit], |row| {
            Ok(ZeroHitQuery {
                query: row.get("query")?,
                hits: row.get("hits")?,
                last_seen: row.get("last_seen")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn upsert_entity(&self, id: &str, name: &str, entity_type: &str, source_doc: &str) -> Result<()> {
        let conn = self.writer_conn();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, first_seen, source_doc) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(name) DO UPDATE SET source_doc=excluded.source_doc",
            rusqlite::params![id, name, entity_type, now, source_doc],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn list_entities(&self) -> Result<Vec<crate::services::kg::Entity>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare("SELECT * FROM entities ORDER BY name").map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::services::kg::Entity {
                id: row.get("id")?,
                name: row.get("name")?,
                entity_type: row.get("entity_type")?,
                summary: row.get("summary")?,
                first_seen: row.get("first_seen")?,
                source_doc: row.get("source_doc")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn insert_session(&self, s: &crate::services::sessions::Session) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO wiki_sessions (id, agent_name, user_id, created_at) VALUES (?1,?2,?3,?4)",
            rusqlite::params![s.id, s.agent_name, s.user_id, s.created_at],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn get_session(&self, id: &str) -> Result<Option<crate::services::sessions::Session>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare("SELECT * FROM wiki_sessions WHERE id = ?1").map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt.query([id]).map_err(|e| Error::Store(e.to_string()))?;
        let session = match rows.next().map_err(|e| Error::Store(e.to_string()))? {
            Some(row) => {
                let s = (|| -> rusqlite::Result<crate::services::sessions::Session> {
                    Ok(crate::services::sessions::Session {
                        id: row.get("id")?,
                        agent_name: row.get("agent_name")?,
                        user_id: row.get("user_id")?,
                        created_at: row.get("created_at")?,
                        context_summary: row.get("context_summary")?,
                    })
                })()
                .map_err(|e| Error::Store(e.to_string()))?;
                Some(s)
            }
            None => None,
        };
        Ok(session)
    }

    async fn upsert_relation(&self, r: &crate::services::kg::RelationRecord) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "INSERT INTO relation_edges (id, src_entity, dst_entity, relation_type, fact, source_doc, valid_at, invalid_at, expired_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(id) DO UPDATE SET expired_at=excluded.expired_at",
            rusqlite::params![r.id, r.src_entity, r.dst_entity, r.relation_type, r.fact, r.source_doc, r.valid_at, r.invalid_at, r.expired_at],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn get_relations_for_entity(&self, entity_id: &str, limit: i64) -> Result<Vec<crate::services::kg::RelationRecord>> {
        let conn = self.writer_conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM relation_edges WHERE (src_entity = ?1 OR dst_entity = ?1) AND expired_at IS NULL LIMIT ?2"
        ).map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt.query_map(rusqlite::params![entity_id, limit], |row| {
            Ok(crate::services::kg::RelationRecord {
                id: row.get("id")?,
                src_entity: row.get("src_entity")?,
                dst_entity: row.get("dst_entity")?,
                relation_type: row.get("relation_type")?,
                fact: row.get("fact")?,
                source_doc: row.get("source_doc")?,
                valid_at: row.get("valid_at")?,
                invalid_at: row.get("invalid_at")?,
                expired_at: row.get("expired_at")?,
            })
        }).map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| Error::Store(e.to_string()))
    }

    async fn invalidate_relations_for_entity(&self, entity_path: &str) -> Result<()> {
        let conn = self.writer_conn();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE relation_edges SET expired_at = ?1 WHERE source_doc = ?2",
            rusqlite::params![now, entity_path],
        ).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn replace_chunk_distilled(&self, chunk_id: &str, distilled: Option<Distilled>) -> Result<()> {
        let conn = self.writer_conn();
        conn.execute(
            "UPDATE chunks SET distilled = ?1 WHERE id = ?2",
            rusqlite::params![distilled.map(|d| serde_json::to_string(&d).unwrap()), chunk_id],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> SqliteStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.migrate().await.unwrap();
        std::mem::forget(dir);
        store
    }

    #[tokio::test]
    async fn document_revisions_roundtrip_and_monotonic_seq() {
        let store = make_store().await;
        let s1 = store.insert_revision("wiki/a.md", "hash1", "body one", "human:test", "create").await.unwrap();
        let s2 = store.insert_revision("wiki/a.md", "hash2", "body two", "agent-x/wikillm-api", "update").await.unwrap();
        assert_eq!((s1, s2), (1, 2));
        // seq is monotonic per rel_path, not globally
        assert_eq!(store.insert_revision("wiki/b.md", "h", "x", "s", "create").await.unwrap(), 1);

        let listed = store.list_revisions("wiki/a.md", 10).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].seq, 2, "newest first");
        assert_eq!(listed[0].operation, "update");
        assert!(listed.iter().all(|r| r.body.is_empty()), "list is metadata-only");

        let full = store.get_revision("wiki/a.md", 1).await.unwrap().unwrap();
        assert_eq!(full.body, "body one");
        let by_hash = store.get_revision_by_hash("wiki/a.md", "hash2").await.unwrap().unwrap();
        assert_eq!(by_hash.seq, 2);
        assert!(store.get_revision("wiki/a.md", 99).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn watermark_upsert_and_get() {
        let store = make_store().await;
        assert!(store.get_watermark("claude", "/tmp/t.jsonl").await.unwrap().is_none());
        store.upsert_watermark(&TranscriptWatermark {
            tool: "claude".into(),
            transcript_path: "/tmp/t.jsonl".into(),
            last_line: 10,
            prefix_hash: Some("abc".into()),
            last_synced_at: Some("2026-01-01T00:00:00Z".into()),
        }).await.unwrap();
        let wm = store.get_watermark("claude", "/tmp/t.jsonl").await.unwrap().unwrap();
        assert_eq!(wm.last_line, 10);
        assert_eq!(wm.prefix_hash.as_deref(), Some("abc"));
        // Upsert updates in place (same PK).
        store.upsert_watermark(&TranscriptWatermark {
            tool: "claude".into(),
            transcript_path: "/tmp/t.jsonl".into(),
            last_line: 25,
            prefix_hash: Some("abc".into()),
            last_synced_at: Some("2026-01-02T00:00:00Z".into()),
        }).await.unwrap();
        assert_eq!(store.get_watermark("claude", "/tmp/t.jsonl").await.unwrap().unwrap().last_line, 25);
    }

    #[tokio::test]
    async fn memory_mutations_record_and_list_newest_first() {
        let store = make_store().await;
        store.record_memory_mutation(&MemoryMutation {
            id: "m1".into(), memory_id: "mem1".into(), action: "add".into(),
            old_content: None, new_content: Some("v1".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
        }).await.unwrap();
        store.record_memory_mutation(&MemoryMutation {
            id: "m2".into(), memory_id: "mem1".into(), action: "update".into(),
            old_content: Some("v1".into()), new_content: Some("v2".into()),
            timestamp: "2026-01-02T00:00:00Z".into(),
        }).await.unwrap();
        let history = store.list_memory_mutations("mem1", 10).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].action, "update");
        assert!(store.list_memory_mutations("other-mem", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_memories_matches_paraphrased_terms_via_fts() {
        let store = make_store().await;
        store.insert_memory("u|a|", "semantic", "payment-api processes transactions via Stripe", "h1", None, None, None).await.unwrap();
        store.insert_memory("u|a|", "semantic", "user-database is a PostgreSQL instance in us-east-1", "h2", None, None, None).await.unwrap();

        // Multi-word query whose word ORDER differs from the content —
        // substring LIKE would miss this; term FTS must find it.
        let hits = store.search_memories("u|a|", "Stripe transactions", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("payment-api"));

        // Terms scattered across the sentence.
        let hits = store.search_memories("u|a|", "postgresql east-1 instance", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("PostgreSQL"));

        // update_memory must refresh the FTS row: old terms gone, new found.
        let id = hits[0].id.clone();
        store.update_memory(&id, "now serving clickhouse analytics", "h3").await.unwrap();
        let stale = store.search_memories("u|a|", "PostgreSQL", 5).await.unwrap();
        assert!(stale.iter().all(|m| m.id != id), "stale terms must not match after update");
        let fresh = store.search_memories("u|a|", "clickhouse analytics", 5).await.unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].id, id);

        // delete_memory removes the FTS row too.
        store.delete_memory(&id).await.unwrap();
        let gone = store.search_memories("u|a|", "clickhouse", 5).await.unwrap();
        assert!(gone.is_empty());
    }

    #[tokio::test]
    async fn search_memories_escapes_wildcards_bumps_and_orders() {
        let store = make_store().await;
        store.insert_memory("u|a|", "semantic", "100% done_thing", "h1", None, None, None).await.unwrap();
        store.insert_memory("u|a|", "semantic", "plain note", "h2", Some("sess-1"), Some("/w/a.md"), Some(true)).await.unwrap();

        // Literal `%`/`_` in the query must not act as LIKE wildcards.
        let hits = store.search_memories("u|a|", "100% done_", 10).await.unwrap();
        assert_eq!(hits.len(), 1, "escaped wildcards match literally");
        assert_eq!(hits[0].content, "100% done_thing");
        // Returned rows reflect post-bump state (bump happens in the same call).
        assert_eq!(hits[0].access_count, 1, "row reflects its own access bump");

        // Access bump side-effect + pass-through columns.
        let bumped = store.search_memories("u|a|", "done", 10).await.unwrap();
        assert_eq!(bumped.len(), 1);
        let again = store.search_memories("u|a|", "done", 10).await.unwrap();
        assert_eq!(again[0].access_count, 3, "two prior searches + this one");

        // Pass-through provenance columns ride on the second row.
        let plain = store.search_memories("u|a|", "plain", 10).await.unwrap();
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].source_session_id.as_deref(), Some("sess-1"));
        assert_eq!(plain[0].source_ref.as_deref(), Some("/w/a.md"));

        // Under FTS, % and _ are inert characters rather than wildcards:
        // the query degrades to its alphanumeric terms and still matches
        // literally. (Parameterized SQL + fts_query sanitization means no
        // injection surface either way.)
        let literal = store.search_memories("u|a|", "100\\%", 10).await.unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].content, "100% done_thing");
    }

    #[tokio::test]
    async fn zero_hit_queries_aggregate_counts() {
        let store = make_store().await;
        for q in ["ghost query", "ghost query", "another ghost"] {
            store.record_query(&QueryRecord {
                id: ulid::Ulid::new().to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                query: q.into(),
                mode: "hybrid".into(),
                project: None,
                latency_ms: 1.0,
                result_count: 0,
                zero_hit: true,
                top_paths: vec![],
                source: None,
                error: None,
            }).await.unwrap();
        }
        let gaps = store.zero_hit_queries(10).await.unwrap();
        assert_eq!(gaps.len(), 2);
        let ghost = gaps.iter().find(|g| g.query == "ghost query").unwrap();
        assert_eq!(ghost.hits, 2);
    }
}
