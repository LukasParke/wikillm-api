# WikiLLM API Benchmark Results

## 2026-08-21 run (post agent-control/runtime-config milestone)

Environment: CachyOS (Linux 7.1.5), AMD Ryzen 9 7950X3D (32 threads), 62 GiB RAM,
Bun 1.3.14. Wiki seeded on tmpfs (`/tmp`). No LLM/embedder configured → retrieval
runs in **FTS mode**; vector/HNSW path requires an embedder and is not measured here.

Methodology fixes this round: the previous "unique page creation" and "mixed
workload" autocannon scenarios silently sent malformed requests (`-i` files are
request *bodies* in autocannon, not request scripts) — their old numbers were
invalid and have been replaced. Unique-page creation now uses autocannon
`--idReplacement`; mixed read/write workloads are covered exclusively by the
realistic client script. Source uploads use `?force=true` to measure sustained
overwrite instead of one-shot 409s.

### Synthetic peak throughput (SQLite backend)

| Scenario | Concurrency | Throughput | Notes |
| --- | ---: | ---: | --- |
| `GET /health` | 200 | ~69,000 req/s | |
| `GET /v1/pages/:path` | 50 | ~33,000 req/s | FS read + hash |
| `GET /v1/pages?folder=&limit=50` | 50 | ~4,000 req/s | 100-entity folder |
| `GET /v1/search?q=` (FTS) | 10 | ~2,000–2,300 req/s | FTS5 bm25 over chunks |
| `PUT /v1/pages/:path` (same page) | 10 | ~3,400 req/s | per-path lock ceiling |
| `PUT` unique page creation | 5 | ~2,800 req/s | chunk + edges + ledgers per write |
| `POST /v1/sources/:path?force=true` | 5 | ~4,100 req/s | |
| `POST /v1/log/append` | 1 | ~400 req/s | whole-file rewrite grows with log size |
| `POST /v1/index/refresh` | 1 | ~600 req/s | regenerates + re-chunks `index.md` |
| `GET /v1/changes?limit=100` | 10 | ~5,500 req/s | |
| `GET /v1/changes?path=<hot>` | 10 | ~59 req/s | sort over ~90k rows for one hot path |
| `POST /v1/ingest` (source + 2 pages + log + index) | 1 | ~90 ops/s | |

All scenarios returned 2xx throughout.

### Realistic mixed workloads (SQLite backend)

Seeded 100-page/20-source wiki; probabilistic clients with think times.

| Scenario | Clients | Think | Throughput | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Read-heavy browsing | 100 | 50 ms | ~3,900 req/s | 0.15 ms | 2.0 ms | 8.1 ms |
| Mixed read/write | 50 | 100 ms | ~977 req/s | 0.77 ms | 4.2 ms | 7.5 ms |
| Write-heavy editing | 25 | 50 ms | ~592–623 req/s | 15 ms | 36 ms | 43–53 ms |
| Batch ingestion | 3 | 200 ms | ~17 ops/s | 69 ms | 160 ms | 213 ms |
| Observer polling | 10 | 500 ms | ~36 req/s | 7.2 ms | 102 ms | 171 ms |

Zero errors across all realistic scenarios.

### Postgres + pgvector backend comparison (focused set, 101-page corpus)

| Scenario | SQLite | Postgres | Delta |
| --- | ---: | ---: | --- |
| `GET /v1/pages/:path` | ~32,800 req/s | ~30,600 req/s | parity |
| `GET /v1/search?q=` (FTS) | ~2,100 req/s | ~554 req/s (p99 55 ms) | pg ts_rank scoring path needs optimization |
| `GET /v1/changes?limit=100` | ~5,500 req/s | ~3,300 req/s | parity-ish |
| `PUT` contended (c=5) | ~3,200 req/s | ~131 req/s (p99 59 ms) | multi-round-trip ledger writes; batch into one transaction |

Takeaways:

1. Read paths are backend-parity; the store abstraction costs nothing measurable.
2. The Postgres write path pays ~4-6 sequential round trips per mutation
   (operation, document upsert, chunks transaction, change). Consolidating these
   into a single transaction is the top write-path optimization.
3. Postgres full-text ranking computes `ts_rank` for every match before LIMIT;
   a sub-select rank-then-limit or `ts_rank_cd` tuning is the follow-up.
4. Two known hot-spot degradations on SQLite at high row counts:
   `GET /v1/changes?path=` sorts all rows for that path (add composite
   `(rel_path, detected_at)` index), and `log/append` rewrites the whole
   `log.md`, so throughput falls as the log grows.

## Historical run (2026-06-22, pre-roadmap implementation)

Environment: Arch Linux, same CPU/memory, Bun 1.3.13. Kept for trend reference;
the unique-creation and (absent) mixed rows from this run were invalidated by the
methodology issue described above.

| Endpoint | Concurrency | Throughput | p99 |
| --- | ---: | ---: | ---: |
| `GET /health` | 200 | ~95,000 req/s | 4 ms |
| `GET /v1/pages/wiki/...` | 100 | ~45,000 req/s | 11 ms |
| `PUT /v1/pages/wiki/...` (same page) | 10 | ~4,900 req/s | 3 ms |
| `POST /v1/ingest` | 1 | ~800 ops/s | 2 ms |
