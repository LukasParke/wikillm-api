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

CREATE TABLE IF NOT EXISTS page_cache (
  rel_path TEXT PRIMARY KEY,
  abs_path TEXT NOT NULL,
  title TEXT,
  summary TEXT,
  frontmatter TEXT NOT NULL DEFAULT '{}',
  word_count INTEGER NOT NULL DEFAULT 0,
  outgoing_links TEXT NOT NULL DEFAULT '[]',
  hash TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  updated_at TEXT,
  updated_by TEXT
);

CREATE INDEX IF NOT EXISTS idx_page_cache_title ON page_cache(title);

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
