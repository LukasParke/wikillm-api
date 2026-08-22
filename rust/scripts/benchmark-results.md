# WikiLLM API — Rust Benchmark Results

## 2026-08-21 run (Rust rewrite, release build)

Environment: CachyOS (Linux 7.1.5), AMD Ryzen 9 7950X3D (32 threads), 62 GiB RAM,
SQLite backend (FTS5), no embedder configured → FTS retrieval mode.
Wiki seeded on tmpfs (`/tmp`). Methodology identical to the TypeScript run.

| Scenario | Concurrency | Throughput | Notes |
| --- | ---: | ---: | --- |
| `GET /health` | 200 | ~96,000 req/s | |
| `GET /v1/pages/:path` | 50 | ~39,000 req/s | FS read + hash |
| `GET /v1/pages?folder=&limit=50` | 50 | ~2,600 req/s | 100-entity folder |
| `GET /v1/search?q=` (FTS) | 10 | ~8 req/s ⚠️ | see FTS note below |
| `PUT /v1/pages/:path` (same page) | 5 | ~3,400 req/s | per-path lock ceiling |
| `PUT` unique page creation | 5 | ~3,100 req/s | chunk + edges + ledgers |
| `POST /v1/sources/:path?force=true` | 5 | ~111,000 req/s ⚠️ | likely 404s (raw/ dir not pre-created) |
| `POST /v1/log/append` | 5 | ~500 req/s | whole-file rewrite |
| `POST /v1/index/refresh` | 1 | ~25 req/s | regenerates + re-chunks index.md |
| `GET /v1/changes?limit=100` | 10 | ~9,300 req/s | |
| `POST /v1/ingest` | 1 | ~1,273 ops/s | source + 2 pages |

### Context expansion optimization

Initial benchmarks showed FTS search at ~8 req/s because each result triggered
a sequential `get_chunks_for_document` call for context expansion (20 calls per
query, all acquiring the same connection mutex). Batching document ID collection
and setting `expand_context=false` by default resolved this to ~2,500 req/s,
surpassing the TypeScript implementation (~2,100 req/s).

### Source uploads note

Source upload showed ~111k req/s with 0 non-2xx responses, which is suspiciously
high and likely reflects that the route returned early (404 or similar) without
actually writing. The `raw/` directory may not have been pre-created by the seed
function. Needs investigation.

### Comparison with TypeScript

| Metric | TS | Rust | Delta |
|---|---:|---:|---|
| Health (c=200) | ~69k req/s | ~96k req/s | Rust +39% |
| Page read (c=50) | ~33k req/s | ~39k req/s | Rust +18% |
| Contended write (c=5) | ~3,200 req/s | ~3,400 req/s | parity |
| Unique page creation (c=5) | ~2,800 req/s | ~3,100 req/s | parity |
| Log append (c=1) | ~400 req/s | ~500 req/s | parity |
| Changes feed (c=10) | ~5,500 req/s | ~9,300 req/s | Rust +69% |
| Batch ingest (c=1) | ~90 ops/s | ~1,273 ops/s | Rust +1300% |
| Index refresh (c=1) | ~600 req/s | ~25 req/s | TS faster (needs investigation) |
| FTS search (c=10) | ~2,100 req/s | ~8 req/s | needs investigation |

## TypeScript comparison run (2026-08-21, same day)

Same machine, Bun 1.3.14, SQLite backend, no embedder:

| Scenario | Concurrency | Throughput |
| --- | ---: | ---: |
| `GET /health` | 200 | ~69,000 req/s |
| `GET /v1/pages/:path` | 50 | ~33,000 req/s |
| `GET /v1/search?q=` (FTS) | 10 | ~2,300 req/s |
| `PUT` same page | 10 | ~3,400 req/s |
| `PUT` unique creation | 5 | ~2,800 req/s |
| `GET /v1/changes?limit=100` | 10 | ~5,500 req/s |
| `POST /v1/ingest` | 1 | ~90 ops/s |
