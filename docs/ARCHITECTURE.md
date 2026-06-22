# WikiLLM API — Architecture & Design Plan

A TypeScript/Hono HTTP API that sits **alongside** a wiki-llm folder (the Karpathy-style LLM wiki pattern). It lets multiple users, agents, and automations read and write the same markdown wiki and raw sources while Obsidian, git, LiveSync, or other tools are also touching the same directory tree.

The filesystem remains the source of truth. The API is a disciplined, concurrency-aware gateway on top of it.

---

## 1. Goals & Constraints

**Goals**

- Expose the wiki folder over a clean REST API.
- Allow multiple concurrent writers (human users, LLM agents, scrapers, CI jobs).
- Preserve the existing plain-markdown workflow — Obsidian/git/LiveSync keep working unchanged.
- Make updates discoverable and traceable (who changed what, when, and why).
- Make ingestion safe: a single source can touch many wiki pages plus `index.md` and `log.md` atomically-ish.

**Hard constraints**

- Wiki folder may be open in Obsidian at the same time.
- Other sync processes (git, LiveSync, Syncthing, Dropbox, etc.) may read/write files at any time.
- No exclusive ownership of the folder — we cannot prevent external writes.
- The API may run on the same machine as the vault or in a container with the folder mounted.

---

## 2. Wiki-LLM Folder Model

We assume the wiki follows the Karpathy-style layout:

```
WIKI_ROOT/
├── AGENTS.md or CLAUDE.md   # schema / conventions for LLM agents
├── index.md                 # content catalog of the wiki
├── log.md                   # chronological, append-only activity log
├── raw/                     # immutable source documents
│   ├── assets/              # downloaded images/attachments
│   ├── article-1.md
│   └── paper.pdf
├── wiki/                    # LLM-generated pages (the wiki proper)
│   ├── entities/
│   ├── concepts/
│   ├── summaries/
│   ├── synthesis.md
│   └── overview.md
└── .obsidian/               # Obsidian settings (ignored)
```

Files are Markdown with optional YAML frontmatter. Cross-links use wiki-style `[[Page Name]]` links. `index.md` is a hand/machine-curated catalog. `log.md` is append-only.

The API treats `AGENTS.md`/`CLAUDE.md` as read-only configuration for agents; it does not edit it unless explicitly asked.

---

## 3. Core Design Principles

1. **Filesystem is source of truth.** SQLite is only a cache/ledger; if the DB is deleted it can be rebuilt by scanning the folder.
2. **Atomic writes.** Every file update is written to a temp file next to the target, then `fs.rename`-ed into place. Readers never see a half-written file.
3. **Advisory in-process locking.** We serialize writes to the same path inside the API process, but we cannot lock out Obsidian. Lock windows are kept extremely short (read → compute → write → release).
4. **Optimistic concurrency control (OCC).** Every resource has an ETag (SHA-256 of content). A `PUT` must include the hash the client based its edit on. If the file changed underneath, return `409 Conflict` with the current content/hash.
5. **Watch external changes.** A file watcher keeps the API’s cached view and change log up to date when Obsidian/git/LiveSync modify files.
6. **Idempotent, structured ingestion.** Ingesting a source is a batch operation that updates many pages; each page update still goes through OCC, and the whole batch is recorded as one operation.
7. **Source attribution.** Every change records which user/agent/source made it.

---

## 4. Repository Layout

