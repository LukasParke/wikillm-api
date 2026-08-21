import { createRequire } from "node:module";
import { randomUUID } from "node:crypto";
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { registerTools } from "./mcp/tools.js";

const require = createRequire(import.meta.url);
const { version } = require("../package.json") as { version: string };

function createMcpServer(): McpServer {
  const server = new McpServer(
    { name: "wikillm", version },
    { capabilities: { tools: {} } },
  );
  registerTools(server);
  return server;
}

function jsonRpcError(
  res: ServerResponse,
  status: number,
  code: number,
  message: string,
): void {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({ jsonrpc: "2.0", error: { code, message }, id: null }),
  );
}

interface RpcRequest {
  method?: unknown;
}

function startHttp(port: number): void {
  const sessions = new Map<string, StreamableHTTPServerTransport>();

  void createServer(async (req: IncomingMessage, res: ServerResponse) => {
    const apiKey = process.env.WIKILLM_API_KEY;
    if (apiKey && req.headers.authorization !== `Bearer ${apiKey}`) {
      jsonRpcError(res, 401, -32001, "Unauthorized");
      return;
    }
    if (req.method !== "POST") {
      // Streamable HTTP GET (SSE stream) and DELETE are intentionally unsupported.
      jsonRpcError(res, 405, -32000, `Method ${req.method ?? "?"} not allowed`);
      return;
    }

    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk as Buffer);
    let body: unknown;
    try {
      body = JSON.parse(Buffer.concat(chunks).toString("utf8")) as unknown;
    } catch {
      jsonRpcError(res, 400, -32700, "Parse error");
      return;
    }

    const sessionId = req.headers["mcp-session-id"];
    const existing =
      typeof sessionId === "string" ? sessions.get(sessionId) : undefined;
    if (existing) {
      await existing.handleRequest(req, res, body);
      return;
    }
    if ((body as RpcRequest)?.method !== "initialize") {
      jsonRpcError(
        res,
        400,
        -32000,
        "Bad Request: no session for mcp-session-id header; send initialize first",
      );
      return;
    }

    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: () => randomUUID(),
      enableJsonResponse: true,
    });
    const server = createMcpServer();
    await server.connect(transport);
    await transport.handleRequest(req, res, body);
    if (transport.sessionId) sessions.set(transport.sessionId, transport);
  }).listen(port, "127.0.0.1", () => {
    console.error(`wikillm MCP server listening on http://127.0.0.1:${port}`);
  });
}

async function main(): Promise<void> {
  const port = process.env.MCP_HTTP_PORT;
  if (port) {
    startHttp(Number(port));
    return;
  }
  const server = createMcpServer();
  await server.connect(new StdioServerTransport());
}

await main();
