import { createServer, type IncomingMessage } from "node:http";
import { serve } from "@hono/node-server";
import { WebSocketServer, type WebSocket } from "ws";
import { loadConfig } from "./config.js";
import { cleanupTempFiles } from "./fs/atomic.js";
import { createWatcher } from "./fs/watcher.js";
import { createApp } from "./app.js";
import { createBroadcaster } from "./services/broadcaster.js";
import type { ChangeEvent } from "./types/index.js";
import {
  createServices,
  startConnectors,
  stopConnectors,
} from "./services/container.js";
import type { WSContext } from "hono/ws";

const config = loadConfig();
cleanupTempFiles(config.WIKI_ROOT);

const services = await createServices(config);
await services.pipeline.reindexAll();

const broadcaster = createBroadcaster();
const emitChange = (event: ChangeEvent["data"]) => {
  broadcaster.broadcast({ type: "change", data: event });
  void services.webhooks.dispatch(event);
};
services.pipeline.setChangeEmitter(emitChange);
const watcherBroadcaster = {
  broadcast: (event: ChangeEvent) => {
    broadcaster.broadcast(event);
    void services.webhooks.dispatch(event.data);
  },
};
const watcher = createWatcher(
  config.WIKI_ROOT,
  services.pipeline,
  watcherBroadcaster,
);
await startConnectors(services);

const app = createApp({
  config,
  store: services.store,
  services,
  broadcaster,
});

const wss = new WebSocketServer({ noServer: true });

wss.on("connection", (ws: WebSocket) => {
  broadcaster.addWS(ws as unknown as WSContext<unknown>);
  ws.on("close", () =>
    broadcaster.removeWS(ws as unknown as WSContext<unknown>),
  );
  ws.on("message", (data: Buffer | ArrayBuffer | Buffer[]) => {
    try {
      const msg = JSON.parse(data.toString()) as { type?: string };
      if (msg.type === "ping") {
        ws.send(
          JSON.stringify({ type: "pong", time: new Date().toISOString() }),
        );
      }
    } catch {
      // ignore non-JSON frames
    }
  });
});

const server = serve({
  fetch: app.fetch,
  port: config.PORT,
  hostname: config.HOST,
});

server.on("upgrade", (request, socket, head) => {
  if (request.url?.startsWith("/v1/ws")) {
    wss.handleUpgrade(request, socket, head, (ws) => {
      wss.emit("connection", ws, request);
    });
  } else {
    socket.destroy();
  }
});

console.log(
  `WikiLLM API (Node) listening on http://${config.HOST}:${config.PORT}`,
);
console.log(`Wiki root: ${config.WIKI_ROOT}`);

function shutdown() {
  console.log("Shutting down...");
  watcher.close();
  stopConnectors(services);
  server.close();
  wss.close();
  void services.store.close().finally(() => process.exit(0));
}

process.on("SIGINT", shutdown);
