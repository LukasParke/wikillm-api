# WikiLLM API

A TypeScript/Hono HTTP API that sits **alongside** a wiki-llm folder (the Karpathy-style LLM wiki pattern). It lets multiple users, agents, and automations read and write the same markdown wiki and raw sources while Obsidian, git, LiveSync, or other tools are also touching the same directory tree.

The filesystem remains the source of truth. The API is a disciplined, concurrency-aware gateway on top of it.

## Features

- **Filesystem as source of truth** — SQLite is only a cache/ledger.
- **Atomic writes** — temp-file + rename so readers never see partial files.
- **Optimistic concurrency control** — every resource has a SHA-256 `etag` / hash; stale writes return `409 Conflict`.
- **Multi-source attribution** — every API key maps to a source name (`agent-codex`, `user-luke`, etc.).
- **File watcher** — detects external changes from Obsidian/git/LiveSync and records them.
- **Live updates** — both **Server-Sent Events** and **WebSocket** feeds broadcast filesystem changes.
- **Batch ingestion** — update a source, many wiki pages, `log.md`, and `index.md` in one request.

## Quick start

```bash
# 1. Install dependencies
bun install

# 2. Configure environment
cp .env.example .env
# Edit .env and set WIKI_ROOT and API_KEYS

# 3. Run
bun run dev
```

The API will be available at `http://localhost:3000`.

## Configuration

| Variable      | Required | Default            | Description                                                    |
| ------------- | -------- | ------------------ | -------------------------------------------------------------- |
| `WIKI_ROOT`   | yes      | —                  | Path to the wiki-llm folder                                    |
| `API_KEYS`    | yes      | —                  | Comma-separated `name:key` pairs, e.g. `agent-codex:secret123` |
| `PORT`        | no       | `3000`             | HTTP port                                                      |
| `HOST`        | no       | `0.0.0.0`          | Bind address                                                   |
| `PUBLIC_READ` | no       | `true`             | Allow unauthenticated read access                              |
| `DB_PATH`     | no       | `./wikillm-api.db` | SQLite cache/ledger path                                       |
| `LOG_LEVEL`   | no       | `info`             | `trace`, `debug`, `info`, `warn`, `error`                      |

## API overview

All routes are under `/v1` unless noted.

| Method | Route                   | Description                                       |
| ------ | ----------------------- | ------------------------------------------------- |
| GET    | `/health`               | Health check                                      |
| GET    | `/v1/pages`             | List wiki pages                                   |
| GET    | `/v1/pages/:rel_path`   | Read a page                                       |
| PUT    | `/v1/pages/:rel_path`   | Create or update a page (OCC via `ifMatch`)       |
| DELETE | `/v1/pages/:rel_path`   | Delete a page                                     |
| GET    | `/v1/sources`           | List raw sources                                  |
| GET    | `/v1/sources/:rel_path` | Read source metadata                              |
| POST   | `/v1/sources/:rel_path` | Upload a source (write-once unless `?force=true`) |
| DELETE | `/v1/sources/:rel_path` | Delete a source                                   |
| GET    | `/v1/index`             | Read `index.md` + structured catalog              |
| POST   | `/v1/index/refresh`     | Regenerate `index.md`                             |
| GET    | `/v1/log`               | Read `log.md`                                     |
| POST   | `/v1/log/append`        | Append to `log.md`                                |
| GET    | `/v1/search?q=...`      | Search pages                                      |
| GET    | `/v1/changes`           | Recent changes feed                               |
| GET    | `/v1/events`            | SSE live change stream                            |
| GET    | `/v1/ws`                | WebSocket live change stream                      |
| POST   | `/v1/ingest`            | Batch ingestion                                   |

## Examples

### Create a page

```bash
curl -X PUT http://localhost:3000/v1/pages/wiki/entities/OpenAI.md \
  -H "Authorization: Bearer secret123" \
  -H "Content-Type: application/json" \
  -d '{
    "content": "# OpenAI\n\nOpenAI is an AI research company.",
    "frontmatter": { "tags": ["ai"] }
  }'
```

### Update with concurrency check

```bash
curl -X PUT http://localhost:3000/v1/pages/wiki/entities/OpenAI.md \
  -H "Authorization: Bearer secret123" \
  -H "Content-Type: application/json" \
  -d '{
    "content": "# OpenAI\n\nUpdated text.",
    "ifMatch": "<hash-from-previous-read>"
  }'
```

### Ingest a source and update many pages

```bash
curl -X POST http://localhost:3000/v1/ingest \
  -H "Authorization: Bearer secret123" \
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
| Mixed read/write    |      50 |      100ms |   ~970 req/s |        8 ms |
| Write-heavy editing |      25 |       50ms |   ~190 req/s |        1 ms |
| Batch ingestion     |       3 |      200ms |    ~19 ops/s |       42 ms |
| Observer polling    |      10 |      500ms |     ~7 req/s |    2,750 ms |

See [`scripts/benchmark-results.md`](scripts/benchmark-results.md) for full details and raw output.

## Coexistence with Obsidian, git, and sync tools

- Writes use atomic temp-file + rename, so Obsidian never sees partial files.
- The API does not lock files long-term; it only serializes writes from API clients.
- External changes are detected and broadcast over SSE/WebSocket.
- `.obsidian`, `.git`, and temporary files are ignored.

## License

MIT
