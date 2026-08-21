import { fileURLToPath } from "node:url";
import path from "node:path";
import postgres, { type Sql } from "postgres";
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

type Row = Record<string, unknown>;

// postgres.js serializes params via JSON.stringify, but its JSONValue type
// cannot express `unknown` leaves; persisted values are plain parsed JSON.
function jsonParam(
  sql: { json(value: { toJSON(): string }): postgres.Parameter },
  value: unknown,
): postgres.Parameter {
  return sql.json(value as { toJSON(): string });
}

// Query params are strings/numbers/plain arrays built locally in this module.
function unsafeQuery(
  sql: Sql,
  query: string,
  params: unknown[],
): Promise<Row[]> {
  return sql.unsafe(query, params as never[]) as Promise<Row[]>;
}
function s(v: unknown): string | null {
  if (v === null || v === undefined) return null;
  return typeof v === "string" ? v : String(v);
}

function num(v: unknown): number {
  if (typeof v === "number") return v;
  if (v === null || v === undefined) return 0;
  return Number(v);
}

function json<T>(v: unknown, fallback: T): T {
  if (v === null || v === undefined) return fallback;
  if (typeof v !== "string") return v as T; // jsonb arrives parsed
  try {
    return JSON.parse(v) as T;
  } catch {
    return fallback;
  }
}

