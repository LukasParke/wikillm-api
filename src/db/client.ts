import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { ChangeEvent, Operation, Page } from "../types/index.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export interface Database {
  exec(sql: string): void;
  prepare(sql: string): Statement;
  close(): void;
}

export interface Statement {
  run(...params: unknown[]): {
    changes: number;
    lastInsertRowid: number | bigint;
  };
  get(...params: unknown[]): unknown | undefined;
  all(...params: unknown[]): unknown[];
}

export async function createDatabase(dbPath: string): Promise<Database> {
  let db: Database;
  if (typeof Bun !== "undefined") {
    const { Database: BunDatabase } = await import("bun:sqlite");
    const bdb = new BunDatabase(dbPath);
    db = {
      exec: (sql) => bdb.exec(sql),
      prepare: (sql) => {
        const stmt = bdb.query(sql);
        return {
          run: (...params) => {
            const result = stmt.run(...(params as any[]));
            return {
              changes: Number(result.changes),
              lastInsertRowid: result.lastInsertRowid ?? 0,
            };
          },
          get: (...params) =>
            stmt.get(...(params as any[])) as unknown | undefined,
          all: (...params) => stmt.all(...(params as any[])) as unknown[],
        };
      },
      close: () => bdb.close(),
    };
  } else {
    // Node fallback
    const BetterSqlite3 = require("better-sqlite3");
    const bdb: any = new BetterSqlite3(dbPath);
    db = {
      exec: (sql) => bdb.exec(sql),
      prepare: (sql) => {
        const stmt = bdb.prepare(sql);
        return {
          run: (...params) => stmt.run(...params),
          get: (...params) => stmt.get(...params),
          all: (...params) => stmt.all(...params),
        };
      },
      close: () => bdb.close(),
    };
  }
  return db;
}

export function migrate(db: Database): void {
  const schema = readFileSync(path.join(__dirname, "schema.sql"), "utf8");
  db.exec(schema);
  const stmt = db.prepare(
    "INSERT OR IGNORE INTO migrations (id, applied_at) VALUES (?, ?)",
  );
  stmt.run(1, new Date().toISOString());
}

export function insertOperation(db: Database, op: Operation): void {
  const stmt = db.prepare(
    "INSERT INTO operations (id, created_at, source, action, paths, metadata, parent_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
  );
  stmt.run(
    op.id,
    op.created_at,
    op.source,
    op.action,
    JSON.stringify(op.paths),
    op.metadata ? JSON.stringify(op.metadata) : null,
    op.parent_id,
  );
}

export function upsertPageCache(db: Database, page: Page): void {
  const stmt = db.prepare(
    `INSERT INTO page_cache
     (rel_path, abs_path, title, summary, frontmatter, word_count, outgoing_links, hash, mtime, updated_at, updated_by)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
     ON CONFLICT(rel_path) DO UPDATE SET
       abs_path=excluded.abs_path,
       title=excluded.title,
       summary=excluded.summary,
       frontmatter=excluded.frontmatter,
       word_count=excluded.word_count,
       outgoing_links=excluded.outgoing_links,
       hash=excluded.hash,
       mtime=excluded.mtime,
       updated_at=excluded.updated_at,
       updated_by=excluded.updated_by`,
  );
  stmt.run(
    page.rel_path,
    page.abs_path,
    page.title,
    page.summary,
    JSON.stringify(page.frontmatter),
    page.word_count,
    JSON.stringify(page.outgoing_links),
    page.hash,
    page.mtime,
    page.updated_at,
    page.updated_by,
  );
}

export function deletePageCache(db: Database, relPath: string): void {
  const stmt = db.prepare("DELETE FROM page_cache WHERE rel_path = ?");
  stmt.run(relPath);
}

export function insertChange(db: Database, change: ChangeEvent["data"]): void {
  const stmt = db.prepare(
    "INSERT INTO changes (id, detected_at, rel_path, change_type, old_hash, new_hash, source, operation_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
  );
  stmt.run(
    change.id,
    change.detected_at,
    change.rel_path,
    change.change_type,
    change.old_hash,
    change.new_hash,
    change.source,
    change.operation_id,
  );
}