```
wikillm-api/
├── src/
│   ├── index.ts              # entry point: load config, start server + watcher
│   ├── app.ts                # Hono app assembly, global middleware, route mounting
│   ├── config.ts             # env-var config + defaults (WIKI_ROOT, PORT, DB_PATH, KEYS)
│   ├── db/
│   │   ├── client.ts         # sqlite client setup
│   │   ├── schema.sql        # migrations / schema
│   │   └── migrations.ts     # simple migration runner
│   ├── fs/
│   │   ├── paths.ts          # safe path resolution under WIKI_ROOT
│   │   ├── atomic.ts         # atomic read/write helpers
│   │   ├── lock.ts           # per-path advisory async locks
│   │   ├── watcher.ts        # chokidar-based recursive watcher
│   │   └── wiki.ts           # list/read helpers for wiki pages & sources
│   ├── routes/
│   │   ├── health.ts
│   │   ├── pages.ts          # CRUD for wiki pages
│   │   ├── sources.ts        # raw source CRUD / upload
│   │   ├── index.ts          # index.md refresh/read
│   │   ├── log.ts            # log.md append/read
│   │   ├── search.ts         # search titles/bodies
│   │   ├── changes.ts        # recent changes feed
│   │   ├── events.ts         # Server-Sent Events live feed
│   │   └── ws.ts             # WebSocket live feed
│   ├── services/
│   │   ├── pageService.ts
│   │   ├── sourceService.ts
│   │   ├── indexService.ts
│   │   ├── logService.ts
│   │   ├── changeTracker.ts  # reconcile watcher events with DB cache
│   │   ├── broadcaster.ts    # fan-out filesystem changes to SSE + WebSocket clients
│   │   └── ingestService.ts  # multi-file batch ingestion
│   ├── middleware/
│   │   ├── auth.ts           # API-key bearer auth
│   │   ├── error.ts          # centralized error response
│   │   └── validate.ts       # Zod validation middleware
│   └── types/
│       └── index.ts          # shared TS types
├── tests/                    # unit + integration tests
├── scripts/
│   └── dev.ts                # tsx-based dev runner
├── package.json
├── tsconfig.json
├── .env.example
├── README.md
└── docs/
    └── ARCHITECTURE.md       # this document
```

---

## 5. Technology Stack

