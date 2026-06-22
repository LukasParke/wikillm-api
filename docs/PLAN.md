# WikiLLM API — Implementation Plan

This document turns the architecture into concrete phases so work can proceed incrementally.

## Phase 0: Foundation (project setup)

- [ ] Initialize repo: `package.json`, `tsconfig.json`, `.gitignore`, `.env.example`.
- [ ] Add runtime tooling (Bun + `tsx` fallback).
- [ ] Add dependencies: `hono`, `zod`, `gray-matter`, `chokidar`, `better-sqlite3` (or `libsql`), `pino`, `ulid`.
- [ ] Add dev dependencies: `vitest`, `supertest`, `@types/node`.
- [ ] Create `src/config.ts` with env validation.
- [ ] Create `src/index.ts` that loads config and starts the Hono server.
  - Export `{ fetch: app.fetch, websocket }` when running under Bun so `Bun.serve` can handle WebSocket upgrades.
  - Node fallback uses `@hono/node-server`.
- [ ] Add `/health` route.

## Phase 1: Safe filesystem layer

- [ ] `src/fs/paths.ts`: relative-path validation, traversal guard, reserved paths.
- [ ] `src/fs/atomic.ts`: atomic read/write with temp-file + rename.
- [ ] `src/fs/lock.ts`: per-path async mutex with sorted multi-lock acquisition.
- [ ] `src/fs/wiki.ts`: list/read helpers for wiki pages and raw sources.
- [ ] Unit tests for path validation, atomic writes, and locking.

## Phase 2: Database + watcher

- [ ] `src/db/schema.sql`: `operations`, `page_cache`, `changes` tables.
- [ ] `src/db/migrations.ts`: simple versioned migration runner.
- [ ] `src/db/client.ts`: typed SQLite client wrapper.
- [ ] `src/fs/watcher.ts`: chokidar watcher, debounce, ignore patterns.
- [ ] `src/services/changeTracker.ts`: reconcile watcher events into `changes` and `page_cache`.
- [ ] On-startup cache sync (full scan or delta).

## Phase 3: Core REST API

- [ ] Middleware: auth (Bearer API key), request logging, error handling, Zod validation.
- [ ] `routes/pages.ts`: `GET`, `PUT`, `DELETE` wiki pages with OCC.
- [ ] `routes/sources.ts`: `GET`, `POST`, `DELETE` raw sources (write-once).
- [ ] `routes/index.ts`: `GET` and `POST /index/refresh`.
- [ ] `routes/log.ts`: `GET` and `POST /log/append`.
- [ ] `routes/search.ts`: basic title/body/frontmatter search.
- [ ] `routes/changes.ts`: activity feed.
- [ ] `routes/events.ts`: Server-Sent Events stream.
- [ ] `routes/ws.ts`: WebSocket change feed using `hono/bun` (`upgradeWebSocket`).
- [ ] Shared broadcaster service: pushes the same `ChangeEvent` to SSE and WebSocket clients.

## Phase 4: Multi-file ingestion

- [ ] `src/services/ingestService.ts`: batch ingestion with sorted locking + OCC checks.
- [ ] `routes/ingest.ts`: `POST /v1/ingest`.
- [ ] Operation logging with parent/child operation IDs.
- [ ] Frontmatter auto-stamping (`updated_at`, `updated_by`).

## Phase 5: Integration, tests, docs

- [ ] Integration tests covering concurrent writes, external file changes, and Obsidian-style renames.
- [ ] WebSocket integration test using Bun's native `WebSocket` client.
- [ ] Dockerize.
- [ ] README with quickstart, env vars, and API examples (including SSE and WebSocket snippets).
- [ ] Decision log for auth, runtime, real-time, and conflict-handling choices.

## Phase 6: Hardening / future

- [ ] RBAC per API key.
- [ ] Git commit/push endpoints.
- [ ] Vector search adapter.
- [ ] Obsidian Web Clipper-compatible endpoint.