export function getPageCache(db: Database, relPath: string): Page | undefined {
  const stmt = db.prepare("SELECT * FROM page_cache WHERE rel_path = ?");
  const row = stmt.get(relPath) as any;
  if (!row) return undefined;
  return rowToPage(row);
}

export function listPageCache(
  db: Database,
  opts: { folder?: string; limit?: number; cursor?: string } = {},
): { items: Page[]; nextCursor?: string } {
  const folder = opts.folder ?? "wiki";
  const limit = opts.limit ?? 50;
  let sql = "SELECT * FROM page_cache WHERE rel_path LIKE ?";
  const params: unknown[] = [`${folder}/%`];
  if (opts.cursor) {
    sql += " AND rel_path > ?";
    params.push(opts.cursor);
  }
  sql += " ORDER BY rel_path LIMIT ?";
  params.push(limit + 1);
  const stmt = db.prepare(sql);
  const rows = stmt.all(...params) as any[];
  const hasMore = rows.length > limit;
  const items = rows.slice(0, limit).map(rowToPage);
  return {
    items,
    nextCursor: hasMore ? items[items.length - 1]?.rel_path : undefined,
  };
}

export function searchPageCache(
  db: Database,
  q: string,
  inField?: "title" | "body" | "frontmatter",
  limit = 20,
): Page[] {
  const term = `%${q}%`;
  let sql: string;
  let params: unknown[];
  if (inField === "title") {
    sql =
      "SELECT * FROM page_cache WHERE title LIKE ? ORDER BY rel_path LIMIT ?";
    params = [term, limit];
  } else if (inField === "frontmatter") {
    sql =
      "SELECT * FROM page_cache WHERE frontmatter LIKE ? ORDER BY rel_path LIMIT ?";
    params = [term, limit];
  } else {
    // body search requires reading file; fallback to title + frontmatter
    sql =
      "SELECT * FROM page_cache WHERE title LIKE ? OR frontmatter LIKE ? ORDER BY rel_path LIMIT ?";
    params = [term, term, limit];
  }
  const stmt = db.prepare(sql);
  return (stmt.all(...params) as any[]).map(rowToPage);
}

export function listChanges(
  db: Database,
  opts: { since?: string; path?: string; source?: string; limit?: number } = {},
): ChangeEvent["data"][] {
  const conditions: string[] = [];
  const params: unknown[] = [];
  if (opts.since) {
    conditions.push("detected_at > ?");
    params.push(opts.since);
  }
  if (opts.path) {
    conditions.push("rel_path = ?");
    params.push(opts.path);
  }
  if (opts.source) {
    conditions.push("source = ?");
    params.push(opts.source);
  }
  let sql = "SELECT * FROM changes";
  if (conditions.length) sql += " WHERE " + conditions.join(" AND ");
  sql += " ORDER BY detected_at DESC LIMIT ?";
  params.push(opts.limit ?? 100);
  const stmt = db.prepare(sql);
  return (stmt.all(...params) as any[]).map(rowToChange);
}

function rowToPage(row: any): Page {
  return {
    rel_path: row.rel_path,
    abs_path: row.abs_path,
    title: row.title ?? null,
    summary: row.summary ?? null,
    frontmatter: JSON.parse(row.frontmatter || "{}"),
    body: "", // body is not stored in cache; read from disk when needed
    word_count: row.word_count ?? 0,
    outgoing_links: JSON.parse(row.outgoing_links || "[]"),
    hash: row.hash,
    mtime: row.mtime,
    updated_at: row.updated_at ?? null,
    updated_by: row.updated_by ?? null,
  };
}

function rowToChange(row: any): ChangeEvent["data"] {
  return {
    id: row.id,
    rel_path: row.rel_path,
    change_type: row.change_type,
    old_hash: row.old_hash ?? null,
    new_hash: row.new_hash ?? null,
    source: row.source ?? null,
    operation_id: row.operation_id ?? null,
    detected_at: row.detected_at,
  };
}
