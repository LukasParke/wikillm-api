# WikiLLM API

A self-hostable **knowledge base service for humans and agents**. Point it at a folder of
markdown — an Obsidian vault, a git repo, or a plain wiki — and it gives you hybrid search,
LLM-answered questions with citations, batch ingestion, live change feeds, and an MCP server,
while the filesystem remains the source of truth.

It runs **alongside** the tools already touching that folder: Obsidian, git, LiveSync, or plain
editors all coexist with the API. Everything derived — indexes, embeddings, FTS — is a
rebuildable cache; delete the database and the service rebuilds it from the files.

Built on the [Karpathy-style LLM wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
pattern with [Google OKF v0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)
bundle conformance and [Cerebras-pattern](https://www.cerebras.ai/blog/how-we-built-our-knowledge-base)
hybrid retrieval.

## Features

- **Filesystem as source of truth** — the wiki folder *is* an OKF bundle; Obsidian and git coexist. The Postgres + pgvector index is a derived cache, with an embedded SQLite fallback (full-text retrieval only).
- **OKF v0.2 conformance** — bundles, required `type` frontmatter, trust tiers (`unverified` / `machine-confirmed` / `human-reviewed`), `status` / `stale_after` lifecycle, provenance `sources`, and `generated {by, at}` stamping on every write.
- **Hybrid retrieval** — full-text search + vector (HNSW) + recency decay, fused with Reciprocal Rank Fusion (K=60), optional LLM rerank, and neighbor-context expansion. No embedder configured? Falls back to full-text mode automatically.
- **Answering with citations** — `POST /v1/query` runs planner → executor → synthesis and returns an answer with `{path, hash, quote}` citations (requires a configured LLM).
- **Optional distillation** — extract question/summary per chunk with the LLM before embedding.
- **Connector framework** — ingest from `git`, `web`, and `github` sources with watermarks and dedupe.
- **Atomic writes + optimistic concurrency** — temp-file + rename; every resource has a SHA-256 `etag`/hash; stale writes return `409 Conflict`.
- **Multi-source attribution** — every API key maps to an OKF actor (`human:<name>` for people, `<name>/wikillm-api` for agents).
- **Live updates** — both Server-Sent Events and WebSocket feeds broadcast filesystem changes, including external edits from Obsidian/git.
- **Projects & RBAC** — named project scopes with per-key `read`/`write`/`admin` roles.
- **MCP server** — LLM-free retrieval tools for agents over stdio or Streamable HTTP.
- **Analytics** — Prometheus `/metrics`, query analytics, and a feedback loop.
- **Batch ingestion** — update a source, many wiki pages, `log.md`, and `index.md` in one request.

## Quick start

```bash
# 1. Install dependencies
bun install

# 2. Configure environment
cp .env.example .env
# Edit .env and set WIKI_ROOT and API_KEYS
```

**Option A — zero-dependency start** (embedded SQLite, full-text search only):

```bash
bun run dev
```

**Option B — full stack** (Postgres + pgvector, hybrid vector search):

```bash
docker compose up -d
```

The API will be available at `http://localhost:3000`.

**Optional — LLM features.** Set `LLM_BASE_URL` (any OpenAI-compatible endpoint: Cerebras,
Ollama, LM Studio, OpenAI) plus `LLM_API_KEY`/`LLM_MODEL` to unlock embeddings, rerank,
distillation, and `POST /v1/query`. Without it, everything else still works in full-text mode.

## Configuration

| Variable | Required | Default | Description |
| -------- | -------- | ------- | ----------- |
| `WIKI_ROOT` | yes | — | Path to the wiki/knowledge-base folder |
| `API_KEYS` | yes | — | Comma-separated `name:key[:scope[:role]]` entries (see [Auth](#auth)) |
| `PORT` | no | `3000` | HTTP port |
| `HOST` | no | `0.0.0.0` | Bind address |
| `PUBLIC_READ` | no | `true` | Allow unauthenticated read access |
| `DB_BACKEND` | no | `auto` | `auto` (postgres when `DATABASE_URL` set, else sqlite), `postgres`, or `sqlite` |
| `DATABASE_URL` | no | — | Postgres connection string (pgvector required for hybrid search) |
| `DB_PATH` | no | `./wikillm-api.db` | SQLite fallback path |
| `LAYOUT` | no | `auto` | Bundle layout profile: `okf`, `wikillm`, or `auto` |
| `OKF_STRICT` | no | `false` | Reject non-conforming writes in bundles declaring `okf_version` |
| `HUMAN_ACTORS` | no | — | Comma-separated API key names attributed as `human:<name>` actors |
| `LLM_BASE_URL` | no | — | OpenAI-compatible base URL (Cerebras, Ollama, LM Studio, OpenAI, …) |
| `LLM_API_KEY` | no | — | API key for the LLM provider |
| `LLM_MODEL` | no | `llama3.1` | Chat model used for rerank/distill/`/v1/query` |
| `LLM_EMBED_MODEL` | no | — | Embedding model; empty = FTS-only mode |
| `EMBEDDING_DIMS` | no | `1536` | Embedding vector dimensions |
| `LLM_DISTILL` | no | `false` | Extract question/summary per chunk with the LLM before embedding |
| `CONNECTOR_POLL_SECONDS` | no | `300` | Connector polling interval |
| `RATE_LIMIT_RPM` | no | `0` | Requests per minute per identity; `0` disables |
| `LOG_LEVEL` | no | `info` | `trace`, `debug`, `info`, `warn`, `error` |

## Auth

`API_KEYS` entries follow the grammar `name:key[:scope[:role]]`:

- **scope** — comma-separated project names or `*` (default `*`, all projects).
- **role** — `admin`, `write`, or `read` (default `write`).

Example: `API_KEYS="agent-codex:secret1,user-luke:secret2,admin:secret3:*:admin"`.

Actor attribution follows the OKF convention: key names starting with `user-`/`human-` or
listed in `HUMAN_ACTORS` are attributed as `human:<name>` actors; all others are attributed
as `<name>/wikillm-api`.

`PUBLIC_READ=true` allows anonymous `GET` requests.

## API overview

All routes are under `/v1` unless noted.

| Method | Route | Description |
| ------ | ----- | ----------- |
| GET | `/health` | Health check (public) |
| GET | `/metrics` | Prometheus metrics (public) |
| GET | `/v1/pages` | List wiki pages |
| GET | `/v1/pages/:rel_path` | Read a page |
| PUT | `/v1/pages/:rel_path` | Create or update a page (OCC via `ifMatch`) |
| DELETE | `/v1/pages/:rel_path` | Delete a page |
| GET | `/v1/sources` | List raw sources |
| GET | `/v1/sources/:rel_path` | Read source metadata |
| POST | `/v1/sources/:rel_path` | Upload a source (write-once unless `?force=true`) |
| DELETE | `/v1/sources/:rel_path` | Delete a source |
| GET | `/v1/index` | Read `index.md` + structured catalog |
| POST | `/v1/index/refresh` | Regenerate `index.md` |
| GET | `/v1/log` | Read `log.md` |
| POST | `/v1/log/append` | Append to `log.md` |
| GET | `/v1/search` | Hybrid search (FTS + vector + recency, RRF fusion) |
| POST | `/v1/query` | Ask a question; answer with citations (requires LLM) |
| GET | `/v1/changes` | Recent changes feed |
| GET | `/v1/events` | SSE live change stream |
| GET | `/v1/ws` | WebSocket live change stream |
| POST | `/v1/ingest` | Batch ingestion |
| GET | `/v1/graph/:rel_path` | Link graph neighbors (depth 1–3) |
| POST | `/v1/okf/validate` | Validate bundle or single document |
| GET | `/v1/okf/layout` | Active layout profile |
| GET | `/v1/bundle/export` | Export bundle as `.tar.gz` |
| POST | `/v1/bundle/import` | Import bundle (admin; `?force=` to overwrite) |
| GET/POST/DELETE | `/v1/connectors` | Manage connectors (`git`, `web`, `github`) (admin) |
| POST | `/v1/connectors/:id/run` | Run a connector now (admin) |
| GET | `/v1/projects` | List projects |
| PUT/DELETE | `/v1/projects/:name` | Manage projects (admin) |
| POST | `/v1/admin/reindex` | Rebuild the index from the filesystem (admin) |
| GET | `/v1/admin/stats` | Store overview stats (admin) |
| POST | `/v1/feedback` | Rate a query answer (`query_id`, `helpful`, `comment?`) |

### Search

`GET /v1/search?q=&limit=&type=&tags=a,b&status=&trust=&fresh=&origin=&project=` returns:

```json
{
  "query": "how does rerank work",
  "mode": "hybrid",
  "latency_ms": 42,
  "results": [
    {
      "rel_path": "wiki/concepts/Rerank.md",
      "title": "Rerank",
      "kind": "page",
      "origin": "agent-codex",
      "okf_type": "concept",
      "tags": ["retrieval"],
      "status": "stable",
      "stale_after": null,
      "trust": "machine-confirmed",
      "hash": "…",
      "mtime": 1755700000000,
      "heading_path": ["Concepts", "Rerank"],
      "snippet": "…",
      "score": 0.0312,
      "context": "…neighboring sections…"
    }
  ]
}
```

`mode` is `hybrid` when an embedder is configured, `fts` otherwise.

### Query

```bash
curl -X POST http://localhost:3000/v1/query \
  -H "Authorization: Bearer secret1" \
  -H "Content-Type: application/json" \
  -d '{"question": "How does RRF fusion work?"}'
```

Returns `{answer, citations: [{rel_path, hash, quote}], evidence, toolsUsed}`. Without a
configured LLM the endpoint responds `503 {"error": "llm_not_configured"}`.

### Create a page

```bash
curl -X PUT http://localhost:3000/v1/pages/wiki/entities/OpenAI.md \
  -H "Authorization: Bearer secret1" \
  -H "Content-Type: application/json" \
  -d '{
    "content": "# OpenAI\n\nOpenAI is an AI research company.",
    "frontmatter": { "tags": ["ai"] }
  }'
```

Writes stamp `updated_at`/`updated_by` and the OKF `generated {by, at}` provenance fields.

### Update with concurrency check

```bash
curl -X PUT http://localhost:3000/v1/pages/wiki/entities/OpenAI.md \
  -H "Authorization: Bearer secret1" \
  -H "Content-Type: application/json" \
  -d '{
    "content": "# OpenAI\n\nUpdated text.",
    "ifMatch": "<hash-from-previous-read>"
  }'
```

A hash mismatch returns `409 Conflict` with the current content.

### Ingest a source and update many pages

```bash
curl -X POST http://localhost:3000/v1/ingest \
  -H "Authorization: Bearer secret1" \
  -H "Content-Type: application/json" \
  -d '{
    "source": {
      "title": "Article A",
      "rel_path": "raw/article-a.md",
      "content": "# Article A\n\n..."
    },
    "operations": [
      { "rel_path": "wiki/summaries/Article A.md", "content": "Summary of A" },
      { "rel_path": "wiki/entities/A.md", "content": "Entity A" }
    ],
    "logEntry": "Article A"
  }'
```

### WebSocket live feed

```javascript
const ws = new WebSocket("ws://localhost:3000/v1/ws");
ws.onmessage = (event) => {
  const change = JSON.parse(event.data);
  console.log(change.data.rel_path, change.data.change_type);
};
```

## MCP server

WikiLLM API ships an MCP server (`src/mcp.ts`) exposing LLM-free retrieval primitives to
agents: `search`, `get_concept`, `read_source`, `list_changes`, `graph_neighbors`,
`propose_edit`, `append_log`, `query`, and `refresh_index`.

It runs over **stdio** by default. Set `MCP_HTTP_PORT` to enable **Streamable HTTP** with
Bearer auth (`WIKILLM_API_KEY`) pointing at a running instance via `WIKILLM_URL`.

### Claude Code

```json
{
  "mcpServers": {
    "wikillm": {
      "command": "bun",
      "args": ["run", "src/mcp.ts"],
      "env": {
        "WIKILLM_URL": "http://localhost:3000",
        "WIKILLM_API_KEY": "key"
      }
    }
  }
}
```

## OKF conformance

The wiki folder is treated as a [Google Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)
v0.2 bundle:

- Documents carry required `type` frontmatter plus `status`, `stale_after`, and provenance
  `sources`.
- Trust tiers (`unverified`, `machine-confirmed`, `human-reviewed`) are derived and indexed.
- Every API write stamps `generated {by, at}` using the OKF actor convention.
- `POST /v1/okf/validate` validates the whole bundle (empty body) or a single document
  (`{content: "..."}`), reporting `{valid, errors, warnings, stats}`.
- `GET /v1/okf/layout` reports the active layout profile (`okf` or `wikillm`).
- `index.md` and `log.md` follow OKF §8/§9 formats (sectioned index, date-grouped log).

## Deployment

### Docker Compose (recommended)

1. Copy `.env.example` to `.env` and configure at least `API_KEYS`.
2. Set `WIKI_PATH` to your wiki folder:
   - Local folder: `WIKI_PATH=./wiki`
   - Remote/network mount on the host: `WIKI_PATH=/mnt/nas/wiki`
3. Deploy:

```bash
# Using the helper script
./scripts/deploy.sh

# Or manually
docker compose up -d
```

### Docker run

```bash
docker run -d -p 3000:3000 \
  -v /path/to/wiki:/wiki \
  -e WIKI_ROOT=/wiki \
  -e API_KEYS='agent-codex:secret,user-luke:secret2' \
  -e PUBLIC_READ=true \
  ghcr.io/lukasparke/wikillm-api:latest
```

### Published image

CI automatically builds and publishes to:

```
ghcr.io/lukasparke/wikillm-api:latest
ghcr.io/lukasparke/wikillm-api:main
ghcr.io/lukasparke/wikillm-api:<semver>
```

## API documentation

Interactive OpenAPI reference is published on GitHub Pages:

**https://lukasparke.github.io/wikillm-api/**

The spec is also available at [`docs/openapi.yaml`](docs/openapi.yaml).

## Running tests

```bash
bun run test:run
```

## Benchmarks

Two benchmark scripts are included:

- `bash scripts/benchmark.sh` — synthetic peak-throughput tests with [autocannon](https://github.com/mcollina/autocannon).
- `bun run scripts/benchmark-realistic.ts` — realistic mixed-workload scenarios with think times, a seeded wiki, and varied client behaviors.

Measured on a Ryzen 9 7950X3D / 62 GiB / Bun 1.3.13:

### Synthetic peak throughput

| Endpoint                             | Concurrency |    Throughput | p99 latency |
| ------------------------------------ | ----------: | ------------: | ----------: |
| `GET /health`                        |         200 | ~95,000 req/s |        4 ms |
| `GET /v1/pages/wiki/...`             |         100 | ~45,000 req/s |       11 ms |
| `PUT /v1/pages/wiki/...` (same page) |          10 |  ~4,900 req/s |        3 ms |
| `POST /v1/ingest`                    |           1 |    ~800 ops/s |        2 ms |
| `PUT /v1/pages/wiki/{unique}.md`     |           1 |    ~929 req/s |           — |

### Realistic workload

| Scenario            | Clients | Think time |   Throughput | p99 latency |
| ------------------- | ------: | ---------: | -----------: | ----------: |
| Read-heavy browsing |     100 |       50ms | ~3,700 req/s |       16 ms |
| Mixed read/write    |      50 |       100ms |   ~970 req/s |        8 ms |
| Write-heavy editing |      25 |       50ms |   ~190 req/s |        1 ms |
| Batch ingestion     |       3 |       200ms |    ~19 ops/s |       42 ms |
| Observer polling    |      10 |       500ms |     ~7 req/s |    2,750 ms |

See [`scripts/benchmark-results.md`](scripts/benchmark-results.md) for full details and raw output.

Benchmarks for the hybrid-search and `/v1/query` pipelines are pending.

## Coexistence with Obsidian, git, and sync tools

- Writes use atomic temp-file + rename, so Obsidian never sees partial files.
- The API does not lock files long-term; it only serializes writes from API clients.
- External changes are detected and broadcast over SSE/WebSocket.
- `.obsidian`, `.git`, and temporary files are ignored.

## License

MIT
