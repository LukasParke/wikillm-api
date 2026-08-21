//! Postgres + pgvector backend: tokio-postgres Client behind a Mutex.
//! Port of the TypeScript `src/store/pg.ts` pgSchemaStatements + queries.

use crate::domain::*;
use crate::error::{Error, Result};
use crate::store::{fts_query, Store};
use async_trait::async_trait;
use pgvector::Vector;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::NoTls;

pub struct PostgresStore {
    client: Arc<Mutex<tokio_postgres::Client>>,
}

impl PostgresStore {
    pub async fn connect(url: &str) -> Self {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .expect("postgres connect");
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("postgres connection error: {e}");
            }
        });
        Self { client: Arc::new(Mutex::new(client)) }
    }

    async fn query(&self, sql: &str, params: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<Vec<tokio_postgres::Row>> {
        let client = self.client.lock().await;
        client.query(sql, params).await.map_err(|e| Error::Store(e.to_string()))
    }

    async fn execute(&self, sql: &str, params: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<u64> {
        let client = self.client.lock().await;
        client.execute(sql, params).await.map_err(|e| Error::Store(e.to_string()))
    }

    fn row_to_document(row: &tokio_postgres::Row) -> DocumentRecord {
        DocumentRecord {
            id: row.get("id"),
            rel_path: row.get("rel_path"),
            kind: DocKind::from_str(row.get::<_, String>("kind").as_str()).unwrap_or(DocKind::Page),
            origin: row.get("origin"),
            title: row.get("title"),
            summary: row.get("summary"),
            body: row.get::<_, Option<String>>("body").unwrap_or_default(),
            frontmatter: row.get::<_, Option<serde_json::Value>>("frontmatter").unwrap_or(serde_json::json!({})),
            word_count: row.get::<_, Option<i64>>("word_count").unwrap_or(0),
            outgoing_links: row.get::<_, Option<serde_json::Value>>("outgoing_links").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            hash: row.get("hash"),
            mtime: row.get::<_, Option<i64>>("mtime").unwrap_or(0),
            content_type: row.get("content_type"),
            okf_type: row.get("okf_type"),
            tags: row.get::<_, Option<serde_json::Value>>("tags").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            status: row.get("status"),
            stale_after: row.get("stale_after"),
            resource: row.get("resource"),
            generated_by: row.get("generated_by"),
            generated_at: row.get("generated_at"),
            verified: row.get::<_, Option<serde_json::Value>>("verified").and_then(|v| serde_json::from_value(v).ok()),
            provenance: row.get::<_, Option<serde_json::Value>>("provenance").and_then(|v| serde_json::from_value(v).ok()),
            updated_at: row.get("updated_at"),
            updated_by: row.get("updated_by"),
            indexed_at: row.get("indexed_at"),
        }
    }

    fn row_to_chunk(row: &tokio_postgres::Row, rel_path: Option<String>) -> ChunkRecord {
        ChunkRecord {
            id: row.get("id"),
            document_id: row.get("document_id"),
            ordinal: row.get::<_, Option<i64>>("ordinal").unwrap_or(0),
            heading_path: row.get("heading_path"),
            content: row.get::<_, Option<String>>("content").unwrap_or_default(),
            distilled: row.get::<_, Option<serde_json::Value>>("distilled").and_then(|v| serde_json::from_value(v).ok()),
            embedded_at: row.get("embedded_at"),
            embed_model: row.get("embed_model"),
            rel_path: rel_path.unwrap_or_default(),
        }
    }

    fn row_to_hit(row: &tokio_postgres::Row) -> ChunkHit {
        ChunkHit {
            chunk_id: row.get("chunk_id"),
            document_id: row.get("document_id"),
            rel_path: row.get("rel_path"),
            kind: row.get("kind"),
            origin: row.get("origin"),
            title: row.get("title"),
            okf_type: row.get("okf_type"),
            tags: row.get::<_, Option<serde_json::Value>>("tags").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            status: row.get("status"),
            stale_after: row.get("stale_after"),
            verified: row.get::<_, Option<serde_json::Value>>("verified").and_then(|v| serde_json::from_value(v).ok()),
            hash: row.get("hash"),
            mtime: row.get::<_, Option<i64>>("mtime").unwrap_or(0),
            heading_path: row.get("heading_path"),
            content: row.get("content"),
            score: row.get::<_, Option<f64>>("score").unwrap_or(0.0),
        }
    }
}

fn build_filter_clause(filters: Option<&SearchFilters>, params: &mut Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>>, base: usize) -> String {
    let Some(f) = filters else { return String::new() };
    let mut conds: Vec<String> = Vec::new();
    let push = |params: &mut Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>>, v: Box<dyn tokio_postgres::types::ToSql + Sync + Send>| -> String {
        params.push(v);
        format!("${}", base + params.len())
    };
    if let Some(kinds) = &f.kinds {
        if !kinds.is_empty() {
            let placeholders: Vec<String> = kinds.iter().map(|k| push(params, Box::new(k.clone()))).collect();
            conds.push(format!("kind IN ({})", placeholders.join(",")));
        }
    }
    if let Some(origins) = &f.origins {
        if !origins.is_empty() {
            let placeholders: Vec<String> = origins.iter().map(|o| push(params, Box::new(o.clone()))).collect();
            conds.push(format!("origin IN ({})", placeholders.join(",")));
        }
    }
    if let Some(types) = &f.okf_types {
        if !types.is_empty() {
            let placeholders: Vec<String> = types.iter().map(|t| push(params, Box::new(t.clone()))).collect();
            conds.push(format!("okf_type IN ({})", placeholders.join(",")));
        }
    }
    for tag in f.tags.clone().unwrap_or_default() {
        let p = push(params, Box::new(tag.clone()));
        conds.push(format!("tags @> '{p}'::jsonb"));
    }
    if let Some(statuses) = &f.statuses {
        if !statuses.is_empty() {
            let placeholders: Vec<String> = statuses.iter().map(|s| push(params, Box::new(s.clone()))).collect();
            conds.push(format!("status IN ({})", placeholders.join(",")));
        }
    }
    if let Some(trust) = &f.trust_min {
        if let Some((_, min)) = TRUST_ORDER.iter().find(|(k, _)| k == trust) {
            if *min >= 1 {
                conds.push("verified IS NOT NULL AND jsonb_array_length(verified) > 0".into());
            }
            if *min >= 2 {
                conds.push("EXISTS (SELECT 1 FROM jsonb_array_elements_text(verified) v WHERE v LIKE 'human:%')".into());
            }
        }
    }
    if f.fresh_only.unwrap_or(false) {
        let p = push(params, Box::new(chrono::Utc::now().to_rfc3339()));
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
                    push(params, Box::new(format!("{p}"))),
                    push(params, Box::new(format!("{p}/%"))),
                ]
            })
            .collect();
        conds.push(format!("({})", parts.join(" OR ")));
    }
    if conds.is_empty() {
        String::new()
    } else {
        format!(" AND {}", conds.join(" AND "))
    }
}

