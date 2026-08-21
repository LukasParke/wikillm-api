import { Hono } from "hono";
import type { AppVariables } from "../app.js";

/**
 * Self-description for agents and clients: what this service is, which
 * surfaces exist, and where the machine-readable docs live.
 */
const app = new Hono<{ Variables: AppVariables }>();

app.get("/", (c) => {
  const config = c.get("config");
  return c.json({
    service: "wikillm-api",
    version: "0.2.0",
    description:
      "Self-hosted LLM knowledge base: OKF bundle management, hybrid retrieval, ingestion connectors. Fully controllable via this API or MCP.",
    wiki_root: config.WIKI_ROOT,
    surfaces: {
      rest: "/v1",
      openapi: "/docs/openapi.yaml (repo) — published on GitHub Pages",
      health: "/health",
      metrics: "/metrics",
      mcp: {
        stdio: "bun run src/mcp.ts",
        http: "MCP_HTTP_PORT=<port> bun run src/mcp.ts (WIKILLM_URL + WIKILLM_API_KEY env)",
      },
    },
    endpoints: [
      "GET    /health",
      "GET    /metrics",
      "GET    /v1 (this document)",
      "CRUD   /v1/pages/:rel_path (OCC via ifMatch)",
      "CRUD   /v1/sources/:rel_path (raw/ namespace)",
      "GET/POST /v1/index(/refresh)",
      "GET/POST /v1/log(/append)",
      "GET    /v1/search?q=&type=&tags=&status=&trust=&fresh=&origin=&project=",
      "POST   /v1/query {question}",
      "GET    /v1/changes?since=&path=&source=&limit=",
      "GET    /v1/events (SSE) | GET /v1/ws (WebSocket)",
      "POST   /v1/ingest {source, operations[], logEntry?}",
      "GET    /v1/graph/:rel_path?depth=1..3",
      "POST   /v1/okf/validate | GET /v1/okf/layout",
      "GET    /v1/bundle/export | POST /v1/bundle/import?force= (admin)",
      "CRUD   /v1/connectors (+ POST /:id/run) (admin)",
      "GET/PUT/DELETE /v1/projects/:name",
      "GET/PUT/DELETE /v1/settings/:key (admin writes; runtime-applied)",
      "GET/POST/DELETE /v1/keys (admin; hashed storage)",
      "POST   /v1/admin/reindex | GET /v1/admin/stats (admin)",
      "POST   /v1/feedback {query_id, helpful}",
    ],
    notes: [
      "All admin surfaces are usable over MCP with equivalent tools.",
      "Settings changes apply at runtime; embedding_dims requires a reindex.",
    ],
  });
});

export default app;
