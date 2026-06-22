import { Hono } from "hono";
import type { Config } from "./config.js";
import type { Database } from "./db/client.js";
import { authMiddleware } from "./middleware/auth.js";
import { errorHandler, notFoundHandler } from "./middleware/error.js";
import type { Broadcaster } from "./services/broadcaster.js";
import changes from "./routes/changes.js";
import events from "./routes/events.js";
import health from "./routes/health.js";
import indexRoute from "./routes/index.js";
import ingest from "./routes/ingest.js";
import logRoute from "./routes/log.js";
import pages from "./routes/pages.js";
import search from "./routes/search.js";
import sources from "./routes/sources.js";
import wsRoute from "./routes/ws.js";

export interface AppVariables {
  config: Config;
  db: Database;
  source: string;
  broadcaster: Broadcaster;
  validatedBody?: unknown;
  validatedQuery?: unknown;
}

export interface AppDependencies {
  config: Config;
  db: Database;
  broadcaster: Broadcaster;
}

export function createApp({
  config,
  db,
  broadcaster,
}: AppDependencies): Hono<{ Variables: AppVariables }> {
  const app = new Hono<{ Variables: AppVariables }>();

  app.use("*", async (c, next) => {
    c.set("config", config);
    c.set("db", db);
    c.set("broadcaster", broadcaster);
    return next();
  });

  app.use("*", authMiddleware(config.API_KEYS, config.PUBLIC_READ));

  app.route("/health", health);
  app.route("/v1/pages", pages);
  app.route("/v1/sources", sources);
  app.route("/v1/index", indexRoute);
  app.route("/v1/log", logRoute);
  app.route("/v1/search", search);
  app.route("/v1/changes", changes);
  app.route("/v1/events", events);
  app.route("/v1/ws", wsRoute);
  app.route("/v1/ingest", ingest);

  app.notFound(notFoundHandler);
  app.onError(errorHandler);

  return app;
}
