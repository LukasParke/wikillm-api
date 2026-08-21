import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ulid } from "ulidx";
import type { ChangeEvent, Operation } from "../types/index.js";
import {
  TRUST_ORDER,
  type ChunkHit,
  type ChunkInput,
  type ChunkRecord,
  type ConnectorConfig,
  type Distilled,
  type DocumentInput,
  type DocumentRecord,
  type FeedbackInput,
  type ListOptions,
  type Page,
  type ProjectInput,
  type ProjectRecord,
  type QueryRecord,
  type SearchFilters,
  type StatsOverview,
  type Store,
} from "./types.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------------------
// Low-level driver interface (bun:sqlite / better-sqlite3)
// ---------------------------------------------------------------------------

export type SqlParam = string | number | bigint | boolean | null;

export interface Statement {
  run(...params: SqlParam[]): { changes: number };
  get(...params: SqlParam[]): unknown;
  all(...params: SqlParam[]): unknown[];
}

export interface SqliteDatabase {
  exec(sql: string): void;
  prepare(sql: string): Statement;
  close(): void;
}

export async function createSqliteDatabase(
  dbPath: string,
): Promise<SqliteDatabase> {
  if (typeof Bun !== "undefined") {
    // bun:sqlite only exists under the Bun runtime; Node falls through to
    // better-sqlite3 below, so this specifier cannot be imported statically.
    const { Database: BunDatabase } = await import("bun:sqlite");
    const bdb = new BunDatabase(dbPath);
    return {
      exec: (sql) => bdb.exec(sql),
      prepare: (sql) => {
        const stmt = bdb.query(sql);
        return {
          run: (...params) => {
            const result = stmt.run(...params);
            return { changes: Number(result.changes) };
          },
          get: (...params) => stmt.get(...params),
          all: (...params) => stmt.all(...params),
        };
      },
      close: () => bdb.close(),
    };
  }
  const BetterSqlite3 = createRequire(import.meta.url)(
    "better-sqlite3",
  ) as new (path: string) => {
    exec(sql: string): void;
    prepare(sql: string): Statement;
    close(): void;
  };
  return new BetterSqlite3(dbPath);
}

// ---------------------------------------------------------------------------
// Row / value helpers
// ---------------------------------------------------------------------------

type Row = Record<string, unknown>;

function asRow(value: unknown): Row {
  return (value ?? {}) as Row;
}

function s(v: unknown): string | null {
  if (v === null || v === undefined) return null;
  return typeof v === "string" ? v : String(v);
}

function num(v: unknown): number {
  return Number(v ?? 0);
}

function bool(v: unknown): boolean {
  return v === 1 || v === true || v === "1";
}

