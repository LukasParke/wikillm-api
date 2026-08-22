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

## 2026-08-22 run — post Foundation-derived memory loop (release build)

Environment: same host (Ryzen 9 7950X3D), SQLite/FTS5, no embedder, no LLM.
Methodology: isolated A/B — pre-change binary (`51787dc`, built in a worktree)
vs new build (`62a0abf`); each server started fresh (empty DB), one warmup
`final_benchmark --docs 100` pass, then a measured pass; servers never ran
concurrently during measurement.

### Latency A/B (p50 ms, identical harness + corpus)

| Endpoint | pre-change | new | delta |
| --- | ---: | ---: | --- |
| `GET /health` | 0.17 | **0.15** | −12% |
| `GET /v1/search` (FTS) | 0.63 | **0.55** | −13% |
| `GET /v1/documents` | 0.22 | **0.16** | −27% |
| `GET /v1/changes?limit=100` | 0.34 | **0.29** | −15% |
| `GET /v1/settings` | 0.18 | **0.17** | parity |

Retrieval quality unchanged: comprehensive suite R@10 = **93.8%** on BOTH
binaries (factoid R@10 100%, MRR 0.349). No quality regression from the
draft-exclusion predicate or RRF/collapse changes.

Memory/knowledge benchmark (pristine server): recall **100% across all six
dimensions** (single_hop, multi_hop, temporal, cross_ref), total precision
39.2% at limit=10, avg latency <1ms. NOTE: running multiple suites against
one shared instance pollutes precision/recall (corpus competition) — always
benchmark against a fresh DB.

### New-surface latencies (added by the memory loop)

| Endpoint | p50 | p95 | p99 | req/s |
| --- | ---: | ---: | ---: | ---: |
| `POST /v1/memory` (store) | 0.28ms | 0.37ms | 0.69ms | ~3.3k |
| `GET /v1/memory` (search+bump) | 0.23ms | 0.30ms | 0.35ms | ~4.1k |
| `GET /v1/memory/:id/history` | 0.15ms | 0.21ms | 0.23ms | ~6.4k |
| `POST /v1/sessions` | 0.32ms | 0.39ms | 0.46ms | ~3.0k |
| `POST /v1/sessions/:id/messages` (heuristic extraction) | 0.20ms | 0.26ms | 0.29ms | ~4.8k |
| `GET /v1/sessions/:id` | 0.75ms | 0.85ms | 1.06ms | ~1.3k |
| `GET /v1/pages/:p/versions` | 0.17ms | 0.22ms | 0.32ms | ~5.6k |
| `GET /v1/pages/:p/versions/:seq` | 0.16ms | 0.22ms | 0.28ms | ~6.0k |
| `GET /v1/pages/:p/diff` (LCS) | 0.18ms | 0.26ms | 0.29ms | ~5.1k |
| `GET /v1/communities` (TTL cached) | 0.15ms | 0.21ms | 0.34ms | ~6.4k |
| `GET /v1/communities/:id/docs` | 0.19ms | 0.27ms | 0.31ms | ~4.9k |
| `GET /v1/admin/gaps` | 0.16ms | 0.21ms | 0.28ms | ~6.1k |

Harness note: `final_benchmark.py`'s memory-recall section previously targeted
a route that never existed (`/v1/memory/search?scope_key=`) and its quality
section queried paths it never seeded — both sections read 0% historically.
Both now exercise the real API (memory recall via `/v1/memory?q=&agent=`).
The 25% memory-recall figure it reports reflects the ledger's substring-LIKE
search ceiling on paraphrased queries — a known limitation, candidate for
FTS-indexing memories.
