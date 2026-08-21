# WikiLLM API — Roadmap: Unified Self-Hosted Knowledge Base Service

Status: **implemented** (2026-08-21). Phases A–F shipped; see README for the
current feature set and `docs/openapi.yaml` for the API surface. Update (2026-08-21, later): an in-process ONNX embedder
(transformers.js/onnxruntime-node, default `Xenova/bge-small-en-v1.5` q8, 384
dims) is now shipped as `embedding_provider=onnx`, so semantic search no longer
requires an external API; on AMD Ryzen AI (Strix Halo) machines the underlying
onnxruntime can target NPU execution providers where the platform exposes them.
Remaining deviations from the original proposal: GitHub connector covers
issues/PRs/releases (discussions deferred); the separate IDF retrieval pass is
folded into BM25/ts_rank scoring, which already encodes term rarity.

## 1. Vision

One self-hostable service that is the best way to build, manage, ingest into, and query an
LLM knowledge base:

- **Filesystem stays the source of truth.** The wiki folder is a plain, Obsidian/git-friendly
  markdown tree that _is_ an [OKF](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)
  bundle. Everything derived (indexes, embeddings) is rebuildable cache.
- **Cerebras-pattern retrieval as a first-class deploy**, per
  ["How We Built Our Knowledge Base"](https://www.cerebras.ai/blog/how-we-built-our-knowledge-base):
  one embeddings store, one connector interface per source, LLM distillation, hybrid retrieval
  (full-text + vector + IDF + recency), RRF fusion + rerank, answers with citations,
  MCP tools for agents, project scoping, auth/audit/analytics.
- **Single binary/container experience**: `docker compose up`, point it at a folder, get
  REST + SSE/WS + MCP immediately; LLM features degrade gracefully when no model is configured.

## 2. Target architecture

```
 SOURCES                INGESTION                 STORE                    SERVING
 ───────               ──────────                ─────                    ───────
 wiki folder (FS)  ─┐  chunkers (md/code)   ┌─> Postgres:            ┌─> REST /v1 (pages, sources,
 git repos          ├>  embedder (pluggable) │    documents, chunks,   │    search, query, graph,
 web/HTML           ├>  distiller (LLM, opt.)│    embeddings, FTS      │    changes, events, ws)
 GitHub             ├>  watermark/dedupe     │    connectors, projects,├─> MCP server (search,
 Slack              ┘   incremental by hash  │    queries, feedback    │    get_concept, graph, ...)
 Obsidian (external writes, watcher)           └────────────────────────┤> /v1/query: planner →
                                                                        │   executor → synthesis
 GOVERNANCE: API keys ↔ projects, RBAC, audit log (operations/changes), │> SSE/WS change feeds
 query analytics + feedback, Prometheus metrics                          └> bundle export/import
```

Unchanged principles: atomic writes, OCC (`ifMatch`/ETag), per-path locks, watcher +
broadcaster, attribution on every mutation, DB is disposable cache.

## 3. Gap analysis (current → target)

| Area          | Today                                        | Target                                                                                                                 |
| ------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| Index store   | SQLite page_cache (metadata only)            | Postgres + pgvector + GIN FTS as derived index; SQLite retained as embedded fallback                                   |
| Search        | Substring `LIKE` over title/body/frontmatter | Hybrid: FTS + HNSW vector + IDF + age decay, RRF fusion (K=60), optional rerank, context re-expansion                  |
| Ingestion     | Manual PUT/ingest + FS watcher               | Connector framework (folder, git, web, GitHub, Slack, plugin scripts) with watermarks, dedupe, incremental re-embed    |
| Distillation  | None                                         | Optional LLM extraction (question/summary/resolution/entities/code refs) stored beside raw text                        |
| Standards     | Implicit wiki-llm layout                     | OKF v0.2 conformance: validate, enforce on write, trust/lifecycle fields indexed, graph endpoints, bundle export       |
| Agent access  | REST + SSE/WS                                | + MCP server exposing retrieval primitives (LLM-free tools)                                                            |
| Answering     | None                                         | `/v1/query`: planner → parallel executor → synthesis with citations (requires configured LLM; retrieval works without) |
| Multi-tenancy | Single wiki root, flat API keys              | Projects = named source bundles; keys scoped to projects; default scope per key                                        |
| AuthZ         | `can_write` flag                             | RBAC per key/project (old Phase 6 item)                                                                                |
| Ops           | Compose + GHCR image, benchmarks             | + one-command bring-up incl. Postgres/pgvector, `/metrics`, backup/rebuild runbook, updated benchmarks                 |

## 4. Phases

### Phase A — Index store upgrade (foundation)

- Add Postgres + pgvector as the primary index/ledger store; repository interface so SQLite
  remains an embedded fallback (FTS5; sqlite-vec optional).
- Schema: `documents` (path/source identity, hash, mtime, frontmatter JSONB, trust/lifecycle
  fields), `chunks` (document_id, ordinal, heading path, content, tsv), `embeddings`
  (chunk_id, vector(dim configurable), model, embedded_at), `connectors`, `connector_state`
  (watermarks), `projects`, `project_sources`, `queries` (analytics), `feedback`.
- Dual-run migration: existing `page_cache`/`operations`/`changes` map onto `documents`;
  keep the changes ledger intact.
- Auto-migrate on boot; `DB_BACKEND=postgres|sqlite` env; compose gains a `db` service
  (`pgvector/pgvector` image) with healthcheck.
- Acceptance: existing test suite green against both backends; fresh `docker compose up`
  reaches healthy with zero manual steps; DB deletion + restart fully rebuilds from FS.

### Phase B — OKF conformance layer

- Treat `WIKI_ROOT` as an OKF bundle; layout profiles: `okf` (free-form concepts),
  `wikillm` (legacy `wiki/`+`raw/`), `auto`.
- Validator: `POST /v1/okf/validate` + CLI script — frontmatter parseability, required
  `type`, reserved filenames, link resolution report, trust-tier derivation
  (unverified / machine-confirmed / human-reviewed), staleness (`stale_after`).
- Write-path enforcement (configurable strictness): stamp `generated {by, at}` using the
  actor convention (`agent-<name>/<model>`, `human:<id>`, `process:<id>`) on every API write;
  preserve unknown frontmatter keys on round-trip.
- Index `status`, `verified`, `stale_after`, `sources`, `tags`, `type` as filterable columns;
  search/results expose trust tier + stale flags.
- Graph: `GET /v1/graph/:rel_path` (out/in edges), `GET /v1/graph?root=...` subgraph;
  broken links tolerated per spec.
- `GET /v1/bundle/export` (tarball) / `POST /v1/bundle/import`; `okf_version` honored.
- Align `indexService`/`logService` output with OKF §8/§9 formats (date-grouped log entries,
  sectioned index with descriptions).
- Acceptance: a wiki served by the API passes external OKF validation; round-trip
  export→import preserves hashes except intentionally stamped fields.

### Phase C — Ingestion pipeline

- Chunkers: markdown heading-aware (keep heading path in chunk metadata), code
  language-aware coarse→fine boundaries (class → method → block), thread-aware for chats.
- Embedder abstraction: any OpenAI-compatible `/embeddings` endpoint (incl. Cerebras
  inference, Ollama, LM Studio) + local fallback (fastembed/ONNX). Config: model, dims,
  batch size. No embedder configured → FTS-only mode, everything else still works.
- Distiller (optional, same provider abstraction): per-document structured extraction
  (question, summary, resolution, entities, code_refs) stored on the chunk/document row;
  both raw and distilled text searchable. Prompt templates configurable per source type.
- Incremental reindex: watcher/connector events → hash-compare → re-embed changed chunks
  only (CocoIndex-style sync state in the same DB).
- Connector framework: registration API + `connector_state` watermarks + dedupe IDs;
  ship with: `folder` (existing watcher), `git` (poll/clone, per-commit diffs), `web`
  (URL list → readability-extracted markdown), `github` (issues/PRs/discussions/releases).
  Plugin contract: a connector emits normalized rows into the shared documents/chunks shape.
- Acceptance: seed 10k-page corpus, mutate 100 files externally, observe only changed chunks
  re-embedded; connector resume after restart loses nothing (watermark test).

### Phase D — Hybrid retrieval & answering

- Retrieval primitives (each its own ranked list, Cerebras-style):
  1. Full-text (GIN/tsv) — exact tokens, error strings, identifiers.
  2. Vector (HNSW, cosine) — paraphrase.
  3. IDF-weighted lexical score — rare tokens beat filler.
  4. Recency decay — configurable half-life per source type.
- Fusion: RRF `score(d) = Σ 1/(60 + rank)` across lists; dedupe to source level; per-source
  contribution cap; optional rerank pass (small model via provider abstraction) → top-k;
  context re-expansion (pull neighboring sections/headings for winners).
- `GET /v1/search` upgraded in place (backward-compatible params + new filters:
  `project`, `type`, `tags`, `trust`, `include_stale`, `source`).
- `POST /v1/query` — planner (pick primitives) → executor (parallel fan-out, normalize
  evidence) → synthesis (answer + citations as `{path, hash, quote}` triples). 409-free:
  citations reference immutable hashes. Disabled-with-clear-error when no LLM configured.
- Feedback loop: `POST /v1/feedback` (query_id, helpful boolean, comment) → `feedback`
  table; failed-query logging (zero-result queries) for corpus-gap reports.
- Acceptance: golden-set eval script (queries → expected paths) reporting hit@k before/after;
  latency budget: p99 < 300 ms for search without rerank at 100k chunks.

### Phase E — MCP server + integrations

- MCP server (stdio + Streamable HTTP) exposing LLM-free primitive tools:
  `search`, `get_concept`, `list_changes`, `graph_neighbors`, `read_source`,
  `propose_edit` (goes through normal OCC write path). Same auth (API key), same audit trail.
- Slack connector + optional bot (Socket Mode): thread-as-document ingestion with burst
  chunking (author-run bursts, topic-prepended, IDF/length/reaction gating) per the
  Cerebras design; `@mention` → `/v1/query` with citations.
- Minimal web UI (static, served by the app): search + read + change feed; not a priority
  beyond making self-hosted demos instant.
- Acceptance: Claude Code connects via MCP and completes "find X, read it, propose an edit"
  end-to-end against a live instance.

### Phase F — Governance, ops, hardening

- Projects: CRUD API, key↔project binding, default scope per key; all search/query/graph
  routes accept project scope; cross-project access denied unless key allows.
- RBAC per key (read/write/admin per project) — closes old Phase 6 item.
- Analytics: query volume, latency histograms, zero-hit rate, per-source freshness lag;
  Prometheus `/metrics`; simple `/v1/stats`.
- Audit: operations/changes ledger already covers mutations; extend to queries (opt-in).
- Ops: backup/restore runbook (FS + Postgres dump + watermark state), rate limiting,
  request body limits, security review of MCP surface, updated benchmark suite
  (synthetic + realistic, now including hybrid search and query pipelines).
- Docs: rewrite README around the three personas (human wiki maintainer, agent via MCP,
  admin deploying); keep Obsidian coexistence guide.

## 5. Decisions to confirm (defaults chosen; reversible)

1. **Postgres as default index backend** — recommended: RRF over FTS+vector+decay is native
   there, matches Cerebras exactly; SQLite stays as embedded fallback for small deploys.
   Alternative: sqlite-only (sqlite-vec) to keep single-process purity — slower path to
   Phase D quality.
2. **Embedding default** — local ONNX model out-of-the-box (no API key needed to evaluate),
   OpenAI-compatible endpoint for production. Dims configurable (default 1536).
3. **Connector priority after folder/git/web**: GitHub before Slack (Slack needs workspace
   OAuth review; GitHub is token-only). Confirm if Slack must lead.
4. **Naming/positioning** — keep repo/name `wikillm-api`, position as "knowledge base
   service"; or rename. Cosmetic, decide before README rewrite.

## 6. References

- Cerebras, "How We Built Our Knowledge Base" (2026-07-15) — one embeddings table, connector
  interface, distillation, bursting, hybrid retrieval + RRF(K=60) + rerank, planner/executor/
  synthesis, MCP primitives, projects.
- Google Cloud, Open Knowledge Format spec v0.2 — bundle structure, required `type`,
  `sources`/`generated`/`verified`/`status`/`stale_after`, actor convention, Attested
  Computations, permissive conformance, `okf_version`.
