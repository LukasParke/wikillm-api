import { loadConfig } from "./config.js";
import { createDatabase, migrate } from "./db/client.js";
import { cleanupTempFiles } from "./fs/atomic.js";
import { syncFullCache } from "./fs/watcher.js";
import { createApp } from "./app.js";
import { createBroadcaster } from "./services/broadcaster.js";
import { createWatcher } from "./fs/watcher.js";

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

const server = Bun.serve({
  hostname: config.HOST,
  port: config.PORT,
  fetch: app.fetch,
  websocket: (app as any).websocket,
});

console.log(`WikiLLM API listening on http://${config.HOST}:${config.PORT}`);
console.log(`Wiki root: ${config.WIKI_ROOT}`);

function shutdown() {
  console.log("Shutting down...");
  watcher.close();
  server.stop(true);
  db.close();
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