/** Postgres DDL, one statement per entry. Vector dims are fixed at migrate time. */
export function pgSchemaStatements(dims: number): string[] {
  return [
    `CREATE TABLE IF NOT EXISTS migrations (
       id INTEGER PRIMARY KEY,
       applied_at TEXT NOT NULL
     )`,
    `CREATE TABLE IF NOT EXISTS operations (
       id TEXT PRIMARY KEY,
       created_at TEXT NOT NULL,
       source TEXT NOT NULL,
       action TEXT NOT NULL,
       paths JSONB NOT NULL,
       metadata JSONB,
       parent_id TEXT REFERENCES operations(id) ON DELETE SET NULL
     )`,
    `CREATE INDEX IF NOT EXISTS idx_operations_created_at ON operations(created_at)`,
    `CREATE INDEX IF NOT EXISTS idx_operations_parent ON operations(parent_id)`,
    `CREATE TABLE IF NOT EXISTS changes (
       id TEXT PRIMARY KEY,
       detected_at TEXT NOT NULL,
       rel_path TEXT NOT NULL,
       change_type TEXT NOT NULL,
       old_hash TEXT,
       new_hash TEXT,
       source TEXT,
       operation_id TEXT
     )`,
    `CREATE INDEX IF NOT EXISTS idx_changes_path ON changes(rel_path)`,
    `CREATE INDEX IF NOT EXISTS idx_changes_detected ON changes(detected_at)`,
    `CREATE TABLE IF NOT EXISTS documents (
       id TEXT PRIMARY KEY,
       rel_path TEXT NOT NULL UNIQUE,
       kind TEXT NOT NULL DEFAULT 'page',
       origin TEXT NOT NULL DEFAULT 'wiki',
       title TEXT,
       summary TEXT,
       body TEXT NOT NULL DEFAULT '',
       frontmatter JSONB NOT NULL DEFAULT '{}',
       word_count INTEGER NOT NULL DEFAULT 0,
       outgoing_links JSONB NOT NULL DEFAULT '[]',
       hash TEXT NOT NULL,
       mtime BIGINT NOT NULL,
       content_type TEXT,
       okf_type TEXT,
       tags JSONB NOT NULL DEFAULT '[]',
       status TEXT,
       stale_after TEXT,
       resource TEXT,
       generated_by TEXT,
       generated_at TEXT,
       verified JSONB,
       provenance JSONB,
       updated_at TEXT,
       updated_by TEXT,
       indexed_at TEXT NOT NULL
     )`,
    `CREATE INDEX IF NOT EXISTS idx_documents_kind ON documents(kind)`,
    `CREATE INDEX IF NOT EXISTS idx_documents_origin ON documents(origin)`,
    `CREATE INDEX IF NOT EXISTS idx_documents_okf_type ON documents(okf_type)`,
    `CREATE TABLE IF NOT EXISTS chunks (
       id TEXT PRIMARY KEY,
       document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
       ordinal INTEGER NOT NULL,
       heading_path TEXT,
       content TEXT NOT NULL,
       distilled JSONB,
       embedded_at TEXT,
       embed_model TEXT,
       tsv tsvector GENERATED ALWAYS AS (
         to_tsvector('english', coalesce(heading_path, '') || ' ' || content)
       ) STORED,
       UNIQUE(document_id, ordinal)
     )`,
    `CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id)`,
    `CREATE INDEX IF NOT EXISTS idx_chunks_tsv ON chunks USING GIN (tsv)`,
    `CREATE TABLE IF NOT EXISTS embeddings (
       chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
       embedding vector(${dims}),
       model TEXT NOT NULL,
       created_at TEXT NOT NULL
     )`,
    `CREATE INDEX IF NOT EXISTS idx_embeddings_hnsw ON embeddings USING hnsw (embedding vector_cosine_ops)`,
    `CREATE TABLE IF NOT EXISTS edges (
       src TEXT NOT NULL,
       dst TEXT NOT NULL,
       PRIMARY KEY (src, dst)
     )`,
    `CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst)`,
    `CREATE TABLE IF NOT EXISTS connectors (
       id TEXT PRIMARY KEY,
       kind TEXT NOT NULL,
       config JSONB NOT NULL DEFAULT '{}',
       enabled BOOLEAN NOT NULL DEFAULT TRUE,
       created_at TEXT NOT NULL,
       updated_at TEXT NOT NULL
     )`,
    `CREATE TABLE IF NOT EXISTS connector_state (
       connector_id TEXT PRIMARY KEY REFERENCES connectors(id) ON DELETE CASCADE,
       watermark JSONB,
       updated_at TEXT NOT NULL
     )`,
    `CREATE TABLE IF NOT EXISTS projects (
       name TEXT PRIMARY KEY,
       description TEXT,
       prefixes JSONB NOT NULL DEFAULT '["*"]',
       connectors JSONB NOT NULL DEFAULT '[]',
       created_at TEXT NOT NULL,
       updated_at TEXT NOT NULL
     )`,
    `CREATE TABLE IF NOT EXISTS queries (
       id TEXT PRIMARY KEY,
       created_at TEXT NOT NULL,
       query TEXT NOT NULL,
       mode TEXT NOT NULL,
       project TEXT,
       latency_ms REAL NOT NULL DEFAULT 0,
       result_count INTEGER NOT NULL DEFAULT 0,
       zero_hit BOOLEAN NOT NULL DEFAULT FALSE,
       top_paths JSONB NOT NULL DEFAULT '[]',
       source TEXT,
       error TEXT
     )`,
    `CREATE INDEX IF NOT EXISTS idx_queries_created ON queries(created_at)`,
    `CREATE TABLE IF NOT EXISTS feedback (
       id TEXT PRIMARY KEY,
       query_id TEXT NOT NULL,
       helpful BOOLEAN NOT NULL,
       comment TEXT,
       created_at TEXT NOT NULL
     )`,
  ];
}