| Concern              | Choice                                                                                                                  | Rationale                                                            |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| HTTP framework       | [Hono](https://hono.dev/)                                                                                               | Lightweight, fast, great TS/Deno/Node/Bun support, middleware model. |
| Validation           | [Zod](https://zod.dev/)                                                                                                 | TS-first schemas for request bodies and query params.                |
| Markdown frontmatter | [gray-matter](https://www.npmjs.com/package/gray-matter)                                                                | De-facto standard.                                                   |
| File watching        | [chokidar](https://www.npmjs.com/package/chokidar)                                                                      | Cross-platform, stable, handles high-volume renames.                 |
| Local database       | [better-sqlite3](https://www.npmjs.com/package/better-sqlite3) (Node) or [libsql](https://www.npmjs.com/package/libsql) | Sync SQLite for cache/ledger. Can be swapped.                        |
| File locking         | Custom per-path async mutex                                                                                             | No cross-process locking required; keeps external tools unaffected.  |
| Logging              | [pino](https://www.npmjs.com/package/pino)                                                                              | Structured logs, low overhead.                                       |
| Testing              | vitest + supertest                                                                                                      | Fast, native TS support.                                             |
| WebSockets           | Hono Bun WebSocket helper (`hono/bun`)                                                                                  | Native Bun WebSocket support; Node fallback via `@hono/node-server`. |

Runtime target: **Bun** is preferred for Hono (fast startup, built-in TS, native WebSocket support), but the code stays runnable on Node 20+ with `tsx`.

---

## 6. Data Model

### 6.1 Filesystem Model

The API recognizes three namespaces under `WIKI_ROOT`:

- `wiki/**` — mutable markdown pages.
- `raw/**` — immutable source documents (write-once).
- `AGENTS.md` / `CLAUDE.md` / `index.md` / `log.md` — special top-level files.

Paths are URL-encoded relative paths without leading slash, e.g. `wiki/entities/OpenAI.md`.

### 6.2 SQLite Cache / Ledger

Reconstructible from the filesystem. Tables:

```sql
-- operations: every mutation initiated through the API
CREATE TABLE operations (
  id TEXT PRIMARY KEY,           -- ulid
  created_at TEXT NOT NULL,      -- ISO-8601
  source TEXT NOT NULL,          -- user/agent source identifier
  action TEXT NOT NULL,          -- 'create','update','delete','ingest','index_refresh','log_append'
  paths TEXT NOT NULL,           -- JSON array of affected relative paths
  metadata TEXT,                 -- JSON object (old/new hashes, conflict resolution, etc.)
  parent_id TEXT                 -- for batched operations
);

-- page_cache: derived metadata for fast listing/search
CREATE TABLE page_cache (
  rel_path TEXT PRIMARY KEY,
  abs_path TEXT NOT NULL,
  title TEXT,
  summary TEXT,
  frontmatter TEXT,              -- JSON
  word_count INTEGER,
  outgoing_links TEXT,           -- JSON array of [[links]]
  hash TEXT NOT NULL,            -- sha256 of file content
  mtime INTEGER NOT NULL,
  updated_at TEXT,
  updated_by TEXT
);

-- changes: combined API + external filesystem changes
CREATE TABLE changes (
  id TEXT PRIMARY KEY,
  detected_at TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  change_type TEXT NOT NULL,     -- 'created','modified','deleted','renamed'
  old_hash TEXT,
  new_hash TEXT,
  source TEXT,                   -- 'api' or 'external'
  operation_id TEXT,
  INDEX idx_path (rel_path),
  INDEX idx_detected (detected_at)
);
```

On startup the cache is rebuilt (or validated via `mtime`/`hash`). The watcher invalidates and refreshes entries.

---

## 7. API Routes

Base path: `/v1`. Content-Type JSON unless otherwise noted.

### Health

```
GET  /health
```

Returns wiki root path, version, watcher status.

### Wiki Pages

```
GET    /v1/pages?folder=wiki/entities&limit=50&cursor=...
GET    /v1/pages/:rel_path          # returns parsed frontmatter + body + etag
PUT    /v1/pages/:rel_path          # body: { content, frontmatter?, ifMatch? }
DELETE /v1/pages/:rel_path
```

`PUT` semantics:

- `ifMatch` is the SHA-256 hash the client read. Required for updates; omit for create.
- If the current file hash differs, return `409 Conflict` with `{ current: { hash, content } }`.
- On success, atomically write file, append optional frontmatter fields (`updated_at`, `updated_by`), update `page_cache`, record an `operation`, emit a `change`.

### Raw Sources

```
GET    /v1/sources?folder=raw&limit=50&cursor=...
GET    /v1/sources/:rel_path
POST   /v1/sources/:rel_path        # body: raw bytes or { content, contentType? }
DELETE /v1/sources/:rel_path
```

Sources are **write-once** by default. Re-uploading the same path returns `409` unless `?force=true`. This protects the immutable source layer.

### Index (`index.md`)

```
GET  /v1/index                      # returns structured catalog + raw markdown
POST /v1/index/refresh              # regenerates index.md from page_cache
```

The refresh service builds a markdown catalog from `page_cache` grouped by category/tags.

### Log (`log.md`)

```
GET  /v1/log                        # returns raw markdown + structured entries
POST /v1/log/append                 # body: { entry: string, prefix? }
```

Append is atomic and prefix-normalized. Entries default to `## [ISO date] source | message`.

### Search

```
GET /v1/search?q=term&in=title|body|frontmatter&limit=20
```

Simple substring + frontmatter filtering. Later pluggable to a local vector/BM25 engine.

### Changes / Activity

```
GET /v1/changes?since=ISO&path=...&source=...&limit=100
```

Returns both API-initiated and externally detected changes.

### Live Events

```
GET /v1/events
```

Server-Sent Events stream of file-system changes (debounced). Clients can subscribe for live updates.

### WebSocket Feed

```
GET /v1/ws
```

Native WebSocket endpoint for the same change stream. Preferred by agents/tools that need bidirectional signaling or lower overhead than SSE.

- Connection upgrades through `upgradeWebSocket` from `hono/bun`.
- Broadcasts the same `ChangeEvent` payload as SSE.
- Clients may optionally send protocol messages (e.g. subscribe filter, ping).
- Server entry point exports `{ fetch: app.fetch, websocket }` so `Bun.serve` can handle the upgrade.

Example event payload:

```json
{
  "type": "change",
  "data": {
    "rel_path": "wiki/entities/OpenAI.md",
    "change_type": "modified",
    "old_hash": "abc...",
    "new_hash": "def...",
    "source": "agent-codex",
    "detected_at": "2026-06-22T21:00:00.000Z"
  }
}
```

### Ingest (batch)

```
POST /v1/ingest
```

Body:

```jsonc
{
  "source": { "title": "Article Title", "rel_path": "raw/article-1.md" },
  "operations": [
    { "rel_path": "wiki/summaries/Article Title.md", "content": "...", "frontmatter": {...} },
    { "rel_path": "wiki/entities/OpenAI.md", "content": "...", "ifMatch": "..." },
    { "rel_path": "wiki/concepts/RLHF.md", "content": "...", "ifMatch": "..." }
  ],
  "index": { "entries": [...] },
  "logEntry": "ingest | Article Title"
}
```

The ingest service:

1. Acquires per-path locks in alphabetical order (deadlock avoidance).
2. Verifies every `ifMatch` still matches.
3. Writes all files atomically.
4. Appends to `log.md`.
5. Refreshes affected `index.md` entries.
6. Records one parent `operation` plus child operations.

If any OCC check fails, the whole batch fails with `409 Multi-Status`, returning per-path status.

---

## 8. Concurrency & Conflict Model

### 8.1 Write Path

```
1. Validate path (must be inside WIKI_ROOT, not a traversal).
2. Acquire per-path async lock.
3. Read current file + compute SHA-256.
4. If If-Match provided and != current hash → 409.
5. (Optionally merge frontmatter with updated_at/updated_by.)
6. Write to `<target>.<uuid>.tmp` in same directory.
7. fs.rename(tmp, target).
8. Release lock.
9. Watcher detects change → update cache + changes table + broadcast to SSE and WebSocket clients.
```

Steps 3–8 happen under the lock but are kept very short. External tools can still read the old file during the write and will atomically see the new one after rename.

### 8.2 Advisory Locking

- An in-memory `Map<string, PromiseQueue>` maps relative paths to async mutexes.
- Locks are only visible to this API process. They protect against two API clients colliding, not against Obsidian.
- For multi-file operations, locks are acquired in sorted path order to prevent deadlocks.

### 8.3 Optimistic Concurrency

Every state-changing request carries an `If-Match: <sha256>` header or `ifMatch` body field. The API rejects stale bases with `409 Conflict` and the current file content. Clients (LLM agents) can then diff/merge and retry.

### 8.4 External Changes

When the watcher sees a change not initiated by the API, it records a `change` row with `source = 'external'`. API clients can poll `/changes`, subscribe to SSE `/events`, or connect to the WebSocket `/ws` feed to react.

---

## 9. Multi-User / Multi-Source Model

We do **not** create per-user copies of the wiki. Everyone shares the same files.

- **Authentication**: API key Bearer tokens. Each key maps to a `source` name (e.g., `agent-codex`, `user-luke`, `clipper`).
- **Attribution**: Each write records the source in:
  - the `operations` table,
  - the `changes` table,
  - optionally frontmatter `updated_by` and `updated_at`.
- **Rate limiting**: per-source token bucket (optional, TBD).
- **Permissions** (future): read/write allowlists per source.

Default: single API key for local personal use, source = `api`.

---

## 10. Coexistence with Obsidian, Git, LiveSync

The API is a polite citizen:

- **No long-lived file handles.** We open, read/write, close immediately.
- **Atomic renames.** Obsidian never sees partial writes.
- **Ignores Obsidian metadata.** `.obsidian`, `.trash`, workspace files are excluded from watcher and API.
- **Respects git.** The API does not modify `.git`. Optional `/v1/git/commit` endpoint can be enabled.
- **Works with LiveSync/Syncthing.** Because writes are atomic and short-lived, sync tools are less likely to capture half-files.
- **Conflict files.** If LiveSync creates conflict copies (e.g., `Note (conflict 2026-06-22).md`), the watcher records them; `/changes` can surface them for review.

### Recommended vault settings

- Disable “Safe mode” if plugins need to read API-frontmatter fields.
- Set Obsidian attachment folder to `raw/assets/` for consistency.
- Enable “Detect all file extensions” so non-markdown sources are visible.

---

## 11. File Watcher & Change Tracking

- `chokidar` watches `WIKI_ROOT` recursively.
- Ignored: `.git`, `node_modules`, `.obsidian`, `*.tmp`, lockfiles, `*.crdownload`, etc.
- Debounce window: ~100ms.
- On event:
  1. Compute relative path and hash.
  2. Compare with `page_cache`.
  3. Insert `changes` row if different.
  4. Update or delete `page_cache` entry.
  5. Broadcast the `ChangeEvent` to all SSE and WebSocket subscribers via `broadcaster.ts`.

Startup behavior:

- If DB is empty/missing, run a full scan and populate `page_cache`.
- If DB exists, compare mtime/hash for a sampled set and resync anything stale; then rely on watcher.

---

## 12. Deployment

### Local development

```bash
WIKI_ROOT=~/wiki-llm-api-test bun run dev
```

### Production / self-hosted

```bash
WIKI_ROOT=/data/wiki PORT=3000 API_KEYS="agent-codex:xxx,user-luke:yyy" bun run start
```

Because the app uses native Bun WebSockets, the entry point exports `{ fetch: app.fetch, websocket }` and is served with `Bun.serve`. Node deployments use `@hono/node-server` instead.

### Docker

```dockerfile
FROM oven/bun:1
WORKDIR /app
COPY . .
RUN bun install
ENV WIKI_ROOT=/wiki
VOLUME ["/wiki"]
EXPOSE 3000
CMD ["bun", "run", "src/index.ts"]
```

Mount the same host directory into both the API container and the Obsidian vault.

---

## 13. Security

- Path traversal: resolve every path, verify `realpath` is inside `WIKI_ROOT`.
- Authentication: Bearer API key required for all mutating routes (configurable to allow public reads).
- File type allowlist for uploads (no executables).
- Max body size limits per route (e.g., 50MB for sources, 5MB for pages).
- Do not expose `.env`, DB file, or lock directory.

---

## 14. MVP Scope vs. Future Work

**MVP**

- CRUD pages and sources.
- Atomic writes + OCC + per-path locks.
- File watcher + changes feed.
- `index.md` refresh and `log.md` append.
- Search by title/body/frontmatter.
- SSE events.
- **WebSocket events** (`/v1/ws`).
- Single-key auth + source attribution.

**Future**

- Multi-key RBAC.
- Git commit/push endpoints.
- Vector search integration (qmd, sqlite-vec).
- Plugin endpoints for Obsidian Web Clipper.
- WebDAV or MCP server adapter.
- Conflict-resolution merge helpers.

---

## 15. Design Decisions (Confirmed)

Before implementation began, the following choices were confirmed:

1. **Runtime**: Bun (with Node 20+ `tsx` fallback).
2. **Authentication model**: Multiple named API keys mapped to source names.
3. **Real-time updates**: SSE **and** native WebSockets both supported; clients choose their integration.
4. **Conflict handling**: Strict OCC (client must retry).
5. **Database**: SQLite via `better-sqlite3` (Node) / `bun:sqlite` (Bun).
6. **Multi-wiki support**: One process per wiki root for the MVP.
