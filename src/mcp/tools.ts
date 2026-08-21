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
}
