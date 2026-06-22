import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { serve } from "@hono/node-server";
import { WebSocketServer, type WebSocket } from "ws";
import { loadConfig } from "./config.js";
import { createDatabase, migrate } from "./db/client.js";
import { cleanupTempFiles } from "./fs/atomic.js";
import { syncFullCache, createWatcher } from "./fs/watcher.js";
import { createApp } from "./app.js";
import { createBroadcaster } from "./services/broadcaster.js";
import type { WSContext } from "hono/ws";

const config = loadConfig();
const db = await createDatabase(config.DB_PATH ?? "wikillm-api.db");
migrate(db);
cleanupTempFiles(config.WIKI_ROOT);
syncFullCache(config.WIKI_ROOT, db);

const broadcaster = createBroadcaster();
const watcher = createWatcher(config.WIKI_ROOT, db, {
  onChange: (event) => broadcaster.broadcast(event),
});

const app = createApp({ config, db, broadcaster });

const wss = new WebSocketServer({ noServer: true });

wss.on("connection", (ws: WebSocket) => {
  broadcaster.addWS(ws as unknown as WSContext<unknown>);
  ws.on("close", () =>
    broadcaster.removeWS(ws as unknown as WSContext<unknown>),
  );
  ws.on("message", (data: Buffer | ArrayBuffer | Buffer[]) => {
    try {
      const msg = JSON.parse(data.toString());
      if (msg.type === "ping") {
        ws.send(
          JSON.stringify({ type: "pong", time: new Date().toISOString() }),
        );
      }
    } catch {
      // ignore
    }
  });
});

const server = serve(
  {
    fetch: app.fetch,
    port: config.PORT,
    hostname: config.HOST,
  } as any,
  {
    createServer: (options: any, handler: any) => {
      const srv = createServer(handler);
      srv.on("upgrade", (request, socket, head) => {
        if (request.url?.startsWith("/v1/ws")) {
          wss.handleUpgrade(request, socket, head, (ws) => {
            wss.emit("connection", ws, request);
          });
        } else {
          socket.destroy();
        }
      });
      return srv;
    },
  } as any,
);

console.log(
  `WikiLLM API (Node) listening on http://${config.HOST}:${config.PORT}`,
);
console.log(`Wiki root: ${config.WIKI_ROOT}`);

function shutdown() {
  console.log("Shutting down...");
  watcher.close();
  server.close();
  wss.close();
  db.close();
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