const HIT_COLUMNS = `
  c.id AS chunk_id,
  c.document_id AS document_id,
  c.heading_path AS heading_path,
  c.content AS content,
  d.rel_path, d.kind, d.origin, d.title, d.okf_type,
  d.tags, d.status, d.stale_after, d.verified, d.hash, d.mtime
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

/** Filter fragment for queries joining `documents d`. Returns SQL and params. */
function filterClause(
  filters: SearchFilters | undefined,
  base = 0,
): [string, unknown[]] {
  if (!filters) return ["", []];
  const conds: string[] = [];
  const params: unknown[] = [];

  const nextParam = (): string => `$${base + params.length + 1}`;

  const inListWith = (column: string, values: string[]): string => {
    const parts: string[] = [];
    for (const v of values) {
      const p = nextParam();
      params.push(v);
      parts.push(p);
    }
    return `${column} IN (${parts.join(",")})`;
  };

  if (filters.kinds && filters.kinds.length > 0) {
    conds.push(inListWith("d.kind", filters.kinds));
  }
  if (filters.origins && filters.origins.length > 0) {
    conds.push(inListWith("d.origin", filters.origins));
  }
  if (filters.okf_types && filters.okf_types.length > 0) {
    conds.push(inListWith("d.okf_type", filters.okf_types));
  }
  for (const tag of filters.tags ?? []) {
    // plain-text binding: $n::jsonb casts misbehave under extended protocol
    const p = nextParam();
    conds.push(
      `EXISTS (SELECT 1 FROM jsonb_array_elements_text(d.tags) t WHERE t = ${p})`,
    );
    params.push(tag);
  }
  if (filters.statuses && filters.statuses.length > 0) {
    conds.push(inListWith("d.status", filters.statuses));
  }
  if (filters.trustMin && TRUST_ORDER[filters.trustMin] >= 1) {
    conds.push(
      `d.verified IS NOT NULL AND jsonb_typeof(d.verified) = 'array' AND jsonb_array_length(d.verified) > 0`,
    );
    if (TRUST_ORDER[filters.trustMin] >= 2) {
      conds.push(
        `EXISTS (SELECT 1 FROM jsonb_array_elements(d.verified) v WHERE v->>'by' LIKE 'human:%')`,
      );
    }
  }
  if (filters.freshOnly) {
    const p = nextParam();
    conds.push(`(d.stale_after IS NULL OR d.stale_after > ${p})`);
    params.push(new Date().toISOString());
  }
  const prefixes = (filters.pathPrefixes ?? ["*"]).filter(
    (prefix) => prefix !== "*",
  );
  if (prefixes.length > 0) {
    const parts: string[] = [];
    for (const prefix of prefixes) {
      const eq = nextParam();
      params.push(prefix);
      const like = nextParam();
      params.push(`${prefix}/%`);
      parts.push(`(d.rel_path = ${eq} OR d.rel_path LIKE ${like})`);
    }
    conds.push(`(${parts.join(" OR ")})`);
  }
  return [conds.length ? ` AND ${conds.join(" AND ")}` : "", params];
}

function vectorLiteral(vector: number[]): string {
  return `[${vector.join(",")}]`;
}

export class PostgresStore implements Store {
  readonly backend = "postgres" as const;

  constructor(private sql: Sql) {}

  static async connect(url: string): Promise<PostgresStore> {
    const sql = postgres(url, { max: 10, prepare: false });
    await sql`SELECT 1`;
    return new PostgresStore(sql);
  }

  async migrate(): Promise<void> {
    await this.sql`CREATE EXTENSION IF NOT EXISTS vector`;
    const dims = Number(process.env.EMBEDDING_DIMS ?? 1536);
    for (const stmt of pgSchemaStatements(dims)) {
      await this.sql.unsafe(stmt);
    }
    await this.sql`
      INSERT INTO migrations (id, applied_at) VALUES (2, ${new Date().toISOString()})
      ON CONFLICT (id) DO NOTHING`;
  }

  async close(): Promise<void> {
    await this.sql.end();
  }

  // -- Documents ------------------------------------------------------------

  async upsertDocument(doc: DocumentInput): Promise<void> {
    const now = new Date().toISOString();
    const rows = await this.sql`
      INSERT INTO documents
        (id, rel_path, kind, origin, title, summary, body, frontmatter, word_count,
         outgoing_links, hash, mtime, content_type, okf_type, tags, status, stale_after,
         resource, generated_by, generated_at, verified, provenance, updated_at, updated_by, indexed_at)
      VALUES (${ulid()}, ${doc.rel_path}, ${doc.kind}, ${doc.origin}, ${doc.title ?? null},
        ${doc.summary ?? null}, ${doc.body ?? ""}, ${jsonParam(this.sql, doc.frontmatter ?? {})},
        ${doc.word_count ?? 0}, ${jsonParam(this.sql, doc.outgoing_links ?? [])}, ${doc.hash},
        ${Math.floor(doc.mtime)}, ${doc.content_type ?? null}, ${doc.okf_type ?? null},
        ${jsonParam(this.sql, doc.tags ?? [])}, ${doc.status ?? null}, ${doc.stale_after ?? null},
        ${doc.resource ?? null}, ${doc.generated_by ?? null}, ${doc.generated_at ?? null},
        ${doc.verified === undefined ? null : jsonParam(this.sql, doc.verified)},
        ${doc.provenance === undefined ? null : jsonParam(this.sql, doc.provenance)},
        ${doc.updated_at ?? null}, ${doc.updated_by ?? null}, ${now})
      ON CONFLICT (rel_path) DO UPDATE SET
        kind = EXCLUDED.kind, origin = EXCLUDED.origin, title = EXCLUDED.title,
        summary = EXCLUDED.summary, body = EXCLUDED.body, frontmatter = EXCLUDED.frontmatter,
        word_count = EXCLUDED.word_count, outgoing_links = EXCLUDED.outgoing_links,
        hash = EXCLUDED.hash, mtime = EXCLUDED.mtime, content_type = EXCLUDED.content_type,
        okf_type = EXCLUDED.okf_type, tags = EXCLUDED.tags, status = EXCLUDED.status,
        stale_after = EXCLUDED.stale_after, resource = EXCLUDED.resource,
        generated_by = EXCLUDED.generated_by, generated_at = EXCLUDED.generated_at,
        verified = EXCLUDED.verified, provenance = EXCLUDED.provenance,
        updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by,
        indexed_at = EXCLUDED.indexed_at
      RETURNING id`;
    void rows;
  }

  async getDocument(relPath: string): Promise<DocumentRecord | null> {
    const rows = await this
      .sql`SELECT * FROM documents WHERE rel_path = ${relPath}`;
    return rows.length ? rowToDocument(rows[0] as Row) : null;
  }

  async deleteDocument(relPath: string): Promise<void> {
    await this.sql`DELETE FROM documents WHERE rel_path = ${relPath}`;
  }

  async listDocuments(opts: ListOptions): Promise<Page<DocumentRecord>> {
    const folder = opts.folder ?? "";
    const limit = opts.limit ?? 50;
    const conds: string[] = [];
    const params: unknown[] = [];
    if (folder) {
      conds.push(`rel_path LIKE $${params.length + 1}`);
      params.push(`${folder}/%`);
    }
    if (opts.kind) {
      conds.push(`kind = $${params.length + 1}`);
      params.push(opts.kind);
    }
    if (opts.origin) {
      conds.push(`origin = $${params.length + 1}`);
      params.push(opts.origin);
    }
    if (opts.cursor) {
      conds.push(`rel_path > $${params.length + 1}`);
      params.push(opts.cursor);
    }
    const where = conds.length ? `WHERE ${conds.join(" AND ")}` : "";
    params.push(limit + 1);
    const rows = await unsafeQuery(
      this.sql,
      `SELECT * FROM documents ${where} ORDER BY rel_path LIMIT $${params.length}`,
      params,
    );
    const hasMore = rows.length > limit;
    const items = (rows as Row[]).slice(0, limit).map(rowToDocument);
    return {
      items,
      nextCursor: hasMore ? items[items.length - 1]?.rel_path : undefined,
    };
  }

  async countDocuments(opts?: { origin?: string }): Promise<number> {
    const rows = opts?.origin
      ? await this
          .sql`SELECT COUNT(*)::int AS n FROM documents WHERE origin = ${opts.origin}`
      : await this.sql`SELECT COUNT(*)::int AS n FROM documents`;
    return num((rows[0] as Row).n);
  }

  // -- Chunks ---------------------------------------------------------------

  async replaceChunks(documentId: string, chunks: ChunkInput[]): Promise<void> {
    await this.sql.begin(async (tx) => {
      await tx`DELETE FROM embeddings WHERE chunk_id IN (SELECT id FROM chunks WHERE document_id = ${documentId})`;
      await tx`DELETE FROM chunks WHERE document_id = ${documentId}`;
      for (const ch of chunks) {
        await tx`
          INSERT INTO chunks (id, document_id, ordinal, heading_path, content, distilled)
          VALUES (${ulid()}, ${documentId}, ${ch.ordinal}, ${ch.heading_path ?? null},
                  ${ch.content}, ${ch.distilled ? jsonParam(tx, ch.distilled) : null})`;
      }
    });
  }

  async getChunksForDocument(documentId: string): Promise<ChunkRecord[]> {
    const rows = await this.sql`
      SELECT * FROM chunks WHERE document_id = ${documentId} ORDER BY ordinal`;
    return (rows as Row[]).map(rowToChunk);
  }

  async getChunk(chunkId: string): Promise<ChunkRecord | null> {
    const rows = await this.sql`SELECT * FROM chunks WHERE id = ${chunkId}`;
    return rows.length ? rowToChunk(rows[0] as Row) : null;
  }

  async setChunkDistilled(
    chunkId: string,
    distilled: Distilled | null,
  ): Promise<void> {
    await this
      .sql`UPDATE chunks SET distilled = ${distilled ? jsonParam(this.sql, distilled) : null} WHERE id = ${chunkId}`;
  }

  async listUnembeddedChunks(
    limit: number,
  ): Promise<Array<ChunkRecord & { rel_path: string }>> {
    const rows = await this.sql`
      SELECT c.*, d.rel_path FROM chunks c
      JOIN documents d ON d.id = c.document_id
      WHERE c.embedded_at IS NULL
      ORDER BY d.indexed_at, c.ordinal LIMIT ${limit}`;
    return (rows as Row[]).map((row) => ({
      ...rowToChunk(row),
      rel_path: s(row.rel_path)!,
    }));
  }

  async upsertEmbeddings(
    items: Array<{ chunkId: string; vector: number[] }>,
    model: string,
    embeddedAt: string,
  ): Promise<void> {
    if (items.length === 0) return;
    await this.sql.begin(async (tx) => {
      for (const item of items) {
        const literal = `[${item.vector.join(",")}]`;
        await tx`
          INSERT INTO embeddings (chunk_id, embedding, model, created_at)
          VALUES (${item.chunkId}, ${literal}::vector, ${model}, ${embeddedAt})
          ON CONFLICT (chunk_id) DO UPDATE SET
            embedding = EXCLUDED.embedding, model = EXCLUDED.model, created_at = EXCLUDED.created_at`;
        await tx`UPDATE chunks SET embedded_at = ${embeddedAt}, embed_model = ${model} WHERE id = ${item.chunkId}`;
      }
    });
  }

  // -- Edges ----------------------------------------------------------------

  async replaceEdges(srcRelPath: string, dstRelPaths: string[]): Promise<void> {
    await this.sql.begin(async (tx) => {
      await tx`DELETE FROM edges WHERE src = ${srcRelPath}`;
      for (const dst of new Set(dstRelPaths)) {
        await tx`INSERT INTO edges (src, dst) VALUES (${srcRelPath}, ${dst}) ON CONFLICT DO NOTHING`;
      }
    });
  }

  async backlinks(relPath: string, limit = 100): Promise<string[]> {
    const rows = await this.sql`
      SELECT src FROM edges WHERE dst = ${relPath} LIMIT ${limit}`;
    return (rows as Row[]).map((r) => s(r.src)!);
  }

  // -- Retrieval primitives ---------------------------------------------------

  supportsVector(): boolean {
    return true;
  }

  async searchFts(
    q: string,
    opts: { limit: number; filters?: SearchFilters },
  ): Promise<ChunkHit[]> {
    const [where, params] = filterClause(opts.filters, 1);
    const rows = await unsafeQuery(
      this.sql,
      `SELECT ${HIT_COLUMNS}, ts_rank(c.tsv, query) AS score
       FROM chunks c
       JOIN documents d ON d.id = c.document_id,
            websearch_to_tsquery('english', $1) query
       WHERE c.tsv @@ query${where}
       ORDER BY score DESC
       LIMIT $${params.length + 2}`,
      [q, ...params, opts.limit],
    );
    return (rows as Row[]).map(rowToHit);
  }

  async searchVector(
    vector: number[],
    opts: { limit: number; filters?: SearchFilters },
  ): Promise<ChunkHit[]> {
    const [where, params] = filterClause(opts.filters, 1);
    const rows = await unsafeQuery(
      this.sql,
      `SELECT ${HIT_COLUMNS}, 1 - (e.embedding <=> $1::vector) AS score
       FROM embeddings e
       JOIN chunks c ON c.id = e.chunk_id
       JOIN documents d ON d.id = c.document_id
       WHERE TRUE${where}
       ORDER BY e.embedding <=> $1::vector
       LIMIT $${params.length + 2}`,
      [vectorLiteral(vector), ...params, opts.limit],
    );
    return (rows as Row[]).map(rowToHit);
  }

  // -- Operations / changes ---------------------------------------------------

  async insertOperation(op: Operation): Promise<void> {
    await this.sql`
      INSERT INTO operations (id, created_at, source, action, paths, metadata, parent_id)
      VALUES (${op.id}, ${op.created_at}, ${op.source}, ${op.action},
              ${jsonParam(this.sql, op.paths)}, ${op.metadata ? jsonParam(this.sql, op.metadata) : null},
              ${op.parent_id})`;
  }

  async getOperation(id: string): Promise<Operation | null> {
    const rows = await this.sql`SELECT * FROM operations WHERE id = ${id}`;
    if (!rows.length) return null;
    const row = rows[0] as Row;
    return {
      id: s(row.id)!,
      created_at: s(row.created_at)!,
      source: s(row.source)!,
      action: s(row.action)!,
      paths: json<string[]>(row.paths, []),
      metadata: json(row.metadata, null),
      parent_id: s(row.parent_id),
    };
  }

  async insertChange(change: ChangeEvent["data"]): Promise<void> {
    await this.sql`
      INSERT INTO changes (id, detected_at, rel_path, change_type, old_hash, new_hash, source, operation_id)
      VALUES (${change.id}, ${change.detected_at}, ${change.rel_path}, ${change.change_type},
              ${change.old_hash}, ${change.new_hash}, ${change.source}, ${change.operation_id})`;
  }

  async listChanges(opts: {
    since?: string;
    path?: string;
    source?: string;
    limit?: number;
  }): Promise<ChangeEvent["data"][]> {
    const conds: string[] = [];
    const params: unknown[] = [];
    if (opts.since) {
      conds.push(`detected_at > $${params.length + 1}`);
      params.push(opts.since);
    }
    if (opts.path) {
      conds.push(`rel_path = $${params.length + 1}`);
      params.push(opts.path);
    }
    if (opts.source) {
      conds.push(`source = $${params.length + 1}`);
      params.push(opts.source);
    }
    const where = conds.length ? `WHERE ${conds.join(" AND ")}` : "";
    params.push(opts.limit ?? 100);
    const rows = await unsafeQuery(
      this.sql,
      `SELECT * FROM changes ${where} ORDER BY detected_at DESC LIMIT $${params.length}`,
      params,
    );
    return (rows as Row[]).map((row) => ({
      id: s(row.id)!,
      rel_path: s(row.rel_path)!,
      change_type: s(row.change_type)! as ChangeEvent["data"]["change_type"],
      old_hash: s(row.old_hash),
      new_hash: s(row.new_hash),
      source: s(row.source) as ChangeEvent["data"]["source"],
      operation_id: s(row.operation_id),
      detected_at: s(row.detected_at)!,
    }));
  }

  // -- Connectors -------------------------------------------------------------

  async putConnector(c: ConnectorConfig): Promise<void> {
    await this.sql`
      INSERT INTO connectors (id, kind, config, enabled, created_at, updated_at)
      VALUES (${c.id}, ${c.kind}, ${jsonParam(this.sql, c.config)}, ${c.enabled}, ${c.created_at}, ${c.updated_at})
      ON CONFLICT (id) DO UPDATE SET
        kind = EXCLUDED.kind, config = EXCLUDED.config,
        enabled = EXCLUDED.enabled, updated_at = EXCLUDED.updated_at`;
  }

  async getConnector(id: string): Promise<ConnectorConfig | null> {
    const rows = await this.sql`SELECT * FROM connectors WHERE id = ${id}`;
    return rows.length ? rowToConnector(rows[0] as Row) : null;
  }

  async listConnectors(): Promise<ConnectorConfig[]> {
    const rows = await this.sql`SELECT * FROM connectors ORDER BY id`;
    return (rows as Row[]).map(rowToConnector);
  }

  async deleteConnector(id: string): Promise<boolean> {
    const rows = await this.sql`
      DELETE FROM connectors WHERE id = ${id} RETURNING id`;
    return rows.length > 0;
  }

  async getConnectorState(id: string): Promise<unknown | null> {
    const rows = await this.sql`
      SELECT watermark FROM connector_state WHERE connector_id = ${id}`;
    return rows.length ? json((rows[0] as Row).watermark, null) : null;
  }

  async setConnectorState(id: string, watermark: unknown): Promise<void> {
    await this.sql`
      INSERT INTO connector_state (connector_id, watermark, updated_at)
      VALUES (${id}, ${jsonParam(this.sql, watermark)}, ${new Date().toISOString()})
      ON CONFLICT (connector_id) DO UPDATE SET
        watermark = EXCLUDED.watermark, updated_at = EXCLUDED.updated_at`;
  }

  // -- Projects ---------------------------------------------------------------

  async putProject(p: ProjectInput): Promise<void> {
    const now = new Date().toISOString();
    await this.sql`
      INSERT INTO projects (name, description, prefixes, connectors, created_at, updated_at)
      VALUES (${p.name}, ${p.description ?? null}, ${jsonParam(this.sql, p.prefixes)},
              ${jsonParam(this.sql, p.connectors ?? [])}, ${now}, ${now})
      ON CONFLICT (name) DO UPDATE SET
        description = EXCLUDED.description, prefixes = EXCLUDED.prefixes,
        connectors = EXCLUDED.connectors, updated_at = EXCLUDED.updated_at`;
  }

  async getProject(name: string): Promise<ProjectRecord | null> {
    const rows = await this.sql`SELECT * FROM projects WHERE name = ${name}`;
    return rows.length ? rowToProject(rows[0] as Row) : null;
  }

  async listProjects(): Promise<ProjectRecord[]> {
    const rows = await this.sql`SELECT * FROM projects ORDER BY name`;
    return (rows as Row[]).map(rowToProject);
  }

  async deleteProject(name: string): Promise<boolean> {
    const rows = await this
      .sql`DELETE FROM projects WHERE name = ${name} RETURNING name`;
    return rows.length > 0;
  }

  // -- Analytics ---------------------------------------------------------------

  async recordQuery(q: QueryRecord): Promise<void> {
    await this.sql`
      INSERT INTO queries (id, created_at, query, mode, project, latency_ms, result_count, zero_hit, top_paths, source, error)
      VALUES (${q.id}, ${q.created_at}, ${q.query}, ${q.mode}, ${q.project}, ${q.latency_ms},
              ${q.result_count}, ${q.zero_hit}, ${jsonParam(this.sql, q.top_paths)}, ${q.source}, ${q.error})`;
  }

  async recordFeedback(f: FeedbackInput): Promise<void> {
    await this.sql`
      INSERT INTO feedback (id, query_id, helpful, comment, created_at)
      VALUES (${ulid()}, ${f.query_id}, ${f.helpful}, ${f.comment ?? null}, ${new Date().toISOString()})`;
  }

  async statsOverview(): Promise<StatsOverview> {
    const [docs] = await this.sql`SELECT COUNT(*)::int AS n FROM documents`;
    const [chunks] = await this.sql`SELECT COUNT(*)::int AS n FROM chunks`;
    const [emb] = await this.sql`
      SELECT COUNT(*)::int AS n FROM chunks WHERE embedded_at IS NOT NULL`;
    const [qs] = await this.sql`SELECT COUNT(*)::int AS n FROM queries`;
    const [zero] = await this.sql`
      SELECT COUNT(*)::int AS n FROM queries WHERE zero_hit`;
    const [helpful] = await this.sql`
      SELECT COUNT(*)::int AS n FROM feedback WHERE helpful`;
    const [total] = await this.sql`SELECT COUNT(*)::int AS n FROM feedback`;
    return {
      documents: num((docs as Row).n),
      chunks: num((chunks as Row).n),
      embedded_chunks: num((emb as Row).n),
      queries: num((qs as Row).n),
      zero_hit_queries: num((zero as Row).n),
      feedback_helpful: num((helpful as Row).n),
      feedback_total: num((total as Row).n),
    };
  }

  // -- Maintenance --------------------------------------------------------------

  async deleteDerivedForOrigin(origin: string): Promise<void> {
    await this.sql`DELETE FROM documents WHERE origin = ${origin}`;
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
    enabled: row.enabled === true,
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
