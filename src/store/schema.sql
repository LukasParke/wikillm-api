-- WikiLLM API index store schema (SQLite backend).
-- Everything here is derived cache: deleting the DB file is always safe;
-- the next boot rebuilds documents/chunks from WIKI_ROOT and connectors.

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
  old_hash TEXT,
  new_hash TEXT,
  source TEXT,
  operation_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_changes_path ON changes(rel_path);
CREATE INDEX IF NOT EXISTS idx_changes_detected ON changes(detected_at);

CREATE TABLE IF NOT EXISTS documents (
  id TEXT PRIMARY KEY,
  rel_path TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL DEFAULT 'page',
  origin TEXT NOT NULL DEFAULT 'wiki',
  title TEXT,
  summary TEXT,
  body TEXT NOT NULL DEFAULT '',
  frontmatter TEXT NOT NULL DEFAULT '{}',
  word_count INTEGER NOT NULL DEFAULT 0,
  outgoing_links TEXT NOT NULL DEFAULT '[]',
  hash TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  content_type TEXT,
  okf_type TEXT,
  tags TEXT NOT NULL DEFAULT '[]',
  status TEXT,
  stale_after TEXT,
  resource TEXT,
  generated_by TEXT,
  generated_at TEXT,
  verified TEXT,
  provenance TEXT,
  updated_at TEXT,
  updated_by TEXT,
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
  content,
  heading_path,
  chunk_id UNINDEXED
);

-- Embedding bookkeeping for SQLite. Vector payloads are only searchable on
-- the Postgres backend; SQLite deployments run in FTS-only retrieval mode.
CREATE TABLE IF NOT EXISTS embeddings (
  chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
  model TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edges (
  src TEXT NOT NULL,
  dst TEXT NOT NULL,
  PRIMARY KEY (src, dst)
);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);

CREATE TABLE IF NOT EXISTS connectors (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  config TEXT NOT NULL DEFAULT '{}',
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS connector_state (
  connector_id TEXT PRIMARY KEY REFERENCES connectors(id) ON DELETE CASCADE,
  watermark TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
  name TEXT PRIMARY KEY,
  description TEXT,
  prefixes TEXT NOT NULL DEFAULT '["*"]',
  connectors TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS queries (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  query TEXT NOT NULL,
  mode TEXT NOT NULL,
  project TEXT,
  latency_ms REAL NOT NULL DEFAULT 0,
  result_count INTEGER NOT NULL DEFAULT 0,
  zero_hit INTEGER NOT NULL DEFAULT 0,
  top_paths TEXT NOT NULL DEFAULT '[]',
  source TEXT,
  error TEXT
);
CREATE INDEX IF NOT EXISTS idx_queries_created ON queries(created_at);

CREATE TABLE IF NOT EXISTS feedback (
  id TEXT PRIMARY KEY,
  query_id TEXT NOT NULL,
  helpful INTEGER NOT NULL,
  comment TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  updated_by TEXT
);

CREATE TABLE IF NOT EXISTS api_keys (
  name TEXT PRIMARY KEY,
  key_hash TEXT NOT NULL UNIQUE,
  key_prefix TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT '["*"]',
  role TEXT NOT NULL DEFAULT 'write',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  created_by TEXT
);