fn vector_literal(v: &[f32]) -> String {
    format!("[{}]", v.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","))
}

#[async_trait]
impl Store for PostgresStore {
    fn backend(&self) -> &'static str {
        "postgres"
    }

    async fn migrate(&self) -> Result<()> {
        let dims = std::env::var("EMBEDDING_DIMS").ok().and_then(|d| d.parse::<i32>().ok()).unwrap_or(1536);
        let ddl: &[&str] = &[
            "CREATE EXTENSION IF NOT EXISTS vector",
            "CREATE TABLE IF NOT EXISTS migrations (id INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, source TEXT NOT NULL, action TEXT NOT NULL, paths JSONB NOT NULL, metadata JSONB, parent_id TEXT REFERENCES operations(id) ON DELETE SET NULL)",
            "CREATE INDEX IF NOT EXISTS idx_operations_created_at ON operations(created_at)",
            "CREATE TABLE IF NOT EXISTS changes (id TEXT PRIMARY KEY, detected_at TEXT NOT NULL, rel_path TEXT NOT NULL, change_type TEXT NOT NULL, old_hash TEXT, new_hash TEXT, source TEXT, operation_id TEXT)",
            "CREATE INDEX IF NOT EXISTS idx_changes_path ON changes(rel_path)",
            "CREATE INDEX IF NOT EXISTS idx_changes_detected ON changes(detected_at)",
            "CREATE TABLE IF NOT EXISTS documents (id TEXT PRIMARY KEY, rel_path TEXT NOT NULL UNIQUE, kind TEXT NOT NULL DEFAULT 'page', origin TEXT NOT NULL DEFAULT 'wiki', title TEXT, summary TEXT, body TEXT NOT NULL DEFAULT '', frontmatter JSONB NOT NULL DEFAULT '{}', word_count INTEGER NOT NULL DEFAULT 0, outgoing_links JSONB NOT NULL DEFAULT '[]', hash TEXT NOT NULL, mtime BIGINT NOT NULL, content_type TEXT, okf_type TEXT, tags JSONB NOT NULL DEFAULT '[]', status TEXT, stale_after TEXT, resource TEXT, generated_by TEXT, generated_at TEXT, verified JSONB, provenance JSONB, updated_at TEXT, updated_by TEXT, indexed_at TEXT NOT NULL)",
            "CREATE INDEX IF NOT EXISTS idx_documents_kind ON documents(kind)",
            "CREATE INDEX IF NOT EXISTS idx_documents_origin ON documents(origin)",
            "CREATE INDEX IF NOT EXISTS idx_documents_okf_type ON documents(okf_type)",
            &format!("CREATE TABLE IF NOT EXISTS chunks (id TEXT PRIMARY KEY, document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE, ordinal INTEGER NOT NULL, heading_path TEXT, content TEXT NOT NULL, distilled JSONB, embedded_at TEXT, embed_model TEXT, tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', coalesce(heading_path,'') || ' ' || content)) STORED, UNIQUE(document_id, ordinal))"),
            "CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id)",
            "CREATE INDEX IF NOT EXISTS idx_chunks_tsv ON chunks USING GIN (tsv)",
            &format!("CREATE TABLE IF NOT EXISTS embeddings (chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE, embedding vector({dims}), model TEXT NOT NULL, created_at TEXT NOT NULL)"),
            "CREATE INDEX IF NOT EXISTS idx_embeddings_hnsw ON embeddings USING hnsw (embedding vector_cosine_ops)",
            "CREATE TABLE IF NOT EXISTS edges (src TEXT NOT NULL, dst TEXT NOT NULL, PRIMARY KEY (src, dst))",
            "CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst)",
            "CREATE TABLE IF NOT EXISTS connectors (id TEXT PRIMARY KEY, kind TEXT NOT NULL, config JSONB NOT NULL DEFAULT '{}', enabled BOOLEAN NOT NULL DEFAULT TRUE, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS connector_state (connector_id TEXT PRIMARY KEY REFERENCES connectors(id) ON DELETE CASCADE, watermark JSONB, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS projects (name TEXT PRIMARY KEY, description TEXT, prefixes JSONB NOT NULL DEFAULT '[\"*\"]', connectors JSONB NOT NULL DEFAULT '[]', created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS queries (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, query TEXT NOT NULL, mode TEXT NOT NULL, project TEXT, latency_ms REAL NOT NULL DEFAULT 0, result_count INTEGER NOT NULL DEFAULT 0, zero_hit BOOLEAN NOT NULL DEFAULT FALSE, top_paths JSONB NOT NULL DEFAULT '[]', source TEXT, error TEXT)",
            "CREATE INDEX IF NOT EXISTS idx_queries_created ON queries(created_at)",
            "CREATE TABLE IF NOT EXISTS feedback (id TEXT PRIMARY KEY, query_id TEXT NOT NULL, helpful BOOLEAN NOT NULL, comment TEXT, created_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS webhooks (id TEXT PRIMARY KEY, url TEXT NOT NULL, events JSONB NOT NULL DEFAULT '[\"change\"]', prefixes JSONB NOT NULL DEFAULT '[\"*\"]', enabled BOOLEAN NOT NULL DEFAULT TRUE, last_status TEXT, last_attempt_at TEXT, created_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value JSONB NOT NULL, updated_at TEXT NOT NULL, updated_by TEXT)",
            "CREATE TABLE IF NOT EXISTS api_keys (name TEXT PRIMARY KEY, key_hash TEXT NOT NULL UNIQUE, key_prefix TEXT NOT NULL, scope JSONB NOT NULL DEFAULT '[\"*\"]', role TEXT NOT NULL DEFAULT 'write', created_at TEXT NOT NULL, updated_at TEXT NOT NULL, created_by TEXT)",
        ];
        for stmt in ddl {
            self.execute(stmt, &[]).await?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    async fn upsert_document(&self, doc: &DocumentInput) -> Result<()> {
        let fm = doc.frontmatter.clone();
        let links = serde_json::to_value(&doc.outgoing_links).unwrap_or_default();
        let tags = serde_json::to_value(&doc.tags).unwrap_or_default();
        let verified = doc.verified.as_ref().map(|v| serde_json::to_value(v).unwrap_or_default());
        let provenance = doc.provenance.as_ref().map(|v| serde_json::to_value(v).unwrap_or_default());
        let now = chrono::Utc::now().to_rfc3339();
        self.execute(
            "INSERT INTO documents (id, rel_path, kind, origin, title, summary, body, frontmatter, word_count, outgoing_links, hash, mtime, content_type, okf_type, tags, status, stale_after, resource, generated_by, generated_at, verified, provenance, updated_at, updated_by, indexed_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)
             ON CONFLICT(rel_path) DO UPDATE SET kind=$3, origin=$4, title=$5, summary=$6, body=$7, frontmatter=$8, word_count=$9, outgoing_links=$10, hash=$11, mtime=$12, content_type=$13, okf_type=$14, tags=$15, status=$16, stale_after=$17, resource=$18, generated_by=$19, generated_at=$20, verified=$21, provenance=$22, updated_at=$23, updated_by=$24, indexed_at=$25",
            &[
                &ulid::Ulid::new().to_string(), &doc.rel_path, &doc.kind.as_str(), &doc.origin,
                &doc.title, &doc.summary, &doc.body, &fm, &doc.word_count, &links,
                &doc.hash, &doc.mtime, &doc.content_type, &doc.okf_type, &tags,
                &doc.status, &doc.stale_after, &doc.resource, &doc.generated_by,
                &doc.generated_at, &verified, &provenance, &doc.updated_at, &doc.updated_by, &now,
            ],
        ).await?;
        Ok(())
    }

    async fn get_document(&self, rel_path: &str) -> Result<Option<DocumentRecord>> {
        let rows = self.query("SELECT * FROM documents WHERE rel_path = $1", &[&rel_path]).await?;
        Ok(rows.first().map(Self::row_to_document))
    }

    async fn delete_document(&self, rel_path: &str) -> Result<()> {
        self.execute("DELETE FROM documents WHERE rel_path = $1", &[&rel_path]).await?;
        Ok(())
    }

    async fn list_documents(&self, opts: &ListOptions, limit: i64, cursor: Option<&str>) -> Result<PageList<DocumentRecord>> {
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(folder) = &opts.folder {
            conds.push(format!("rel_path LIKE ${}", params.len() + 1));
            params.push(Box::new(format!("{folder}/%")));
        }
        if let Some(kind) = &opts.kind {
            conds.push(format!("kind = ${}", params.len() + 1));
            params.push(Box::new(kind.as_str().to_string()));
        }
        if let Some(origin) = &opts.origin {
            conds.push(format!("origin = ${}", params.len() + 1));
            params.push(Box::new(origin.clone()));
        }
        if let Some(c) = cursor {
            conds.push(format!("rel_path > ${}", params.len() + 1));
            params.push(Box::new(c.to_string()));
        }
        let base = params.len();
        let fragment = build_filter_clause(opts.filters.as_ref(), &mut params, base);
        if !fragment.is_empty() {
            conds.push(fragment.trim_start_matches(" AND ").to_string());
        }
        params.push(Box::new(limit + 1));
        let where_clause = if conds.is_empty() { String::new() } else { format!(" WHERE {}", conds.join(" AND ")) };
        let sql = format!("SELECT * FROM documents{where_clause} ORDER BY rel_path LIMIT ${}", params.len());
        let rows = self.query(&sql, &params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>()).await?;
        let mut items: Vec<DocumentRecord> = rows.iter().map(Self::row_to_document).collect();
        let next_cursor = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|d| d.rel_path.clone())
        } else {
            None
        };
        Ok(PageList { items, next_cursor })
    }

    async fn count_documents(&self, origin: Option<&str>) -> Result<i64> {
        match origin {
            Some(o) => {
                let origin_str: &str = o;
                let origin_ref: &(dyn tokio_postgres::types::ToSql + Sync + Send) = &origin_str;
                let rows = self.query("SELECT COUNT(*)::int AS n FROM documents WHERE origin = $1", &[origin_ref]).await?;
                Ok(rows.first().map(|r| r.get::<_, i64>(0)).unwrap_or(0))
            }
            None => {
                let rows = self.query("SELECT COUNT(*)::int AS n FROM documents", &[]).await?;
                Ok(rows.first().map(|r| r.get::<_, i64>(0)).unwrap_or(0))
            }
        }
    }

    async fn replace_chunks(&self, document_id: &str, chunks: &[ChunkInput]) -> Result<()> {
        let mut client = self.client.lock().await;
        let tx = client.transaction().await.map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM embeddings WHERE chunk_id IN (SELECT id FROM chunks WHERE document_id = $1)", &[&document_id]).await.map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM chunks WHERE document_id = $1", &[&document_id]).await.map_err(|e| Error::Store(e.to_string()))?;
        for ch in chunks {
            let distilled = ch.distilled.as_ref().map(|d| serde_json::to_value(d).unwrap_or_default());
            tx.execute(
                "INSERT INTO chunks (id, document_id, ordinal, heading_path, content, distilled) VALUES ($1,$2,$3,$4,$5,$6)",
                &[&ulid::Ulid::new().to_string(), &document_id, &ch.ordinal, &ch.heading_path, &ch.content, &distilled],
            ).await.map_err(|e| Error::Store(e.to_string()))?;
        }
        tx.commit().await.map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    async fn get_chunks_for_document(&self, document_id: &str) -> Result<Vec<ChunkRecord>> {
        let rows = self.query(
            "SELECT * FROM chunks WHERE document_id = $1 ORDER BY ordinal",
            &[&document_id],
        ).await?;
        Ok(rows.iter().map(|r| Self::row_to_chunk(r, None)).collect())
    }

    async fn get_chunk(&self, chunk_id: &str) -> Result<Option<ChunkRecord>> {
        let rows = self.query("SELECT * FROM chunks WHERE id = $1", &[&chunk_id]).await?;
        Ok(rows.first().map(|r| Self::row_to_chunk(r, None)))
    }

    async fn upsert_embeddings(&self, items: &[(String, Vec<f32>)], model: &str, embedded_at: &str) -> Result<()> {
        for (chunk_id, vector) in items {
            let vec_literal = vector_literal(vector);
            self.execute(
                "INSERT INTO embeddings (chunk_id, embedding, model, created_at) VALUES ($1, $2::vector, $3, $4)
                 ON CONFLICT(chunk_id) DO UPDATE SET embedding = $2::vector, model = $3, created_at = $4",
                &[&chunk_id, &vec_literal, &model, &embedded_at],
            ).await?;
            self.execute(
                "UPDATE chunks SET embedded_at = $1, embed_model = $2 WHERE id = $3",
                &[&embedded_at, &model, &chunk_id],
            ).await?;
        }
        Ok(())
    }

    async fn list_unembedded_chunks(&self, limit: i64) -> Result<Vec<ChunkRecord>> {
        let rows = self.query(
            "SELECT c.*, d.rel_path FROM chunks c JOIN documents d ON d.id = c.document_id WHERE c.embedded_at IS NULL ORDER BY d.indexed_at, c.ordinal LIMIT $1",
            &[&limit],
        ).await?;
        Ok(rows.iter().map(|r| {
            let rel: String = r.get("rel_path");
            Self::row_to_chunk(r, Some(rel))
        }).collect())
    }

    async fn reset_embeddings(&self, dims: Option<i32>) -> Result<()> {
        self.execute("DELETE FROM embeddings", &[]).await?;
        self.execute("UPDATE chunks SET embedded_at = NULL, embed_model = NULL", &[]).await?;
        if let Some(dims) = dims {
            self.execute(&format!("ALTER TABLE embeddings ALTER COLUMN embedding TYPE vector({dims})"), &[]).await?;
        }
        Ok(())
    }

    async fn replace_edges(&self, src: &str, dsts: &[String]) -> Result<()> {
        self.execute("DELETE FROM edges WHERE src = $1", &[&src]).await?;
        for dst in dsts {
            self.execute("INSERT INTO edges (src, dst) VALUES ($1, $2) ON CONFLICT DO NOTHING", &[&src, dst]).await?;
        }
        Ok(())
    }

    async fn backlinks(&self, rel_path: &str, limit: i64) -> Result<Vec<String>> {
        let rows = self.query("SELECT src FROM edges WHERE dst = $1 LIMIT $2", &[&rel_path, &limit]).await?;
        Ok(rows.iter().map(|r| r.get::<_, String>("src")).collect())
    }

    async fn search_fts(&self, q: &str, limit: i64, filters: Option<&SearchFilters>) -> Result<Vec<ChunkHit>> {
        let match_expr = fts_query(q);
        if match_expr.is_empty() {
            return Ok(Vec::new());
        }
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = vec![Box::new(match_expr)];
        let fragment = build_filter_clause(filters, &mut params, 1);
        params.push(Box::new(limit));
        let sql = format!(
            "SELECT c.id AS chunk_id, c.document_id AS document_id, c.heading_path AS heading_path, c.content AS content, d.rel_path, d.kind, d.origin, d.title, d.okf_type, d.tags, d.status, d.stale_after, d.verified, d.hash, d.mtime, ts_rank(c.tsv, query) AS score FROM chunks c JOIN documents d ON d.id = c.document_id, websearch_to_tsquery('english', $1) query WHERE c.tsv @@ query{fragment} ORDER BY score DESC LIMIT ${}",
            params.len()
        );
        let rows = self.query(&sql, &params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>()).await?;
        Ok(rows.iter().map(Self::row_to_hit).collect())
    }

    async fn search_vector(&self, vector: &[f32], limit: i64, filters: Option<&SearchFilters>) -> Result<Vec<ChunkHit>> {
        let vec_literal = vector_literal(vector);
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = vec![Box::new(vec_literal)];
        let fragment = build_filter_clause(filters, &mut params, 1);
        params.push(Box::new(limit));
        let sql = format!(
            "SELECT c.id AS chunk_id, c.document_id AS document_id, c.heading_path AS heading_path, c.content AS content, d.rel_path, d.kind, d.origin, d.title, d.okf_type, d.tags, d.status, d.stale_after, d.verified, d.hash, d.mtime, 1 - (e.embedding <=> $1::vector) AS score FROM embeddings e JOIN chunks c ON c.id = e.chunk_id JOIN documents d ON d.id = c.document_id WHERE TRUE{fragment} ORDER BY e.embedding <=> $1::vector LIMIT ${}",
            params.len() + 1
        );
        let rows = self.query(&sql, &params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>()).await?;
        Ok(rows.iter().map(Self::row_to_hit).collect())
    }

    fn supports_vector(&self) -> bool {
        true
    }

    async fn insert_operation(&self, op: &Operation) -> Result<()> {
        let paths = serde_json::to_value(&op.paths).unwrap_or_default();
        let metadata = op.metadata.clone();
        self.execute(
            "INSERT INTO operations (id, created_at, source, action, paths, metadata, parent_id) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            &[&op.id, &op.created_at, &op.source, &op.action, &paths, &metadata, &op.parent_id],
        ).await?;
        Ok(())
    }

    async fn get_operation(&self, id: &str) -> Result<Option<Operation>> {
        let rows = self.query("SELECT * FROM operations WHERE id = $1", &[&id]).await?;
        let row = match rows.first() {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(Operation {
            id: row.get("id"),
            created_at: row.get("created_at"),
            source: row.get("source"),
            action: row.get("action"),
            paths: row.get::<_, Option<serde_json::Value>>("paths").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            metadata: row.get("metadata"),
            parent_id: row.get("parent_id"),
        }))
    }

    async fn insert_change(&self, change: &ChangeEventData) -> Result<()> {
        self.execute(
            "INSERT INTO changes (id, detected_at, rel_path, change_type, old_hash, new_hash, source, operation_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            &[&change.id, &change.detected_at, &change.rel_path, &change.change_type, &change.old_hash, &change.new_hash, &change.source, &change.operation_id],
        ).await?;
        Ok(())
    }

    async fn list_changes(&self, since: Option<&str>, path: Option<&str>, source: Option<&str>, limit: i64) -> Result<Vec<ChangeEventData>> {
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(s) = since {
            conds.push(format!("detected_at > ${}", params.len() + 1));
            params.push(Box::new(s.to_string()));
        }
        if let Some(p) = path {
            conds.push(format!("rel_path = ${}", params.len() + 1));
            params.push(Box::new(p.to_string()));
        }
        if let Some(s) = source {
            conds.push(format!("source = ${}", params.len() + 1));
            params.push(Box::new(s.to_string()));
        }
        params.push(Box::new(limit));
        let where_clause = if conds.is_empty() { String::new() } else { format!(" WHERE {}", conds.join(" AND ")) };
        let sql = format!("SELECT * FROM changes{where_clause} ORDER BY detected_at DESC LIMIT ${}", params.len());
        let rows = self.query(&sql, &params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect::<Vec<_>>()).await?;
        Ok(rows.iter().map(|row| ChangeEventData {
            id: row.get("id"),
            rel_path: row.get("rel_path"),
            change_type: row.get("change_type"),
            old_hash: row.get("old_hash"),
            new_hash: row.get("new_hash"),
            source: row.get("source"),
            operation_id: row.get("operation_id"),
            detected_at: row.get("detected_at"),
        }).collect())
    }

    async fn put_connector(&self, c: &ConnectorConfig) -> Result<()> {
        let config = serde_json::to_value(&c.config).unwrap_or_default();
        self.execute(
            "INSERT INTO connectors (id, kind, config, enabled, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT(id) DO UPDATE SET kind=$2, config=$3, enabled=$4, updated_at=$6",
            &[&c.id, &c.kind, &config, &c.enabled, &c.created_at, &c.updated_at],
        ).await?;
        Ok(())
    }

    async fn get_connector(&self, id: &str) -> Result<Option<ConnectorConfig>> {
        let rows = self.query("SELECT * FROM connectors WHERE id = $1", &[&id]).await?;
        let row = match rows.first() {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(ConnectorConfig {
            id: row.get("id"),
            kind: row.get("kind"),
            config: row.get::<_, Option<serde_json::Value>>("config").unwrap_or(serde_json::json!({})),
            enabled: row.get("enabled"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }))
    }

    async fn list_connectors(&self) -> Result<Vec<ConnectorConfig>> {
        let rows = self.query("SELECT * FROM connectors ORDER BY id", &[]).await?;
        Ok(rows.iter().map(|row| ConnectorConfig {
            id: row.get("id"),
            kind: row.get("kind"),
            config: row.get::<_, Option<serde_json::Value>>("config").unwrap_or(serde_json::json!({})),
            enabled: row.get("enabled"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }).collect())
    }

    async fn delete_connector(&self, id: &str) -> Result<bool> {
        let n = self.execute("DELETE FROM connectors WHERE id = $1", &[&id]).await?;
        Ok(n > 0)
    }

    async fn get_connector_state(&self, id: &str) -> Result<Option<Value>> {
        let rows = self.query("SELECT watermark FROM connector_state WHERE connector_id = $1", &[&id]).await?;
        Ok(rows.first().and_then(|r| r.get("watermark")))
    }

    async fn set_connector_state(&self, id: &str, watermark: &Value) -> Result<()> {
        let wm = watermark.clone();
        self.execute(
            "INSERT INTO connector_state (connector_id, watermark, updated_at) VALUES ($1,$2,$3)
             ON CONFLICT(connector_id) DO UPDATE SET watermark=$2, updated_at=$3",
            &[&id, &wm, &chrono::Utc::now().to_rfc3339()],
        ).await?;
        Ok(())
    }

    async fn put_project(&self, p: &ProjectInput) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let prefixes = serde_json::to_value(&p.prefixes).unwrap_or_default();
        let connectors = serde_json::to_value(&p.connectors).unwrap_or_default();
        self.execute(
            "INSERT INTO projects (name, description, prefixes, connectors, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT(name) DO UPDATE SET description=$2, prefixes=$3, connectors=$4, updated_at=$6",
            &[&p.name, &p.description, &prefixes, &connectors, &now, &now],
        ).await?;
        Ok(())
    }

    async fn get_project(&self, name: &str) -> Result<Option<ProjectRecord>> {
        let rows = self.query("SELECT * FROM projects WHERE name = $1", &[&name]).await?;
        let row = match rows.first() {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(ProjectRecord {
            name: row.get("name"),
            description: row.get("description"),
            prefixes: row.get::<_, Option<serde_json::Value>>("prefixes").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| vec!["*".into()]),
            connectors: row.get::<_, Option<serde_json::Value>>("connectors").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }))
    }

    async fn list_projects(&self) -> Result<Vec<ProjectRecord>> {
        let rows = self.query("SELECT * FROM projects ORDER BY name", &[]).await?;
        Ok(rows.iter().map(|row| ProjectRecord {
            name: row.get("name"),
            description: row.get("description"),
            prefixes: row.get::<_, Option<serde_json::Value>>("prefixes").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| vec!["*".into()]),
            connectors: row.get::<_, Option<serde_json::Value>>("connectors").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }).collect())
    }

    async fn delete_project(&self, name: &str) -> Result<bool> {
        let n = self.execute("DELETE FROM projects WHERE name = $1", &[&name]).await?;
        Ok(n > 0)
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>> {
        let rows = self.query("SELECT * FROM api_keys ORDER BY name", &[]).await?;
        Ok(rows.iter().map(|row| ApiKeyRecord {
            name: row.get("name"),
            key_hash: row.get("key_hash"),
            key_prefix: row.get("key_prefix"),
            scope: row.get::<_, Option<serde_json::Value>>("scope").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| vec!["*".into()]),
            role: row.get("role"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            created_by: row.get("created_by"),
        }).collect())
    }

    async fn get_api_key(&self, name: &str) -> Result<Option<ApiKeyRecord>> {
        let all = self.list_api_keys().await?;
        Ok(all.into_iter().find(|k| k.name == name))
    }

    async fn find_api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKeyRecord>> {
        let all = self.list_api_keys().await?;
        Ok(all.into_iter().find(|k| k.key_hash == key_hash))
    }

    async fn upsert_api_key(&self, input: &ApiKeyUpsert) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let scope = serde_json::to_value(&input.scope).unwrap_or_default();
        self.execute(
            "INSERT INTO api_keys (name, key_hash, key_prefix, scope, role, created_at, updated_at, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT(name) DO UPDATE SET key_hash=$2, key_prefix=$3, scope=$4, role=$5, updated_at=$7, created_by=$8",
            &[&input.name, &input.key_hash, &input.key_prefix, &scope, &input.role, &now, &now, &input.created_by],
        ).await?;
        Ok(())
    }

    async fn delete_api_key(&self, name: &str) -> Result<bool> {
        let n = self.execute("DELETE FROM api_keys WHERE name = $1", &[&name]).await?;
        Ok(n > 0)
    }

    async fn count_api_keys(&self) -> Result<i64> {
        let rows = self.query("SELECT COUNT(*)::int AS n FROM api_keys", &[]).await?;
        Ok(rows.first().map(|r| r.get::<_, i64>(0)).unwrap_or(0))
    }

    async fn list_webhooks(&self) -> Result<Vec<WebhookRecord>> {
        let rows = self.query("SELECT * FROM webhooks ORDER BY id", &[]).await?;
        Ok(rows.iter().map(|row| WebhookRecord {
            id: row.get("id"),
            url: row.get("url"),
            events: row.get::<_, Option<serde_json::Value>>("events").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| vec!["change".into()]),
            prefixes: row.get::<_, Option<serde_json::Value>>("prefixes").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| vec!["*".into()]),
            enabled: row.get("enabled"),
            last_status: row.get("last_status"),
            last_attempt_at: row.get("last_attempt_at"),
            created_at: row.get("created_at"),
        }).collect())
    }

    async fn get_webhook(&self, id: &str) -> Result<Option<WebhookRecord>> {
        let all = self.list_webhooks().await?;
        Ok(all.into_iter().find(|w| w.id == id))
    }

    async fn put_webhook(&self, w: &WebhookRecord) -> Result<()> {
        let events = serde_json::to_value(&w.events).unwrap_or_default();
        let prefixes = serde_json::to_value(&w.prefixes).unwrap_or_default();
        self.execute(
            "INSERT INTO webhooks (id, url, events, prefixes, enabled, created_at) VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT(id) DO UPDATE SET url=$2, events=$3, prefixes=$4, enabled=$5",
            &[&w.id, &w.url, &events, &prefixes, &w.enabled, &w.created_at],
        ).await?;
        Ok(())
    }

    async fn delete_webhook(&self, id: &str) -> Result<bool> {
        let n = self.execute("DELETE FROM webhooks WHERE id = $1", &[&id]).await?;
        Ok(n > 0)
    }

    async fn record_webhook_attempt(&self, id: &str, status: &str) -> Result<()> {
        self.execute(
            "UPDATE webhooks SET last_status = $1, last_attempt_at = $2 WHERE id = $3",
            &[&status, &chrono::Utc::now().to_rfc3339(), &id],
        ).await?;
        Ok(())
    }

    async fn record_query(&self, q: &QueryRecord) -> Result<()> {
        let top_paths = serde_json::to_value(&q.top_paths).unwrap_or_default();
        self.execute(
            "INSERT INTO queries (id, created_at, query, mode, project, latency_ms, result_count, zero_hit, top_paths, source, error) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[&q.id, &q.created_at, &q.query, &q.mode, &q.project, &q.latency_ms, &q.result_count, &q.zero_hit, &top_paths, &q.source, &q.error],
        ).await?;
        Ok(())
    }

    async fn record_feedback(&self, query_id: &str, helpful: bool, comment: Option<&str>) -> Result<()> {
        self.execute(
            "INSERT INTO feedback (id, query_id, helpful, comment, created_at) VALUES ($1,$2,$3,$4,$5)",
            &[&ulid::Ulid::new().to_string(), &query_id, &helpful, &comment, &chrono::Utc::now().to_rfc3339()],
        ).await?;
        Ok(())
    }

    async fn stats_overview(&self) -> Result<StatsOverview> {
        let docs = self.query("SELECT COUNT(*)::int AS n FROM documents", &[]).await?;
        let chunks = self.query("SELECT COUNT(*)::int AS n FROM chunks", &[]).await?;
        let emb = self.query("SELECT COUNT(*)::int AS n FROM chunks WHERE embedded_at IS NOT NULL", &[]).await?;
        let qs = self.query("SELECT COUNT(*)::int AS n FROM queries", &[]).await?;
        let zero = self.query("SELECT COUNT(*)::int AS n FROM queries WHERE zero_hit", &[]).await?;
        let helpful = self.query("SELECT COUNT(*)::int AS n FROM feedback WHERE helpful", &[]).await?;
        let total = self.query("SELECT COUNT(*)::int AS n FROM feedback", &[]).await?;
        let get = |rows: &Vec<tokio_postgres::Row>| -> i64 { rows.first().map(|r| r.get::<_, i64>(0)).unwrap_or(0) };
        Ok(StatsOverview {
            documents: get(&docs),
            chunks: get(&chunks),
            embedded_chunks: get(&emb),
            queries: get(&qs),
            zero_hit_queries: get(&zero),
            feedback_helpful: get(&helpful),
            feedback_total: get(&total),
        })
    }

    async fn get_settings(&self) -> Result<Value> {
        let rows = self.query("SELECT key, value FROM settings", &[]).await?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let key: String = row.get("key");
            let value: Value = row.get("value");
            map.insert(key, value);
        }
        Ok(Value::Object(map))
    }

    async fn set_setting(&self, key: &str, value: &Value, updated_by: &str) -> Result<()> {
        let v = value.clone();
        self.execute(
            "INSERT INTO settings (key, value, updated_at, updated_by) VALUES ($1,$2,$3,$4)
             ON CONFLICT(key) DO UPDATE SET value=$2, updated_at=$3, updated_by=$4",
            &[&key, &v, &chrono::Utc::now().to_rfc3339(), &updated_by],
        ).await?;
        Ok(())
    }

    async fn delete_setting(&self, key: &str) -> Result<bool> {
        let n = self.execute("DELETE FROM settings WHERE key = $1", &[&key]).await?;
        Ok(n > 0)
    }

    async fn delete_derived_for_origin(&self, origin: &str) -> Result<()> {
        self.execute("DELETE FROM documents WHERE origin = $1", &[&origin]).await?;
        Ok(())
    }

    async fn replace_chunk_distilled(&self, chunk_id: &str, distilled: Option<Distilled>) -> Result<()> {
        let d = distilled.map(|d| serde_json::to_value(&d).unwrap_or_default());
        self.execute("UPDATE chunks SET distilled = $1 WHERE id = $2", &[&d, &chunk_id]).await?;
        Ok(())
    }

    async fn collection_fingerprint(&self, prefix: Option<&str>) -> Result<(i64, i64)> {
        let rows = match prefix {
            Some(p) => self.query(
                "SELECT COUNT(*)::int AS n, COALESCE(MAX(mtime),0)::bigint AS m FROM documents WHERE rel_path LIKE $1",
                &[&format!("{p}/%")],
            ).await?,
            None => self.query(
                "SELECT COUNT(*)::int AS n, COALESCE(MAX(mtime),0)::bigint AS m FROM documents",
                &[],
            ).await?,
        };
        let row = rows.first().ok_or_else(|| Error::Store("fingerprint row missing".into()))?;
        Ok((row.get::<_, i64>(0), row.get::<_, i64>(1)))
    }
}
