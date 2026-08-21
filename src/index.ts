import { loadConfig } from "./config.js";
import { cleanupTempFiles } from "./fs/atomic.js";
import { createWatcher } from "./fs/watcher.js";
import { createApp } from "./app.js";
import { createBroadcaster } from "./services/broadcaster.js";
import {
  createServices,
  startConnectors,
  stopConnectors,
} from "./services/container.js";

const config = loadConfig();
cleanupTempFiles(config.WIKI_ROOT);

const services = await createServices(config);
const indexed = await services.pipeline.reindexAll();

const broadcaster = createBroadcaster();
services.pipeline.setChangeEmitter((event) => broadcaster.broadcast(event));
const watcher = createWatcher(config.WIKI_ROOT, services.pipeline, broadcaster);
await startConnectors(services);

const app = createApp({
  config,
  store: services.store,
  services,
  broadcaster,
});

// hono/bun attaches its upgrade handler on the app object; Bun.serve accepts
// it at runtime but the handler type is not expressible via hono's public API.
const wsHandler = (app as unknown as { websocket?: never }).websocket;
const server = Bun.serve({
  hostname: config.HOST,
  port: config.PORT,
  fetch: app.fetch,
  ...(wsHandler ? { websocket: wsHandler } : {}),
});
console.log(`WikiLLM API listening on http://${config.HOST}:${config.PORT}`);
console.log(`Wiki root: ${config.WIKI_ROOT}`);
console.log(`Indexed ${indexed} documents at boot`);

function shutdown() {
  console.log("Shutting down...");
  watcher.close();
  stopConnectors(services);
  server.stop(true);
  void services.store.close().finally(() => process.exit(0));
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
