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

## Running with Docker

```bash
docker compose up --build
```

Mount your wiki folder into `/wiki`.

## Running tests

```bash
bun run test:run
```

## Coexistence with Obsidian, git, and sync tools

- Writes use atomic temp-file + rename, so Obsidian never sees partial files.
- The API does not lock files long-term; it only serializes writes from API clients.
- External changes are detected and broadcast over SSE/WebSocket.
- `.obsidian`, `.git`, and temporary files are ignored.

## License

MIT
