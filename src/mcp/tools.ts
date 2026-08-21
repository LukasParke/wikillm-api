import { z } from "zod";
import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";

const WIKILLM_URL = process.env.WIKILLM_URL ?? "http://127.0.0.1:3000";
const WIKILLM_API_KEY = process.env.WIKILLM_API_KEY ?? "";

class ApiError extends Error {
  readonly status: number;
  readonly body: string;

  constructor(status: number, statusText: string, body: string) {
    super(`WikiLLM API ${status} ${statusText}: ${body}`);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

/** Thin client for the running WikiLLM API. Throws ApiError on non-2xx. */
async function api(path: string, init?: RequestInit): Promise<unknown> {
  const headers = new Headers(init?.headers);
  if (WIKILLM_API_KEY)
    headers.set("Authorization", `Bearer ${WIKILLM_API_KEY}`);
  headers.set("Content-Type", "application/json");

  const res = await fetch(`${WIKILLM_URL}${path}`, { ...init, headers });
  const body = await res.text();
  if (!res.ok)
    throw new ApiError(res.status, res.statusText, body.slice(0, 800));
  try {
    return JSON.parse(body) as unknown;
  } catch {
    return body;
  }
}

function encPath(path: string): string {
  return path.split("/").map(encodeURIComponent).join("/");
}

async function run(handler: () => Promise<string>): Promise<CallToolResult> {
  try {
    return { content: [{ type: "text", text: await handler() }] };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return {
      content: [{ type: "text", text: `Error: ${message}` }],
      isError: true,
    };
  }
}

interface SearchHit {
  rel_path?: string;
  heading_path?: string | null;
  snippet?: string;
  hash?: string;
  score?: number;
}

export function registerTools(server: McpServer): void {
  server.registerTool(
    "search",
    {
      description:
        "Full-text search across the WikiLLM knowledge base. Returns matching chunks with their page path, heading, snippet, and content hash.",
      inputSchema: {
        q: z.string().describe("Query text"),
        limit: z.number().int().min(1).max(100).default(10).optional(),
        project: z.string().optional(),
      },
    },
    async ({ q, limit, project }) =>
      run(async () => {
        const params = new URLSearchParams({ q });
        params.set("limit", String(limit ?? 10));
        if (project) params.set("project", project);
        const data = await api(`/v1/search?${params.toString()}`);
        const record = data as Record<string, unknown>;
        const results: SearchHit[] = Array.isArray(data)
          ? data
          : Array.isArray(record["results"])
            ? record["results"]
            : [];
        if (results.length === 0) return `No results for ${JSON.stringify(q)}.`;
        return results
          .map((hit) => {
            const heading = hit.heading_path ?? "";
            const hash =
              typeof hit.hash === "string" && hit.hash ? hit.hash : "(no hash)";
            return `${hit.rel_path} :: ${heading}\n${hit.snippet ?? ""}\n(${hash})`;
          })
          .join("\n\n");
      }),
  );

  server.registerTool(
    "get_concept",
    {
      description:
        "Fetch a wiki page by path: frontmatter summary plus markdown body (truncated to 4000 chars).",
      inputSchema: {
        path: z.string().describe("Page path, e.g. 'concepts/occ.md'"),
      },
    },
    async ({ path }) =>
      run(async () => {
        const page = (await api(`/v1/pages/${encPath(path)}`)) as Record<
          string,
          unknown
        >;
        const lines: string[] = [];
        for (const key of [
          "rel_path",
          "title",
          "summary",
          "hash",
          "mtime",
          "word_count",
        ] as const) {
          const value = page[key];
          if (value !== undefined && value !== null)
            lines.push(`${key}: ${String(value)}`);
        }
        const links = page["outgoing_links"];
        if (Array.isArray(links) && links.length > 0)
          lines.push(`links: ${links.join(", ")}`);
        const fm = page["frontmatter"];
        if (
          fm !== undefined &&
          fm !== null &&
          Object.keys(fm as object).length > 0
        ) {
          lines.push(`\n--- frontmatter ---\n${JSON.stringify(fm, null, 2)}`);
        }
        const rawBody =
          typeof page["body"] === "string"
            ? page["body"]
            : typeof page["content"] === "string"
              ? page["content"]
              : "";
        if (rawBody) {
          const body =
            rawBody.length > 4000
              ? `${rawBody.slice(0, 4000)}\n… [truncated]`
              : rawBody;
          lines.push(`\n--- body ---\n${body}`);
        }
        return lines.join("\n");
      }),
  );

  server.registerTool(
    "read_source",
    {
      description:
        "Read source-document metadata (path, size, hash, content type, mtime) by path.",
      inputSchema: {
        path: z.string().describe("Source path within the wiki root"),
      },
    },
    async ({ path }) =>
      run(async () => {
        const meta = (await api(`/v1/sources/${encPath(path)}`)) as Record<
          string,
          unknown
        >;
        const mtimeRaw =
          meta["mtime"] ?? meta["mtimeMs"] ?? meta["modified_at"];
        const mtimeNum = Number(mtimeRaw);
        const mtime =
          Number.isFinite(mtimeNum) && mtimeNum > 0
            ? new Date(mtimeNum * (mtimeNum < 1e12 ? 1000 : 1)).toISOString()
            : typeof mtimeRaw === "string" && mtimeRaw
              ? mtimeRaw
              : "(unknown)";
        return [
          `path: ${(meta["path"] ?? meta["rel_path"] ?? path) as string}`,
          `size: ${meta["size"] ?? "(unknown)"}`,
          `hash: ${meta["hash"] ?? "(unknown)"}`,
          `content_type: ${meta["content_type"] ?? "(unknown)"}`,
          `mtime: ${mtime}`,
        ].join("\n");
      }),
  );

  server.registerTool(
    "list_changes",
    {
      description:
        "List recent changes (writes, ingests, deletes) recorded by the WikiLLM API.",
      inputSchema: {
        limit: z.number().int().min(1).max(1000).default(20).optional(),
      },
    },
    async ({ limit }) =>
      run(async () => {
        const params = new URLSearchParams({ limit: String(limit ?? 20) });
        return JSON.stringify(
          await api(`/v1/changes?${params.toString()}`),
          null,
          2,
        );
      }),
  );

  server.registerTool(
    "graph_neighbors",
    {
      description:
        "Traverse the wiki link graph around a page up to the given depth.",
      inputSchema: {
        path: z.string().describe("Page path to start from"),
        depth: z.number().int().min(1).max(5).default(1).optional(),
      },
    },
    async ({ path, depth }) =>
      run(async () => {
        const params = new URLSearchParams({ depth: String(depth ?? 1) });
        return JSON.stringify(
          await api(`/v1/graph/${encPath(path)}?${params.toString()}`),
          null,
          2,
        );
      }),
  );

  server.registerTool(
    "propose_edit",
    {
      description:
        "Optimistically write a page. If expected_hash is omitted, the current page hash is fetched first; a stale hash yields a 409 conflict explaining OCC.",
      inputSchema: {
        path: z.string().describe("Page path to write"),
        content: z.string().describe("New markdown body"),
        frontmatter: z.record(z.unknown()).optional(),
        expected_hash: z
          .string()
          .optional()
          .describe("Hash the edit is based on (OCC guard)"),
      },
    },
    async ({ path, content, frontmatter, expected_hash }) =>
      run(async () => {
        let hash = expected_hash;
        if (!hash) {
          const current = (await api(`/v1/pages/${encPath(path)}`)) as Record<
            string,
            unknown
          >;
          const currentHash = current["hash"];
          if (typeof currentHash !== "string" || !currentHash) {
            throw new Error(
              "could not determine current hash for the page; pass expected_hash explicitly.",
            );
          }
          hash = currentHash;
        }
        try {
          const result = await api(`/v1/pages/${encPath(path)}`, {
            method: "PUT",
            body: JSON.stringify({ content, frontmatter, ifMatch: hash }),
          });
          return JSON.stringify(result, null, 2);
        } catch (err) {
          if (err instanceof ApiError && err.status === 409) {
            return [
              "Conflict (HTTP 409): the page changed since your expected_hash was taken.",
              "This is optimistic concurrency control (OCC): re-read the page, merge your",
              "changes with the newer content, and retry with a fresh expected_hash.",
              "",
              "Conflict payload:",
              err.body,
            ].join("\n");
          }
          throw err;
        }
      }),
  );

  server.registerTool(
    "append_log",
    {
      description: "Append an entry to the knowledge-base log/journal.",
      inputSchema: {
        message: z.string().describe("Log message"),
        kind: z.string().optional(),
      },
    },
    async ({ message, kind }) =>
      run(async () =>
        JSON.stringify(
          await api("/v1/log/append", {
            method: "POST",
            body: JSON.stringify({ message, kind }),
          }),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "query",
    {
      description:
        "Ask a natural-language question against the knowledge base (LLM-backed; returns llm_not_configured errors verbatim if no LLM provider is set up).",
      inputSchema: {
        question: z
          .string()
          .describe("Question to answer from the knowledge base"),
      },
    },
    async ({ question }) =>
      run(async () =>
        JSON.stringify(
          await api("/v1/query", {
            method: "POST",
            body: JSON.stringify({ question }),
          }),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "refresh_index",
    {
      description: "Trigger a re-index of the knowledge base.",
      inputSchema: {},
    },
    async () =>
      run(async () => JSON.stringify(await api("/v1/index/refresh"), null, 2)),
  );

  server.registerTool(
    "settings_list",
    {
      description:
        "List all WikiLLM settings with type, default, current value, and override state. Secret values are masked.",
      inputSchema: {},
    },
    async () =>
      run(async () => {
        const data = (await api("/v1/settings")) as Record<string, unknown>;
        const settings = Array.isArray(data["settings"])
          ? (data["settings"] as Record<string, unknown>[])
          : [];
        if (settings.length === 0) return "No settings found.";
        return settings
          .map((s) => {
            const key = String(s["key"] ?? "(unknown)");
            const type = String(s["type"] ?? "?");
            const isSecret =
              s["value"] === undefined ||
              s["value"] === null ||
              (typeof s["value"] === "string" &&
                /^\*+$/.test(s["value"] as string));
            const value = isSecret ? "<masked>" : JSON.stringify(s["value"]);
            const def = JSON.stringify(s["default"]);
            const overridden = s["overridden"] ? ", overridden" : "";
            return `${key} (${type}, default=${def}, value=${value}${overridden})`;
          })
          .join("\n");
      }),
  );
  server.registerTool(
    "settings_get",
    {
      description: "Fetch a single setting view by key.",
      inputSchema: { key: z.string().describe("Setting key") },
    },
    async ({ key }) =>
      run(async () =>
        JSON.stringify(
          await api(`/v1/settings/${encodeURIComponent(key)}`),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "settings_set",
    {
      description:
        "Set a setting value. Value may be a string, number, boolean, array, or object; it is sent as-is. Notes when a reindex is required.",
      inputSchema: {
        key: z.string().describe("Setting key"),
        value: z
          .union([
            z.string(),
            z.number(),
            z.boolean(),
            z.array(z.unknown()),
            z.record(z.unknown()),
          ])
          .describe("New value"),
      },
    },
    async ({ key, value }) =>
      run(async () => {
        const result = await api(`/v1/settings/${encodeURIComponent(key)}`, {
          method: "PUT",
          body: JSON.stringify({ value }),
        });
        const record = result as Record<string, unknown>;
        if (record["reindex_required"] === true)
          return `Set ${key}.\nNOTE: reindex required for this change to take effect — call admin_reindex.`;
        return `Set ${key}.\n${JSON.stringify(result, null, 2)}`;
      }),
  );

  server.registerTool(
    "settings_reset",
    {
      description: "Reset a setting back to its env/default value.",
      inputSchema: { key: z.string().describe("Setting key") },
    },
    async ({ key }) =>
      run(
        async () =>
          `Reset ${key}.\n${JSON.stringify(
            await api(`/v1/settings/${encodeURIComponent(key)}`, {
              method: "DELETE",
            }),
            null,
            2,
          )}`,
      ),
  );

  server.registerTool(
    "keys_list",
    {
      description:
        "List API keys (prefix only — plaintext keys are never returned after creation).",
      inputSchema: {},
    },
    async () =>
      run(async () => {
        const data = (await api("/v1/keys")) as Record<string, unknown>;
        const keys = Array.isArray(data["keys"])
          ? (data["keys"] as Record<string, unknown>[])
          : [];
        if (keys.length === 0) return "No API keys.";
        return keys
          .map((k) =>
            [
              String(k["name"] ?? "(unnamed)"),
              String(k["key_prefix"] ?? ""),
              `role=${String(k["role"] ?? "?")}`,
              `scope=${JSON.stringify(k["scope"])}`,
              k["created_at"] !== undefined
                ? `created=${String(k["created_at"])}`
                : "",
              k["created_by"] !== undefined
                ? `by=${String(k["created_by"])}`
                : "",
            ]
              .filter(Boolean)
              .join(" "),
          )
          .join("\n");
      }),
  );

  server.registerTool(
    "key_create",
    {
      description:
        "Create an API key. The plaintext key is shown ONCE in the output — store it immediately.",
      inputSchema: {
        name: z.string().optional(),
        role: z.enum(["admin", "write", "read"]).default("write").optional(),
        scope: z.array(z.string()).default(["*"]).optional(),
      },
    },
    async ({ name, role, scope }) =>
      run(async () => {
        const body: Record<string, unknown> = {};
        if (name !== undefined) body["name"] = name;
        if (role !== undefined) body["role"] = role;
        if (scope !== undefined) body["scope"] = scope;
        const result = await api("/v1/keys", {
          method: "POST",
          body: JSON.stringify(body),
        });
        const record = result as Record<string, unknown>;
        return [
          "WARNING: this plaintext key is shown ONLY once — save it now.",
          `key: ${String(record["key"] ?? "(missing)")}`,
          `prefix: ${String(record["key_prefix"] ?? "")}`,
          `role: ${String(record["role"] ?? "")}`,
          `scope: ${JSON.stringify(record["scope"])}`,
        ].join("\n");
      }),
  );

  server.registerTool(
    "key_delete",
    {
      description: "Delete an API key by name.",
      inputSchema: { name: z.string().describe("Key name") },
    },
    async ({ name }) =>
      run(async () =>
        JSON.stringify(
          await api(`/v1/keys/${encodeURIComponent(name)}`, {
            method: "DELETE",
          }),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "projects_list",
    {
      description: "List configured wiki projects.",
      inputSchema: {},
    },
    async () =>
      run(async () => JSON.stringify(await api("/v1/projects"), null, 2)),
  );

  server.registerTool(
    "project_put",
    {
      description: "Create or update a project (path prefixes are required).",
      inputSchema: {
        name: z.string().describe("Project name"),
        prefixes: z
          .array(z.string())
          .min(1)
          .describe("Path prefixes served by this project"),
        description: z.string().optional(),
        connectors: z.array(z.string()).optional(),
      },
    },
    async ({ name, prefixes, description, connectors }) =>
      run(async () => {
        const body: Record<string, unknown> = { prefixes };
        if (description !== undefined) body["description"] = description;
        if (connectors !== undefined) body["connectors"] = connectors;
        return JSON.stringify(
          await api(`/v1/projects/${encodeURIComponent(name)}`, {
            method: "PUT",
            body: JSON.stringify(body),
          }),
          null,
          2,
        );
      }),
  );

  server.registerTool(
    "project_delete",
    {
      description: "Delete a project by name.",
      inputSchema: { name: z.string().describe("Project name") },
    },
    async ({ name }) =>
      run(async () =>
        JSON.stringify(
          await api(`/v1/projects/${encodeURIComponent(name)}`, {
            method: "DELETE",
          }),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "connectors_list",
    {
      description: "List configured connectors (admin).",
      inputSchema: {},
    },
    async () =>
      run(async () => JSON.stringify(await api("/v1/connectors"), null, 2)),
  );

  server.registerTool(
    "connector_create",
    {
      description: "Create a connector of kind git, web, or github.",
      inputSchema: {
        kind: z.enum(["git", "web", "github"]),
        config: z
          .record(z.unknown())
          .describe("Connector-specific configuration"),
        id: z.string().optional(),
        enabled: z.boolean().optional(),
      },
    },
    async ({ kind, config, id, enabled }) =>
      run(async () => {
        const body: Record<string, unknown> = { kind, config };
        if (id !== undefined) body["id"] = id;
        if (enabled !== undefined) body["enabled"] = enabled;
        return JSON.stringify(
          await api("/v1/connectors", {
            method: "POST",
            body: JSON.stringify(body),
          }),
          null,
          2,
        );
      }),
  );

  server.registerTool(
    "connector_delete",
    {
      description: "Delete a connector by id.",
      inputSchema: { id: z.string().describe("Connector id") },
    },
    async ({ id }) =>
      run(async () =>
        JSON.stringify(
          await api(`/v1/connectors/${encodeURIComponent(id)}`, {
            method: "DELETE",
          }),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "connector_run",
    {
      description:
        "Trigger a connector run and report how many documents were ingested.",
      inputSchema: { id: z.string().describe("Connector id") },
    },
    async ({ id }) =>
      run(async () => {
        const result = await api(
          `/v1/connectors/${encodeURIComponent(id)}/run`,
          {
            method: "POST",
          },
        );
        const record = result as Record<string, unknown>;
        if (Array.isArray(record["docs"]))
          return `Ingested ${record["docs"].length} document(s).\n${JSON.stringify(result, null, 2)}`;
        return JSON.stringify(result, null, 2);
      }),
  );

  server.registerTool(
    "admin_reindex",
    {
      description: "Trigger a full administrative reindex.",
      inputSchema: {},
    },
    async () =>
      run(async () =>
        JSON.stringify(
          await api("/v1/admin/reindex", { method: "POST" }),
          null,
          2,
        ),
      ),
  );

  server.registerTool(
    "admin_stats",
    {
      description:
        "Get an overview of index/document counts and other admin stats.",
      inputSchema: {},
    },
    async () =>
      run(async () => {
        const stats = (await api("/v1/admin/stats")) as Record<string, unknown>;
        const lines: string[] = [];
        for (const [key, value] of Object.entries(stats)) {
          if (
            value !== null &&
            typeof value === "object" &&
            !Array.isArray(value)
          ) {
            for (const [k, v] of Object.entries(
              value as Record<string, unknown>,
            ))
              lines.push(`${key}.${k}: ${String(v)}`);
          } else {
            lines.push(
              `${key}: ${Array.isArray(value) ? value.length : String(value)}`,
            );
          }
        }
        return lines.length > 0
          ? lines.join("\n")
          : JSON.stringify(stats, null, 2);
      }),
  );

  server.registerTool(
    "okf_validate",
    {
      description:
        "Validate the knowledge base against the OKF bundle spec; reports validity, errors, warnings, and stats.",
      inputSchema: {},
    },
    async () =>
      run(async () => {
        const report = (await api("/v1/okf/validate", {
          method: "POST",
        })) as Record<string, unknown>;
        const valid = report["valid"] === true ? "valid" : "INVALID";
        const errors = Array.isArray(report["errors"])
          ? (report["errors"] as unknown[]).length
          : 0;
        const warnings = Array.isArray(report["warnings"])
          ? (report["warnings"] as unknown[]).length
          : 0;
        return [
          `bundle ${valid} (${errors} error(s), ${warnings} warning(s))`,
          JSON.stringify(report, null, 2),
        ].join("\n");
      }),
  );

  server.registerTool(
    "delete_page",
    {
      description:
        "Delete a page. Like propose_edit, the current hash is fetched first unless expected_hash is given and sent via If-Match.",
      inputSchema: {
        path: z.string().describe("Page path to delete"),
        expected_hash: z
          .string()
          .optional()
          .describe("Hash the delete is based on (OCC guard)"),
      },
    },
    async ({ path, expected_hash }) =>
      run(async () => {
        let hash = expected_hash;
        if (!hash) {
          const current = (await api(`/v1/pages/${encPath(path)}`)) as Record<
            string,
            unknown
          >;
          const currentHash = current["hash"];
          if (typeof currentHash !== "string" || !currentHash) {
            throw new Error(
              "could not determine current hash for the page; pass expected_hash explicitly.",
            );
          }
          hash = currentHash;
        }
        try {
          const result = await api(`/v1/pages/${encPath(path)}`, {
            method: "DELETE",
            headers: { "If-Match": hash },
          });
          return JSON.stringify(result, null, 2);
        } catch (err) {
          if (err instanceof ApiError && err.status === 409) {
            return [
              "Conflict (HTTP 409): the page changed since your expected_hash was taken.",
              "Re-read the page and retry with a fresh expected_hash.",
              "",
              "Conflict payload:",
              err.body,
            ].join("\n");
          }
          throw err;
        }
      }),
  );

  server.registerTool(
    "put_source",
    {
      description:
        "Create or overwrite a raw source document. Set force to bypass conflict checks.",
      inputSchema: {
        path: z.string().describe("Source path within the wiki root"),
        content: z.string().describe("Raw source content"),
        force: z.boolean().default(false).optional(),
      },
    },
    async ({ path, content, force }) =>
      run(async () => {
        const suffix = force ? "?force=true" : "";
        return JSON.stringify(
          await api(`/v1/sources/${encPath(path)}${suffix}`, {
            method: "POST",
            body: JSON.stringify({ content }),
          }),
          null,
          2,
        );
      }),
  );

  server.registerTool(
    "add_feedback",
    {
      description: "Submit feedback on a query result.",
      inputSchema: {
        query_id: z.string().describe("Query id the feedback refers to"),
        helpful: z.boolean().describe("Whether the answer was helpful"),
        comment: z.string().optional(),
      },
    },
    async ({ query_id, helpful, comment }) =>
      run(async () => {
        const body: Record<string, unknown> = { query_id, helpful };
        if (comment !== undefined) body["comment"] = comment;
        return JSON.stringify(
          await api("/v1/feedback", {
            method: "POST",
            body: JSON.stringify(body),
          }),
          null,
          2,
        );
      }),
  );
}
