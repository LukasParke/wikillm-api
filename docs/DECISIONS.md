# WikiLLM API — Design Decisions

Decisions confirmed with the project owner on 2026-06-22.

## Runtime

**Decision:** Use **Bun** as the primary runtime.

- Native TypeScript execution.
- Fast startup and low memory footprint.
- Hono is heavily optimized for Bun.
- We will keep the code runnable on Node 20+ with `tsx` as a fallback, but CI/dev defaults to Bun.

## Authentication

**Decision:** Use **multiple named API keys** for the MVP.

- Each key maps to a source identifier such as `agent-codex`, `user-luke`, or `clipper`.
- Authorization header: `Authorization: Bearer <key>`.
- The source name is recorded in:
  - the `operations` table,
  - the `changes` table,
  - and optionally frontmatter fields `updated_by` / `updated_at`.
- Read access may be left public by default (configurable via `PUBLIC_READ=true|false`).

## Real-time updates

**Decision:** Provide **both Server-Sent Events (SSE) and native WebSockets** so different tools/agents can pick their integration method.

- **SSE endpoint:** `GET /v1/events` — simple HTTP one-way stream, ideal for browsers and simple scripts.
- **WebSocket endpoint:** `GET /v1/ws` — native Bun WebSocket upgrade via `hono/bun`, lower overhead and bidirectional-capable, ideal for agents and long-lived connections.
- Both endpoints broadcast the same `ChangeEvent` payload (path, change type, hash, source, timestamp).
- A shared broadcaster service keeps SSE and WebSocket clients in sync.
- The Bun server entry point exports `{ fetch: app.fetch, websocket }` so `Bun.serve` can route upgrade requests.

## Conflict resolution

**Decision:** Use **strict optimistic concurrency control (OCC)**.

- Every resource exposes a SHA-256 `etag` / hash.
- `PUT` requests must include `If-Match` or an `ifMatch` body field with the hash the client read.
- If the file changed in the meantime, the API returns `409 Conflict` with the current content and hash.
- The client (LLM agent, script, or UI) is responsible for diffing/merging and retrying.
- This keeps the server simple and avoids silent overwrites from Obsidian or other agents.

## Multi-user model

**Decision:** Shared wiki, attributed writes.

- No per-user copies of the wiki.
- Every write is attributed to a source via API key mapping.
- Fine-grained RBAC is a future feature; MVP uses a single `can_write` flag per key.

## Filesystem coexistence

**Decision:** Remain a polite citizen.

- Use atomic temp-file + rename writes.
- Keep files open only for the shortest possible window.
- Ignore `.obsidian`, `.git`, `node_modules`, and temporary files.
- Record external changes via the file watcher so API clients can react.
