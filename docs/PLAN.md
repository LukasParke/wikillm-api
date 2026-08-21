# WikiLLM API — Implementation Plan

This document turns the architecture into concrete phases so work can proceed incrementally.

## Phase 0: Foundation (project setup)

- [x] Initialize repo: `package.json`, `tsconfig.json`, `.gitignore`, `.env.example`.
- [x] Add runtime tooling (Bun + `tsx` fallback).
- [x] Add dependencies: `hono`, `zod`, `gray-matter`, `chokidar`, `better-sqlite3` (or `libsql`), `pino`, `ulid`.
- [x] Add dev dependencies: `vitest`, `supertest`, `@types/node`.
- [x] Create `src/config.ts` with env validation.
- [x] Create `src/index.ts` that loads config and starts the Hono server.
  - Export `{ fetch: app.fetch, websocket }` when running under Bun so `Bun.serve` can handle WebSocket upgrades.
  - Node fallback uses `@hono/node-server`.
- [x] Add `/health` route.

## Phase 1: Safe filesystem layer

- [x] `src/fs/paths.ts`: relative-path validation, traversal guard, reserved paths.
- [x] `src/fs/atomic.ts`: atomic read/write with temp-file + rename.
- [x] `src/fs/lock.ts`: per-path async mutex with sorted multi-lock acquisition.
- [x] `src/fs/wiki.ts`: list/read helpers for wiki pages and raw sources.
- [x] Unit tests for path validation, atomic writes, and locking.

## Phase 2: Database + watcher

- [x] `src/db/schema.sql`: `operations`, `page_cache`, `changes` tables.
- [x] `src/db/migrations.ts`: simple versioned migration runner.
- [x] `src/db/client.ts`: typed SQLite client wrapper.
- [x] `src/fs/watcher.ts`: chokidar watcher, debounce, ignore patterns.
- [x] `src/services/changeTracker.ts`: reconcile watcher events into `changes` and `page_cache`.
- [x] On-startup cache sync (full scan or delta).

## Phase 3: Core REST API

- [x] Middleware: auth (Bearer API key), request logging, error handling, Zod validation.
- [x] `routes/pages.ts`: `GET`, `PUT`, `DELETE` wiki pages with OCC.
- [x] `routes/sources.ts`: `GET`, `POST`, `DELETE` raw sources (write-once).
- [x] `routes/index.ts`: `GET` and `POST /index/refresh`.
- [x] `routes/log.ts`: `GET` and `POST /log/append`.
- [x] `routes/search.ts`: basic title/body/frontmatter search.
- [x] `routes/changes.ts`: activity feed.
- [x] `routes/events.ts`: Server-Sent Events stream.
- [x] `routes/ws.ts`: WebSocket change feed using `hono/bun` (`upgradeWebSocket`).
- [x] Shared broadcaster service: pushes the same `ChangeEvent` to SSE and WebSocket clients.

## Phase 4: Multi-file ingestion

- [x] `src/services/ingestService.ts`: batch ingestion with sorted locking + OCC checks.
- [x] `routes/ingest.ts`: `POST /v1/ingest`.
- [x] Operation logging with parent/child operation IDs.
- [x] Frontmatter auto-stamping (`updated_at`, `updated_by`).

## Phase 5: Integration, tests, docs

- [x] Integration tests covering concurrent writes, external file changes, and Obsidian-style renames.
- [x] WebSocket integration test using Bun's native `WebSocket` client.
- [x] Dockerize.
- [x] README with quickstart, env vars, and API examples (including SSE and WebSocket snippets).
- [x] Decision log for auth, runtime, real-time, and conflict-handling choices.

## Phase 6: Hardening / future

- [x] RBAC per API key.
- [x] Git commit/push endpoints.
- [x] Vector search adapter.
- [x] Obsidian Web Clipper-compatible endpoint.