function json<T>(v: unknown, fallback: T): T {
  const raw = s(v);
  if (raw === null || raw === "") return fallback;
  try {
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

function jstr(value: unknown): string | null {
  return value === undefined || value === null ? null : JSON.stringify(value);
}

/** Escape a user query into safe FTS5 terms joined with OR (recall-first). */
export function ftsQuery(q: string): string {
  const terms = q
    .split(/\s+/)
    .map((t) => t.replace(/["'()*:^]/g, " ").trim())
    .filter((t) => t.length > 0)
    .slice(0, 12);
  if (terms.length === 0) return "";
  return terms.map((t) => `"${t}"`).join(" OR ");
}

const HIT_COLUMNS = `
  c.id AS chunk_id,
  c.document_id AS document_id,
  c.heading_path AS heading_path,
  c.content AS content,
  d.rel_path AS rel_path,
  d.kind AS kind,
  d.origin AS origin,
  d.title AS title,
  d.okf_type AS okf_type,
  d.tags AS tags,
  d.status AS status,
  d.stale_after AS stale_after,
  d.verified AS verified,
  d.hash AS hash,
  d.mtime AS mtime
`;

function rowToHit(row: Row): ChunkHit {
  return {
    chunk_id: s(row.chunk_id)!,
    document_id: s(row.document_id)!,
    rel_path: s(row.rel_path)!,
    kind: s(row.kind)!,
    origin: s(row.origin)!,
    title: s(row.title),
    okf_type: s(row.okf_type),
    tags: json<string[]>(row.tags, []),
    status: s(row.status),
    stale_after: s(row.stale_after),
    verified: json(row.verified, null),
    hash: s(row.hash)!,
    mtime: num(row.mtime),
    heading_path: s(row.heading_path),
    content: s(row.content) ?? "",
    score: num(row.score),
  };
}

/**
 * Shared WHERE fragment for search filters. Applies to queries that join
 * `documents d`. Returns [fragmentSql, params].
 */
function filterClause(filters?: SearchFilters): [string, SqlParam[]] {
  if (!filters) return ["", []];
  const conds: string[] = [];
  const params: SqlParam[] = [];
  if (filters.kinds && filters.kinds.length > 0) {
    conds.push(`d.kind IN (${filters.kinds.map(() => "?").join(",")})`);
    params.push(...filters.kinds);
  }
  if (filters.origins && filters.origins.length > 0) {
    conds.push(`d.origin IN (${filters.origins.map(() => "?").join(",")})`);
    params.push(...filters.origins);
  }
  if (filters.okf_types && filters.okf_types.length > 0) {
    conds.push(`d.okf_type IN (${filters.okf_types.map(() => "?").join(",")})`);
    params.push(...filters.okf_types);
  }
  for (const tag of filters.tags ?? []) {
    // tags column holds a JSON array; match the quoted element
    conds.push(`d.tags LIKE ?`);
    params.push(`%"${tag}"%`);
  }
  if (filters.statuses && filters.statuses.length > 0) {
    conds.push(`d.status IN (${filters.statuses.map(() => "?").join(",")})`);
    params.push(...filters.statuses);
  }
  if (filters.trustMin && TRUST_ORDER[filters.trustMin] >= 1) {
    conds.push(`d.verified IS NOT NULL AND d.verified != '[]'`);
    if (TRUST_ORDER[filters.trustMin] >= 2) {
      conds.push(`d.verified LIKE '%"human:%'`);
    }
  }
  if (filters.freshOnly) {
    conds.push(`(d.stale_after IS NULL OR d.stale_after > ?)`);
    params.push(new Date().toISOString());
  }
  const prefixes = (filters.pathPrefixes ?? ["*"]).filter((p) => p !== "*");
  if (prefixes.length > 0) {
    conds.push(
      prefixes
        .map(() => `(d.rel_path = ? OR d.rel_path LIKE ? OR d.rel_path LIKE ?)`)
        .join(" OR "),
    );
    for (const p of prefixes) {
      params.push(p, `${p}/%`, `${p}/%/%`);
    }
  }
  return [conds.length ? ` AND ${conds.join(" AND ")}` : "", params];
}

// ---------------------------------------------------------------------------
// SqliteStore
// ---------------------------------------------------------------------------

export class SqliteStore implements Store {
  readonly backend = "sqlite" as const;

  constructor(private db: SqliteDatabase) {}

  migrate(): Promise<void> {
    const schema = readFileSync(path.join(__dirname, "schema.sql"), "utf8");
    this.db.exec(schema);
    this.db
      .prepare(
        "INSERT OR IGNORE INTO migrations (id, applied_at) VALUES (?, ?)",
      )
      .run(2, new Date().toISOString());
    return Promise.resolve();
  }

  close(): Promise<void> {
    this.db.close();
    return Promise.resolve();
  }

  // -- Documents ------------------------------------------------------------

  upsertDocument(doc: DocumentInput): Promise<void> {
    const existing = this.db
      .prepare("SELECT id FROM documents WHERE rel_path = ?")
      .get(doc.rel_path) as Row | undefined;
    const id = existing ? s(existing.id)! : ulid();
    this.db
      .prepare(
        `INSERT INTO documents
         (id, rel_path, kind, origin, title, summary, body, frontmatter, word_count,
          outgoing_links, hash, mtime, content_type, okf_type, tags, status, stale_after,
          resource, generated_by, generated_at, verified, provenance, updated_at, updated_by, indexed_at)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
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
           indexed_at=excluded.indexed_at`,
      )
      .run(
        id,
        doc.rel_path,
        doc.kind,
        doc.origin,
        doc.title ?? null,
        doc.summary ?? null,
        doc.body ?? "",
        jstr(doc.frontmatter ?? {}) ?? "{}",
        doc.word_count ?? 0,
        jstr(doc.outgoing_links ?? []) ?? "[]",
        doc.hash,
        doc.mtime,
        doc.content_type ?? null,
        doc.okf_type ?? null,
        jstr(doc.tags ?? []) ?? "[]",
        doc.status ?? null,
        doc.stale_after ?? null,
        doc.resource ?? null,
        doc.generated_by ?? null,
        doc.generated_at ?? null,
        jstr(doc.verified ?? null),
        jstr(doc.provenance ?? null),
        doc.updated_at ?? null,
        doc.updated_by ?? null,
        new Date().toISOString(),
      );
    return Promise.resolve();
  }

  getDocument(relPath: string): Promise<DocumentRecord | null> {
    const row = this.db
      .prepare("SELECT * FROM documents WHERE rel_path = ?")
      .get(relPath);
    return Promise.resolve(row ? rowToDocument(asRow(row)) : null);
  }

  deleteDocument(relPath: string): Promise<void> {
    this.db
      .prepare(
        `DELETE FROM chunks_fts WHERE chunk_id IN (
           SELECT c.id FROM chunks c JOIN documents d ON d.id = c.document_id
           WHERE d.rel_path = ?)`,
      )
      .run(relPath);
    this.db.prepare("DELETE FROM documents WHERE rel_path = ?").run(relPath);
    return Promise.resolve();
  }

  listDocuments(opts: ListOptions): Promise<Page<DocumentRecord>> {
    const folder = opts.folder ?? "";
    const limit = opts.limit ?? 50;
    let sql = "SELECT * FROM documents WHERE 1=1";
    const params: SqlParam[] = [];
    if (folder) {
      sql += " AND rel_path LIKE ?";
      params.push(`${folder}/%`);
    }
    if (opts.kind) {
      sql += " AND kind = ?";
      params.push(opts.kind);
    }
    if (opts.origin) {
      sql += " AND origin = ?";
      params.push(opts.origin);
    }
    if (opts.cursor) {
      sql += " AND rel_path > ?";
      params.push(opts.cursor);
    }
    sql += " ORDER BY rel_path LIMIT ?";
    params.push(limit + 1);
    const rows = this.db.prepare(sql).all(...params) as unknown[];
    const hasMore = rows.length > limit;
    const items = rows.slice(0, limit).map((r) => rowToDocument(asRow(r)));
    return Promise.resolve({
      items,
      nextCursor: hasMore ? items[items.length - 1]?.rel_path : undefined,
    });
  }

  countDocuments(opts?: { origin?: string }): Promise<number> {
    let sql = "SELECT COUNT(*) AS n FROM documents";
    const params: SqlParam[] = [];
    if (opts?.origin) {
      sql += " WHERE origin = ?";
      params.push(opts.origin);
    }
    const row = asRow(this.db.prepare(sql).get(...params));
    return Promise.resolve(num(row.n));
  }

  // -- Chunks ---------------------------------------------------------------

  replaceChunks(documentId: string, chunks: ChunkInput[]): Promise<void> {
    this.db.exec("BEGIN");
    try {
      this.db
        .prepare(
          `DELETE FROM chunks_fts WHERE chunk_id IN (SELECT id FROM chunks WHERE document_id = ?)`,
        )
        .run(documentId);
      this.db
        .prepare("DELETE FROM chunks WHERE document_id = ?")
        .run(documentId);
      const insert = this.db.prepare(
        `INSERT INTO chunks (id, document_id, ordinal, heading_path, content, distilled)
         VALUES (?,?,?,?,?,?)`,
      );
      const insertFts = this.db.prepare(
        `INSERT INTO chunks_fts (content, heading_path, chunk_id) VALUES (?,?,?)`,
      );
      for (const ch of chunks) {
        const cid = ulid();
        insert.run(
          cid,
          documentId,
          ch.ordinal,
          ch.heading_path ?? null,
          ch.content,
          jstr(ch.distilled ?? null),
        );
        insertFts.run(ch.content, ch.heading_path ?? "", cid);
      }
      this.db
        .prepare(
          `DELETE FROM embeddings WHERE chunk_id NOT IN (SELECT id FROM chunks)`,
        )
        .run();
      this.db.exec("COMMIT");
    } catch (err) {
      this.db.exec("ROLLBACK");
      throw err;
    }
    return Promise.resolve();
  }

  getChunksForDocument(documentId: string): Promise<ChunkRecord[]> {
    const rows = this.db
      .prepare("SELECT * FROM chunks WHERE document_id = ? ORDER BY ordinal")
      .all(documentId) as unknown[];
    return Promise.resolve(rows.map((r) => rowToChunk(asRow(r))));
  }

  getChunk(chunkId: string): Promise<ChunkRecord | null> {
    const row = this.db
      .prepare("SELECT * FROM chunks WHERE id = ?")
      .get(chunkId);
    return Promise.resolve(row ? rowToChunk(asRow(row)) : null);
  }

  upsertEmbeddings(
    items: Array<{ chunkId: string; vector: number[] }>,
    model: string,
    embeddedAt: string,
  ): Promise<void> {
    // SQLite has no searchable vector payload; record bookkeeping only.
    const update = this.db.prepare(
      "UPDATE chunks SET embedded_at = ?, embed_model = ? WHERE id = ?",
    );
    for (const item of items) update.run(embeddedAt, model, item.chunkId);
    return Promise.resolve();
  }

  setChunkDistilled(
    chunkId: string,
    distilled: Distilled | null,
  ): Promise<void> {
    this.db
      .prepare("UPDATE chunks SET distilled = ? WHERE id = ?")
      .run(distilled ? JSON.stringify(distilled) : null, chunkId);
    return Promise.resolve();
  }

  listUnembeddedChunks(
    limit: number,
  ): Promise<Array<ChunkRecord & { rel_path: string }>> {
    const rows = this.db
      .prepare(
        `SELECT c.*, d.rel_path AS rel_path FROM chunks c
         JOIN documents d ON d.id = c.document_id
         WHERE c.embedded_at IS NULL
         ORDER BY d.indexed_at, c.ordinal LIMIT ?`,
      )
      .all(limit) as unknown[];
    return Promise.resolve(
      rows.map((r) => {
        const row = asRow(r);
        return { ...rowToChunk(row), rel_path: s(row.rel_path)! };
      }),
    );
  }

  // -- Edges ----------------------------------------------------------------

  replaceEdges(srcRelPath: string, dstRelPaths: string[]): Promise<void> {
    this.db.exec("BEGIN");
    try {
      this.db.prepare("DELETE FROM edges WHERE src = ?").run(srcRelPath);
      const ins = this.db.prepare(
        "INSERT OR IGNORE INTO edges (src, dst) VALUES (?,?)",
      );
      for (const dst of new Set(dstRelPaths)) ins.run(srcRelPath, dst);
      this.db.exec("COMMIT");
    } catch (err) {
      this.db.exec("ROLLBACK");
      throw err;
    }
    return Promise.resolve();
  }

  backlinks(relPath: string, limit = 100): Promise<string[]> {
    const rows = this.db
      .prepare("SELECT src FROM edges WHERE dst = ? LIMIT ?")
      .all(relPath, limit) as unknown[];
    return Promise.resolve(rows.map((r) => s(asRow(r).src)!));
  }

  // -- Retrieval primitives ---------------------------------------------------

  supportsVector(): boolean {
    return false;
  }

  searchFts(
    q: string,
    opts: { limit: number; filters?: SearchFilters },
  ): Promise<ChunkHit[]> {
    const match = ftsQuery(q);
    if (!match) return Promise.resolve([]);
    const [where, fParams] = filterClause(opts.filters);
    const sql = `
      SELECT ${HIT_COLUMNS}, -bm25(chunks_fts) AS score
      FROM chunks_fts
      JOIN chunks c ON c.id = chunks_fts.chunk_id
      JOIN documents d ON d.id = c.document_id
      WHERE chunks_fts MATCH ?${where}
      ORDER BY score DESC
      LIMIT ?`;
    const rows = this.db
      .prepare(sql)
      .all(match, ...fParams, opts.limit) as unknown[];
    return Promise.resolve(rows.map((r) => rowToHit(asRow(r))));
  }

  searchVector(): Promise<ChunkHit[]> {
    return Promise.resolve([]);
  }

  // -- Operations / changes ---------------------------------------------------

  insertOperation(op: Operation): Promise<void> {
    this.db
      .prepare(
        "INSERT INTO operations (id, created_at, source, action, paths, metadata, parent_id) VALUES (?,?,?,?,?,?,?)",
      )
      .run(
        op.id,
        op.created_at,
        op.source,
        op.action,
        JSON.stringify(op.paths),
        op.metadata ? JSON.stringify(op.metadata) : null,
        op.parent_id,
      );
    return Promise.resolve();
  }

  getOperation(id: string): Promise<Operation | null> {
    const row = this.db
      .prepare("SELECT * FROM operations WHERE id = ?")
      .get(id);
    if (!row) return Promise.resolve(null);
    const r = asRow(row);
    return Promise.resolve({
      id: s(r.id)!,
      created_at: s(r.created_at)!,
      source: s(r.source)!,
      action: s(r.action)!,
      paths: json<string[]>(r.paths, []),
      metadata: json(r.metadata, null),
      parent_id: s(r.parent_id),
    });
  }

  insertChange(change: ChangeEvent["data"]): Promise<void> {
    this.db
      .prepare(
        "INSERT INTO changes (id, detected_at, rel_path, change_type, old_hash, new_hash, source, operation_id) VALUES (?,?,?,?,?,?,?,?)",
      )
      .run(
        change.id,
        change.detected_at,
        change.rel_path,
        change.change_type,
        change.old_hash,
        change.new_hash,
        change.source,
        change.operation_id,
      );
    return Promise.resolve();
  }

  listChanges(opts: {
    since?: string;
    path?: string;
    source?: string;
    limit?: number;
  }): Promise<ChangeEvent["data"][]> {
    const conditions: string[] = [];
    const params: SqlParam[] = [];
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
    const rows = this.db.prepare(sql).all(...params) as unknown[];
    return Promise.resolve(
      rows.map((r) => {
        const row = asRow(r);
        return {
          id: s(row.id)!,
          rel_path: s(row.rel_path)!,
          change_type: s(
            row.change_type,
          )! as ChangeEvent["data"]["change_type"],
          old_hash: s(row.old_hash),
          new_hash: s(row.new_hash),
          source: s(row.source) as ChangeEvent["data"]["source"],
          operation_id: s(row.operation_id),
          detected_at: s(row.detected_at)!,
        };
      }),
    );
  }

  // -- Connectors -------------------------------------------------------------

  putConnector(c: ConnectorConfig): Promise<void> {
    this.db
      .prepare(
        `INSERT INTO connectors (id, kind, config, enabled, created_at, updated_at)
         VALUES (?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, config=excluded.config,
           enabled=excluded.enabled, updated_at=excluded.updated_at`,
      )
      .run(
        c.id,
        c.kind,
        jstr(c.config)!,
        c.enabled ? 1 : 0,
        c.created_at,
        c.updated_at,
      );
    return Promise.resolve();
  }

  getConnector(id: string): Promise<ConnectorConfig | null> {
    const row = this.db
      .prepare("SELECT * FROM connectors WHERE id = ?")
      .get(id);
    return Promise.resolve(row ? rowToConnector(asRow(row)) : null);
  }

  listConnectors(): Promise<ConnectorConfig[]> {
    const rows = this.db
      .prepare("SELECT * FROM connectors ORDER BY id")
      .all() as unknown[];
    return Promise.resolve(rows.map((r) => rowToConnector(asRow(r))));
  }

  deleteConnector(id: string): Promise<boolean> {
    const res = this.db.prepare("DELETE FROM connectors WHERE id = ?").run(id);
    return Promise.resolve(res.changes > 0);
  }

  getConnectorState(id: string): Promise<unknown | null> {
    const row = this.db
      .prepare("SELECT watermark FROM connector_state WHERE connector_id = ?")
      .get(id);
    if (!row) return Promise.resolve(null);
    return Promise.resolve(json(asRow(row).watermark, null));
  }

  setConnectorState(id: string, watermark: unknown): Promise<void> {
    this.db
      .prepare(
        `INSERT INTO connector_state (connector_id, watermark, updated_at) VALUES (?,?,?)
         ON CONFLICT(connector_id) DO UPDATE SET watermark=excluded.watermark, updated_at=excluded.updated_at`,
      )
      .run(id, jstr(watermark)!, new Date().toISOString());
    return Promise.resolve();
  }

  // -- Projects ---------------------------------------------------------------

  putProject(p: ProjectInput): Promise<void> {
    const now = new Date().toISOString();
    this.db
      .prepare(
        `INSERT INTO projects (name, description, prefixes, connectors, created_at, updated_at)
         VALUES (?,?,?,?,?,?)
         ON CONFLICT(name) DO UPDATE SET description=excluded.description,
           prefixes=excluded.prefixes, connectors=excluded.connectors, updated_at=excluded.updated_at`,
      )
      .run(
        p.name,
        p.description ?? null,
        jstr(p.prefixes)!,
        jstr(p.connectors ?? [])!,
        now,
        now,
      );
    return Promise.resolve();
  }

  getProject(name: string): Promise<ProjectRecord | null> {
    const row = this.db
      .prepare("SELECT * FROM projects WHERE name = ?")
      .get(name);
    return Promise.resolve(row ? rowToProject(asRow(row)) : null);
  }

  listProjects(): Promise<ProjectRecord[]> {
    const rows = this.db
      .prepare("SELECT * FROM projects ORDER BY name")
      .all() as unknown[];
    return Promise.resolve(rows.map((r) => rowToProject(asRow(r))));
  }

  deleteProject(name: string): Promise<boolean> {
    const res = this.db
      .prepare("DELETE FROM projects WHERE name = ?")
      .run(name);
    return Promise.resolve(res.changes > 0);
  }

  // -- Analytics ---------------------------------------------------------------

  recordQuery(q: QueryRecord): Promise<void> {
    this.db
      .prepare(
        `INSERT INTO queries (id, created_at, query, mode, project, latency_ms, result_count, zero_hit, top_paths, source, error)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)`,
      )
      .run(
        q.id,
        q.created_at,
        q.query,
        q.mode,
        q.project,
        q.latency_ms,
        q.result_count,
        q.zero_hit ? 1 : 0,
        jstr(q.top_paths)!,
        q.source,
        q.error,
      );
    return Promise.resolve();
  }

  recordFeedback(f: FeedbackInput): Promise<void> {
    this.db
      .prepare(
        "INSERT INTO feedback (id, query_id, helpful, comment, created_at) VALUES (?,?,?,?,?)",
      )
      .run(
        ulid(),
        f.query_id,
        f.helpful ? 1 : 0,
        f.comment ?? null,
        new Date().toISOString(),
      );
    return Promise.resolve();
  }

  statsOverview(): Promise<StatsOverview> {
    const count = (sql: string): number =>
      num(asRow(this.db.prepare(sql).get()).n);
    return Promise.resolve({
      documents: count("SELECT COUNT(*) AS n FROM documents"),
      chunks: count("SELECT COUNT(*) AS n FROM chunks"),
      embedded_chunks: count(
        "SELECT COUNT(*) AS n FROM chunks WHERE embedded_at IS NOT NULL",
      ),
      queries: count("SELECT COUNT(*) AS n FROM queries"),
      zero_hit_queries: count(
        "SELECT COUNT(*) AS n FROM queries WHERE zero_hit = 1",
      ),
      feedback_helpful: count(
        "SELECT COUNT(*) AS n FROM feedback WHERE helpful = 1",
      ),
      feedback_total: count("SELECT COUNT(*) AS n FROM feedback"),
    });
  }

  // -- Maintenance --------------------------------------------------------------

  deleteDerivedForOrigin(origin: string): Promise<void> {
    this.db
      .prepare(
        `DELETE FROM chunks_fts WHERE chunk_id IN (
           SELECT c.id FROM chunks c JOIN documents d ON d.id = c.document_id WHERE d.origin = ?)`,
      )
      .run(origin);
    this.db.prepare("DELETE FROM documents WHERE origin = ?").run(origin);
    return Promise.resolve();
  }
}

// ---------------------------------------------------------------------------
// Row mappers
// ---------------------------------------------------------------------------

function rowToDocument(row: Row): DocumentRecord {
  return {
    id: s(row.id)!,
    rel_path: s(row.rel_path)!,
    kind: s(row.kind) as DocumentRecord["kind"],
    origin: s(row.origin)!,
    title: s(row.title),
    summary: s(row.summary),
    body: s(row.body) ?? "",
    frontmatter: json(row.frontmatter, {}),
    word_count: num(row.word_count),
    outgoing_links: json<string[]>(row.outgoing_links, []),
    hash: s(row.hash)!,
    mtime: num(row.mtime),
    content_type: s(row.content_type),
    okf_type: s(row.okf_type),
    tags: json<string[]>(row.tags, []),
    status: s(row.status),
    stale_after: s(row.stale_after),
    resource: s(row.resource),
    generated_by: s(row.generated_by),
    generated_at: s(row.generated_at),
    verified: json(row.verified, null),
    provenance: json(row.provenance, null),
    updated_at: s(row.updated_at),
    updated_by: s(row.updated_by),
    indexed_at: s(row.indexed_at)!,
  };
}

function rowToChunk(row: Row): ChunkRecord {
  return {
    id: s(row.id)!,
    document_id: s(row.document_id)!,
    ordinal: num(row.ordinal),
    heading_path: s(row.heading_path),
    content: s(row.content) ?? "",
    distilled: json(row.distilled, null),
    embedded_at: s(row.embedded_at),
    embed_model: s(row.embed_model),
  };
}

function rowToConnector(row: Row): ConnectorConfig {
  return {
    id: s(row.id)!,
    kind: s(row.kind)!,
    config: json(row.config, {}),
    enabled: bool(row.enabled),
    created_at: s(row.created_at)!,
    updated_at: s(row.updated_at)!,
  };
}

function rowToProject(row: Row): ProjectRecord {
  return {
    name: s(row.name)!,
    description: s(row.description),
    prefixes: json<string[]>(row.prefixes, ["*"]),
    connectors: json<string[]>(row.connectors, []),
    created_at: s(row.created_at)!,
    updated_at: s(row.updated_at)!,
  };
}
