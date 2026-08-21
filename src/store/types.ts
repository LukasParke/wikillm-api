import type { ChangeEvent, Operation } from "../types/index.js";

// ---------------------------------------------------------------------------
// Domain records
// ---------------------------------------------------------------------------

/** A document known to the index. FS-backed pages/sources plus connector docs. */
export interface DocumentRecord {
  id: string;
  rel_path: string;
  /** page = wiki markdown, source = raw/ asset, doc = connector-materialized */
  kind: "page" | "source" | "doc";
  /** "wiki" for FS-backed, otherwise the connector id */
  origin: string;
  title: string | null;
  summary: string | null;
  body: string;
  frontmatter: Record<string, unknown>;
  word_count: number;
  outgoing_links: string[];
  hash: string;
  mtime: number;
  content_type: string | null;
  // OKF extracted fields
  okf_type: string | null;
  tags: string[];
  status: string | null;
  stale_after: string | null;
  resource: string | null;
  generated_by: string | null;
  generated_at: string | null;
  verified: Array<{ by: string; at: string }> | null;
  provenance: Array<Record<string, unknown>> | null;
  updated_at: string | null;
  updated_by: string | null;
  indexed_at: string;
}

export interface DocumentInput {
  rel_path: string;
  kind: DocumentRecord["kind"];
  origin: string;
  title?: string | null;
  summary?: string | null;
  body?: string;
  frontmatter?: Record<string, unknown>;
  word_count?: number;
  outgoing_links?: string[];
  hash: string;
  mtime: number;
  content_type?: string | null;
  okf_type?: string | null;
  tags?: string[];
  status?: string | null;
  stale_after?: string | null;
  resource?: string | null;
  generated_by?: string | null;
  generated_at?: string | null;
  verified?: Array<{ by: string; at: string }> | null;
  provenance?: Array<Record<string, unknown>> | null;
  updated_at?: string | null;
  updated_by?: string | null;
}

export interface Distilled {
  question?: string;
  summary?: string;
  resolution?: string;
  entities?: string[];
  code_refs?: string[];
  [key: string]: unknown;
}

export interface ChunkInput {
  ordinal: number;
  heading_path?: string | null;
  content: string;
  distilled?: Distilled | null;
}

export interface ChunkRecord extends ChunkInput {
  id: string;
  document_id: string;
  embedded_at: string | null;
  embed_model: string | null;
}

/** A scored chunk hit returned by retrieval primitives. */
export interface ChunkHit {
  chunk_id: string;
  document_id: string;
  rel_path: string;
  kind: string;
  origin: string;
  title: string | null;
  okf_type: string | null;
  tags: string[];
  status: string | null;
  stale_after: string | null;
  verified: Array<{ by: string; at: string }> | null;
  hash: string;
  mtime: number;
  heading_path: string | null;
  content: string;
  score: number;
}

export interface SearchFilters {
  kinds?: Array<"page" | "source" | "doc">;
  origins?: string[];
  okf_types?: string[];
  tags?: string[];
  statuses?: string[];
  /** minimum trust tier: unverified < machine-confirmed < human-reviewed */
  trustMin?: "unverified" | "machine-confirmed" | "human-reviewed";
  /** exclude concepts past stale_after */
  freshOnly?: boolean;
  /** restrict to these rel_path prefixes (project scoping); ["*"] disables */
  pathPrefixes?: string[];
}

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

export interface ConnectorConfig {
  id: string;
  kind: string;
  config: Record<string, unknown>;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

export interface ProjectRecord {
  name: string;
  description: string | null;
  /** rel_path prefixes included in this project */
  prefixes: string[];
  /** connector ids included in this project */
  connectors: string[];
  created_at: string;
  updated_at: string;
}

export interface ProjectInput {
  name: string;
  description?: string | null;
  prefixes: string[];
  connectors?: string[];
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

export interface QueryRecord {
  id: string;
  created_at: string;
  query: string;
  mode: string;
  project: string | null;
  latency_ms: number;
  result_count: number;
  zero_hit: boolean;
  top_paths: string[];
  source: string | null;
  error: string | null;
}

export interface FeedbackInput {
  query_id: string;
  helpful: boolean;
  comment?: string | null;
}

export interface StatsOverview {
  documents: number;
  chunks: number;
  embedded_chunks: number;
  queries: number;
  zero_hit_queries: number;
  feedback_helpful: number;
  feedback_total: number;
}

export interface ApiKeyRecord {
  name: string;
  /** sha256 hex of the bearer secret; plaintext never stored */
  key_hash: string;
  /** first 6 chars of the secret for identification */
  key_prefix: string;
  scope: string[];
  role: "admin" | "write" | "read";
  created_at: string;
  updated_at: string;
  created_by: string;
}

export interface ApiKeyUpsert {
  name: string;
  key_hash: string;
  key_prefix: string;
  scope: string[];
  role: "admin" | "write" | "read";
  created_by: string;
}

export type SettingsMap = Record<string, unknown>;

// ---------------------------------------------------------------------------
// Store interface — the single persistence contract.
// All methods async so SQLite and Postgres implementations are interchangeable.
// ---------------------------------------------------------------------------

export type TrustTier = "unverified" | "machine-confirmed" | "human-reviewed";

export const TRUST_ORDER: Record<TrustTier, number> = {
  unverified: 0,
  "machine-confirmed": 1,
  "human-reviewed": 2,
};

export function trustTier(
  verified: Array<{ by: string; at: string }> | null | undefined,
): TrustTier {
  if (!verified || verified.length === 0) return "unverified";
  return verified.some((v) => v.by.startsWith("human:"))
    ? "human-reviewed"
    : "machine-confirmed";
}

export interface ListOptions {
  folder?: string;
  kind?: DocumentRecord["kind"];
  origin?: string;
  limit?: number;
  cursor?: string;
}

export interface Page<T> {
  items: T[];
  nextCursor?: string;
}

export interface Store {
  readonly backend: "sqlite" | "postgres";

