# WikiLLM API — Rust Implementation

A self-hostable LLM knowledge base service, rewritten from TypeScript to Rust
using **axum 0.8**, **tokio**, **rusqlite** (bundled FTS5), and
**tokio-postgres** (pgvector).

## Quick start

```bash
cd rust
cargo build --release

# Configure
export WIKI_ROOT=/path/to/wiki
export API_KEYS="admin:your-secret:*:admin"
export DB_BACKEND=sqlite
export DB_PATH=./wikillm.db

# Run
./target/release/wikillm-api
```

With Postgres + pgvector:

```bash
export DB_BACKEND=postgres
export DATABASE_URL=postgres://user:pass@localhost/wikillm
./target/release/wikillm-api
```

## Architecture

```
src/
├── main.rs              # entrypoint: config → store → services → serve
├── lib.rs               # module tree
├── config.rs            # env parsing (API_KEYS grammar, backends, LLM)
├── domain.rs            # shared types (DocumentRecord, ChunkHit, etc.)
├── error.rs             # Error enum + Result alias
├── store/
│   ├── mod.rs           # async Store trait + fts_query helper
│   ├── sqlite.rs        # rusqlite backend (FTS5, WAL)
│   └── pg.rs            # tokio-postgres backend (pgvector HNSW, GIN tsv)
├── fs/
│   ├── paths.rs         # path guards (traversal/reserved/namespace)
│   ├── atomic.rs        # temp+rename writes, SHA-256 hashing
│   ├── lock.rs          # per-path async mutex (FIFO, sorted multi-acquire)
│   └── watcher.rs       # notify-based FS watcher with debounce
├── okf/
│   ├── parse.rs         # frontmatter split, link/wikilink extraction
│   ├── trust.rs         # trust tiers, actor convention, staleness
│   └── validate.rs      # OKF conformance validator
├── ingest/
│   ├── chunkers.rs      # markdown/code chunking (heading-aware)
│   └── pipeline.rs      # parse → chunk → store → edges → embed queue
├── llm/
│   ├── provider.rs      # OpenAI-compatible chat/embeddings client
│   └── embedder.rs      # Embedder trait + api/onnx providers
├── services/
│   ├── search.rs        # hybrid retrieval (RRF K=60), rerank, expansion
│   ├── query.rs         # planner → executor → synthesis pipeline
│   ├── graph.rs         # link-graph traversal (JSON + DOT export)
│   ├── settings.rs      # runtime settings (DB > env > default, hot-applied)
│   ├── keys.rs          # KeyRegistry (env bootstrap + DB hashed keys)
│   ├── project.rs       # project scoping
│   ├── okf_service.rs   # bundle-level OKF validation
│   ├── webhooks.rs      # HMAC-SHA256 signed outbound webhooks
│   ├── broadcaster.rs   # SSE/WS fan-out
│   ├── metrics.rs       # Prometheus text exposition
│   ├── bundle.rs        # tar.gz export/import
│   └── connectors/      # git, web, github connectors
├── http/
│   ├── mod.rs           # axum Router + all handlers
│   ├── auth.rs          # bearer auth middleware
│   └── rate_limit.rs    # fixed-window per-identity limiter
└── mcp/                 # MCP stdio server (41 tools)
```

## Tests

```bash
# All suites (76 tests)
cargo test

# Individual suites
cargo test --test core_rs        # fs/okf/chunkers (42 tests)
cargo test --test retrieval_rs   # RRF fusion + recency + planner (11 tests)
cargo test --test store_rs       # SQLite store roundtrips (10 tests)

# Postgres backend (requires running pgvector instance)
TEST_PG_URL=postgres://user:pass@localhost/test cargo test --test store_rs
```

## Benchmarking

```bash
bash scripts/benchmark.sh
```

Requires `bunx` (for autocannon). Results are written to `/tmp/rust-bench-results.txt`.

## Design decisions

- **Async Store trait**: both backends implement the same `#[async_trait]`
  trait so callers are backend-agnostic.
- **SQLite**: single `Connection` behind `std::sync::Mutex`; FTS5 virtual
  table maintained manually on chunk replace/delete; bm25 scoring via `-bm25()`.
- **Postgres**: `tokio_postgres::Client` behind `tokio::sync::Mutex`;
  generated `tsvector` column + GIN index for FTS; pgvector HNSW index for
  vector search; positional `$n` parameters aligned after simple conditions.
- **Embed queue**: dedicated worker task consuming document IDs from an
  unbounded channel; runs distill (optional) then embed sequentially per doc.
- **Settings**: DB override > env > default with 1-second TTL cache;
  change hooks fire after cache invalidation so hot-appliable knobs take
  effect immediately.
- **Auth**: env keys are immutable bootstrap credentials; DB-managed keys
  store only SHA-256 hashes (plaintext returned once at creation).
- **Webhooks**: deliveries signed with HMAC-SHA256 using the `webhook_secret`
  runtime setting; retries at 250ms / 1s / 4s.

## Known differences from the TypeScript version

| Area | TS | Rust |
|---|---|---|
| HTTP framework | Hono | axum 0.8 |
| SQLite driver | bun:sqlite / better-sqlite3 | rusqlite (bundled) |
| Postgres driver | postgres.js / tokio-postgres | tokio-postgres |
| ONNX embeddings | transformers.js (JS) | feature-gated stub (`onnx` feature) |
| MCP transport | @modelcontextprotocol/sdk | hand-rolled JSON-RPC over stdio |
| Frontmatter parser | gray-matter | serde_yaml between `---` fences |
| Watcher | chokidar | notify v7 |

All REST endpoints, response shapes, error codes, and MCP tool names match
the TypeScript implementation. The two implementations can be used
interchangeably against the same wiki folder and database.
