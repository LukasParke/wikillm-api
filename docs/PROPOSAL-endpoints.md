# Proposal: Knowledge-Base API Surface Expansion

Status: draft, 2026-08-21. Builds on the shipped v1 surface (pages, sources,
search, query, graph, OKF, connectors, settings, keys). Goal: make the API a
complete *library* interface — browse anything, export anything, move data in
bulk — so agents and tools never need filesystem access.

## P0 — Close the browsing gap

### `GET /v1/documents`

General listing across every indexed document (wiki pages, raw sources,
connector docs). The store already implements this primitive
(`Store.listDocuments`); only the route is missing. Today connector content is
searchable but not enumerable, which blocks "what's in the KB?" workflows.

```
GET /v1/documents?kind=page|source|doc
                &origin=<connectorId|wiki>
                &folder=<prefix>
                &type=<okfType>&tags=a,b&status=&trust=
                &fresh=true&project=<name>
                &limit=&cursor=
→ { items: [{ rel_path, kind, origin, title, okf_type, tags, status,
              stale_after, trust, hash, mtime, updated_at, updated_by }],
    nextCursor? }
```

Reuses the exact filter machinery of `/v1/search`; no new store code.
MCP tool: `documents_list`. Effort: ~half a day including tests.

### Raw content download

Pages return JSON envelopes today; sources return metadata only — there is no
way to download original bytes over HTTP.

```
GET /v1/pages/:rel_path/raw          → text/markdown body (+ ETag)
GET /v1/sources/:rel_path/content    → original bytes (+ Content-Type, ETag)
GET /v1/documents/:rel_path/content  → dispatches by kind (convenience)
```

ETag = content hash, so agents get conditional `If-None-Match` caching free.
MCP tools: `download_page`, `download_source`. Effort: ~half a day.

## P1 — Bulk operations

### Filtered bundle export

`GET /v1/bundle/export` exists but is all-or-nothing. Add scope filters:

```
GET /v1/bundle/export?prefix=wiki/entities&kind=page&origin=wiki
GET /v1/bundle/export?project=compiler&since=2026-08-01T00:00:00Z
```

Implementation: same tar stream with a path filter; `since` uses the changes
ledger for incremental sync (agents can implement pull-based replication).
MCP tool: `export_bundle` returning a temp file path or base64 chunking needs
deciding (prefer: tool returns a one-time download URL served by the API).

### Batch write endpoint

Multi-file writes exist via `/v1/ingest`, but plain page mutations are
one-request-per-file. For agent refactors ("rename concept across 40 pages"):

```
POST /v1/pages/batch
{ "operations": [ { rel_path, content?, frontmatter?, ifMatch?, delete? } ] }
→ per-op results, partial-failure semantics identical to /v1/ingest
```

Reuses ingestService locking (sorted multi-lock) without the source/log/index
side effects. MCP tool: `pages_batch`. Effort: ~1 day.

## P2 — Quality-of-life

- **Bulk delete**: `POST /v1/documents/delete { rel_paths[], ifMatch? }` with
  the same partial-failure contract.
- **Collection ETags**: `ETag` on list responses derived from max(mtime)+count,
  enabling cheap re-polls for sync clients.
- **Graph export**: `GET /v1/graph/:path?format=dot|json` for visualization
  tooling (JSON shape already exists; DOT is trivial).
- **Asset upload**: `POST /v1/sources/:path/binary` streaming multipart for
  large binaries into `raw/assets/` with size caps (current JSON/base64 path
  is fine below ~10 MB).
- **Outbound webhooks**: `POST /v1/webhooks { url, events, filters }` —
  server-side push complement to SSE for agents behind NATs. Largest item;
  needs delivery/retry semantics.

## Non-goals (explicitly)

- Per-document version history (git remains the version store; the changes
  ledger covers audit).
- Query-language search (SQL-like); hybrid search + filters cover real use.

## Suggested order

P0 items first (small, unlock agent browsing + downloads), then batch write,
then P2 by demand. Every endpoint ships with its MCP tool in the same change,
per the "fully agent-controllable" invariant.