  migrate(): Promise<void>;
  close(): Promise<void>;

  // Documents
  upsertDocument(doc: DocumentInput): Promise<void>;
  getDocument(relPath: string): Promise<DocumentRecord | null>;
  deleteDocument(relPath: string): Promise<void>;
  listDocuments(opts: ListOptions): Promise<Page<DocumentRecord>>;
  countDocuments(opts?: { origin?: string }): Promise<number>;

  // Chunks
  replaceChunks(documentId: string, chunks: ChunkInput[]): Promise<void>;
  getChunksForDocument(documentId: string): Promise<ChunkRecord[]>;
  getChunk(chunkId: string): Promise<ChunkRecord | null>;
  upsertEmbeddings(
    items: Array<{ chunkId: string; vector: number[] }>,
    model: string,
    embeddedAt: string,
  ): Promise<void>;
  setChunkDistilled(
    chunkId: string,
    distilled: Distilled | null,
  ): Promise<void>;
  listUnembeddedChunks(
    limit: number,
  ): Promise<Array<ChunkRecord & { rel_path: string }>>;

  // Edges (link graph)
  replaceEdges(srcRelPath: string, dstRelPaths: string[]): Promise<void>;
  backlinks(relPath: string, limit?: number): Promise<string[]>;

  // Retrieval primitives
  searchFts(
    q: string,
    opts: { limit: number; filters?: SearchFilters },
  ): Promise<ChunkHit[]>;
  searchVector(
    vector: number[],
    opts: { limit: number; filters?: SearchFilters },
  ): Promise<ChunkHit[]>;
  supportsVector(): boolean;

  // Operations ledger
  insertOperation(op: Operation): Promise<void>;
  getOperation(id: string): Promise<Operation | null>;

  // Changes ledger
  insertChange(change: ChangeEvent["data"]): Promise<void>;
  listChanges(opts: {
    since?: string;
    path?: string;
    source?: string;
    limit?: number;
  }): Promise<ChangeEvent["data"][]>;

  // Connectors
  putConnector(c: ConnectorConfig): Promise<void>;
  getConnector(id: string): Promise<ConnectorConfig | null>;
  listConnectors(): Promise<ConnectorConfig[]>;
  deleteConnector(id: string): Promise<boolean>;
  getConnectorState(id: string): Promise<unknown | null>;
  setConnectorState(id: string, watermark: unknown): Promise<void>;

  // Projects
  putProject(p: ProjectInput): Promise<void>;
  getProject(name: string): Promise<ProjectRecord | null>;
  listProjects(): Promise<ProjectRecord[]>;
  deleteProject(name: string): Promise<boolean>;

  // Analytics
  recordQuery(q: QueryRecord): Promise<void>;
  recordFeedback(f: FeedbackInput): Promise<void>;
  statsOverview(): Promise<StatsOverview>;

  // Maintenance
  deleteDerivedForOrigin(origin: string): Promise<void>;
  /** Drop all embedding vectors and clear embedded flags (dims change). */
  resetEmbeddings(): Promise<void>;

  // Runtime settings (key/value overrides persisted across restarts)
  getSettings(): Promise<SettingsMap>;
  setSetting(key: string, value: unknown, updatedBy: string): Promise<void>;
  deleteSetting(key: string): Promise<boolean>;

  // API keys (hashed; env keys are merged at the auth layer)
  listApiKeys(): Promise<ApiKeyRecord[]>;
  getApiKey(name: string): Promise<ApiKeyRecord | null>;
  findApiKeyByHash(keyHash: string): Promise<ApiKeyRecord | null>;
  upsertApiKey(input: ApiKeyUpsert): Promise<void>;
  deleteApiKey(name: string): Promise<boolean>;
  countApiKeys(): Promise<number>;
}
