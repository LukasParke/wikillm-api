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
- **General document listing + raw downloads** — `GET /v1/documents` spans pages, sources, and connector docs with shared filters and ETag revalidation (304); `/content` endpoints download raw content, dispatched by kind.
- **Batch writes** — `POST /v1/pages/batch` applies up to 1000 writes/deletes atomically behind an all-or-nothing OCC preflight.
- **Outbound webhooks (signed)** — admin-registered HTTP endpoints receive HMAC-SHA256-signed change deliveries with retries.
- **Local ONNX embeddings** — semantic search with zero external services via an in-process transformers.js embedder (see [ONNX embeddings (local)](#onnx-embeddings-local)).
- **Projects & RBAC** — named project scopes with per-key `read`/`write`/`admin` roles.
- **MCP server** — LLM-free retrieval tools for agents over stdio or Streamable HTTP.
- **Analytics** — Prometheus `/metrics`, query analytics, and a feedback loop.
- **Fully runtime-configurable** — settings, LLM endpoint, and rate limits hot-apply via `GET/PUT /v1/settings/:key` (no restart); fresh instances bootstrap an admin key automatically.

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
| `API_KEYS` | no | — | Comma-separated `name:key[:scope[:role]]` entries (see [Auth](#auth)). **Optional**: a fresh instance with no keys mints a bootstrap admin key and prints it to the log once |
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
| `BOOTSTRAP_ADMIN_KEY` | no | — | Pin the bootstrap admin key instead of a random one (only used when no keys are configured) |

## Auth

`API_KEYS` entries follow the grammar `name:key[:scope[:role]]` (the variable is optional —
see [Bootstrap flow](#runtime-configuration-no-restart) for keyless first boot):

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
| GET | `/v1/documents` | List every indexed document (pages, sources, connector docs) with filters + ETag revalidation |
| GET | `/v1/documents/:rel_path/content` | Download any document's content (dispatches by kind) |
| GET | `/v1/pages/:rel_path/raw` | Read a page's raw markdown (ETag) |
| GET | `/v1/sources/:rel_path/content` | Download a source's original bytes (`Content-Type` + ETag) |
| POST | `/v1/pages/batch` | Atomic multi-page writes/deletes (all-or-nothing OCC preflight, max 1000) |
| POST | `/v1/documents/delete` | Bulk delete with per-op results (connector-managed docs return `connector_managed`) |
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
| GET | `/v1/graph/:rel_path` | Link graph neighbors (depth 1–3); `format=dot` returns Graphviz (`text/vnd.graphviz`) |
| POST | `/v1/okf/validate` | Validate bundle or single document |
| GET | `/v1/okf/layout` | Active layout profile |
| GET | `/v1/bundle/export` | Export bundle as `.tar.gz`; filters `prefix`/`kind`/`origin`/`since`/`project`, header `X-Exported-Files`, 404 when nothing matches |
| POST | `/v1/bundle/import` | Import bundle (admin; `?force=` to overwrite) |
| GET/POST/DELETE | `/v1/connectors` | Manage connectors (`git`, `web`, `github`) (admin) |
| POST | `/v1/connectors/:id/run` | Run a connector now (admin) |
| GET | `/v1/projects` | List projects |
| PUT/DELETE | `/v1/projects/:name` | Manage projects (admin) |
| GET/POST | `/v1/webhooks` | List / register outbound webhooks (admin) |
| DELETE | `/v1/webhooks/:id` | Delete a webhook (admin) |
| POST | `/v1/admin/reindex` | Rebuild the index from the filesystem (admin) |
| GET | `/v1/admin/stats` | Store overview stats (admin) |
| POST | `/v1/feedback` | Rate a query answer (`query_id`, `helpful`, `comment?`) |
| GET | `/v1` | Service self-description: info, full endpoint inventory, MCP invocation instructions |
| GET | `/v1/settings` | List runtime settings with metadata; secrets masked (admin) |
| GET/PUT/DELETE | `/v1/settings/:key` | Read/update/delete a runtime setting (admin); hot-applied at runtime |
| GET/POST | `/v1/keys` | List / create API keys (admin); plaintext returned once, stored hashed |
| DELETE | `/v1/keys/:name` | Delete an API key (admin) |

### Runtime configuration (no restart)

Most settings are **runtime-configurable**: precedence is DB override > environment
variable > default. Admins manage them via the settings API (`Authorization: Bearer <key>`):

```bash
curl http://localhost:3000/v1/settings -H "Authorization: Bearer <admin-key>"
curl -X PUT http://localhost:3000/v1/settings/rate_limit_rpm \
```

Hot-appliable keys include: `public_read`, `rate_limit_rpm`, `connector_poll_seconds`,
`llm_base_url`, `llm_api_key` (secret — write-only, masked in listings), `llm_model`,
`llm_embed_model`, `embedding_dims`, `llm_distill`, `okf_strict`, `human_actors`, `layout`,
`embedding_provider` (`none`/`api`/`onnx`/`auto`), `onnx_model`, `onnx_dtype`
(`q8`/`fp16`/`fp32`), `onnx_device`, `max_upload_mb`, and `webhook_secret` (secret — masked).

**Secrets are masked** in `GET /v1/settings` responses.

**Changing `embedding_dims` wipes existing embeddings**; the response includes
`reindex_required: true`. Rebuild vectors afterward:

```bash
curl -X POST http://localhost:3000/v1/admin/reindex -H "Authorization: Bearer <admin-key>"
```

**Immutable deployment-level settings** (`wiki_root`, `port`, `host`, `db_backend`,
`database_url`) are reported via `GET /v1/settings` but a `PUT` returns `405`.

**Bootstrap flow:** `API_KEYS` is optional. A fresh instance with zero configured keys mints
one admin key and prints it to the log exactly once:

```
WikiLLM bootstrap admin key ... shown once
```

Set `BOOTSTRAP_ADMIN_KEY` to pin that secret instead of a random one.

#### ONNX embeddings (local)

Semantic search works with **zero external services**: an in-process transformers.js
embedder (default `Xenova/bge-small-en-v1.5`, q8, 384 dims) runs inside the API. Switch
it on at runtime — no restart, no API key:

```bash
curl -X PUT http://localhost:3000/v1/settings/embedding_dims \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" -d '{"value": 384}'
curl -X PUT http://localhost:3000/v1/settings/embedding_provider \
  -H "Authorization: Bearer <admin-key>" \
  -H "Content-Type: application/json" -d '{"value": "onnx"}'
curl -X POST http://localhost:3000/v1/admin/reindex \
  -H "Authorization: Bearer <admin-key>"
```

`embedding_provider` accepts `none`, `api` (OpenAI-compatible endpoint), `onnx`, or
`auto`. Model, quantization, and device are tunable via `onnx_model`, `onnx_dtype`
(`q8`/`fp16`/`fp32`), and `onnx_device`; on Strix Halo / Ryzen AI hardware, pass an
ONNX execution-provider string through `onnx_device` to target the NPU.

## Post-deploy setup entirely via API/MCP

A fresh deployment needs no shell access to configure — everything happens over the API
(or the equivalent MCP tools). Concrete curl sequence:

```bash
BASE=http://localhost:3000

# 1. Read the bootstrap admin key from the container logs (printed once)
docker compose logs wikillm-api | grep "bootstrap admin key"

# 2. Create a real agent key (plaintext is returned exactly once, stored hashed)
curl -X POST $BASE/v1/keys -H "Authorization: Bearer <bootstrap-key>" \
  -H "Content-Type: application/json" -d '{"name": "agent-main", "role": "admin"}'

# 3. Delete the bootstrap key
curl -X DELETE $BASE/v1/keys/bootstrap -H "Authorization: Bearer <bootstrap-key>"

# 4. Configure the LLM endpoint at runtime (no restart)
curl -X PUT $BASE/v1/settings/llm_base_url -H "Authorization: Bearer <agent-key>" \
  -H "Content-Type: application/json" -d '{"value": "https://api.cerebras.ai/v1"}'
curl -X PUT $BASE/v1/settings/llm_api_key -H "Authorization: Bearer <agent-key>" \
  -H "Content-Type: application/json" -d '{"value": "sk-..."}'
curl -X PUT $BASE/v1/settings/llm_model -H "Authorization: Bearer <agent-key>" \
  -H "Content-Type: application/json" -d '{"value": "llama3.1"}'

# 5. Create a connector and a project
curl -X POST $BASE/v1/connectors -H "Authorization: Bearer <agent-key>" \
  -H "Content-Type: application/json" \
  -d '{"type": "git", "name": "docs", "url": "https://github.com/org/docs"}'
curl -X PUT $BASE/v1/projects/main -H "Authorization: Bearer <agent-key>" \
  -H "Content-Type: application/json" -d '{"description": "Main knowledge base"}'
```

The same flow via MCP tool names (`key_create`, `settings_set`, `connector_create`,
`project_put`): call `key_create` to mint the agent key, `settings_set` for each runtime
setting (`llm_base_url`, `llm_api_key`, `llm_model`, …), then `connector_create` and
`project_put` to wire up sources and projects — all without touching the server.

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

WikiLLM API ships an MCP server (`src/mcp.ts`) exposing **29 tools** covering the entire
control surface: the original retrieval primitives (`search`, `get_concept`, `read_source`,
`list_changes`, `graph_neighbors`, `propose_edit`, `append_log`, `query`, `refresh_index`)
plus full management — `settings_get`/`settings_set`/`settings_delete`/`settings_list`,
`key_create`/`keys_list`/`key_delete`, `project_*`, `connectors_*` (including run),
`admin_reindex`/`admin_stats`, `okf_validate`, `delete_page`, `put_source`, and `add_feedback`.

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

Measured on a Ryzen 9 7950X3D / 62 GiB / Bun 1.3.14 (2026-08-21), SQLite backend,
no embedder configured (FTS retrieval mode):

### Synthetic peak throughput

| Endpoint                                 | Concurrency |   Throughput | Notes |
| ---------------------------------------- | ----------: | -----------: | --- |
| `GET /health`                            |         200 | ~69,000 req/s | |
| `GET /v1/pages/wiki/...`                 |          50 | ~33,000 req/s | |
| `GET /v1/search?q=` (FTS mode)           |          10 |  ~2,300 req/s | FTS5 bm25 |
| `PUT /v1/pages/wiki/...` (same page)     |          10 |   ~3,400 req/s | per-path lock ceiling |
| `PUT` unique page creation               |           5 |   ~2,800 req/s | chunk + graph + ledgers |
| `POST /v1/sources/:path?force=true`      |           5 |   ~4,100 req/s | |
| `POST /v1/ingest`                        |           1 |      ~90 ops/s | source + 2 pages + log + index |

### Realistic workload

| Scenario            | Clients | Think time |   Throughput | p99 latency |
| ------------------- | ------: | ---------: | -----------: | ----------: |
| Read-heavy browsing |     100 |       50ms | ~3,900 req/s |        8 ms |
| Mixed read/write    |      50 |      100ms |   ~977 req/s |        8 ms |
| Write-heavy editing |      25 |       50ms |   ~608 req/s |       48 ms |
| Batch ingestion     |       3 |      200ms |     ~17 ops/s |      213 ms |
| Observer polling    |      10 |      500ms |     ~36 req/s |      171 ms |

### Postgres + pgvector comparison

Read paths are at parity with SQLite. Two write/retrieval paths are currently
slower on Postgres and flagged for optimization (see
[`scripts/benchmark-results.md`](scripts/benchmark-results.md) for details):
contended writes pay multi-round-trip ledger commits (~131 req/s vs ~3,200 on
SQLite at concurrency 5), and FTS ranking computes `ts_rank` for all matches
(~554 req/s vs ~2,100). Vector (HNSW) retrieval requires an embedder and is not
yet benchmarked.

See [`scripts/benchmark-results.md`](scripts/benchmark-results.md) for full
details, methodology notes, and the historical trend.

## Coexistence with Obsidian, git, and sync tools

- Writes use atomic temp-file + rename, so Obsidian never sees partial files.
- The API does not lock files long-term; it only serializes writes from API clients.
- External changes are detected and broadcast over SSE/WebSocket.
- `.obsidian`, `.git`, and temporary files are ignored.

## License

MIT
