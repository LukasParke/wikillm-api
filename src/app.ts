import { Hono } from "hono";
import type { Config } from "./config.js";
import type { Store } from "./store/types.js";
import { authMiddleware, type AuthInfo } from "./middleware/auth.js";
import { rateLimitMiddleware } from "./middleware/rateLimit.js";
import { errorHandler, notFoundHandler } from "./middleware/error.js";
import { metrics } from "./obs/metrics.js";
import type { Broadcaster } from "./services/broadcaster.js";
import type { Services } from "./services/container.js";
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
import rootRoute from "./routes/root.js";
import settingsRoute from "./routes/settings.js";
import keysRoute from "./routes/keys.js";
import queryRoute from "./routes/query.js";
import graphRoute from "./routes/graph.js";
import okfRoute from "./routes/okf.js";
import bundleRoute from "./routes/bundle.js";
import connectorsRoute from "./routes/connectors.js";
import projectsRoute from "./routes/projects.js";
import adminRoute from "./routes/admin.js";
import feedbackRoute from "./routes/feedback.js";
import metricsRoute from "./routes/metrics.js";

export interface AppVariables {
  config: Config;
  store: Store;
  deps: Services;
  source: string;
  auth: AuthInfo;
  broadcaster: Broadcaster;
  validatedBody?: unknown;
  validatedQuery?: unknown;
}

export interface AppDependencies {
  config: Config;
  store: Store;
  services: Services;
  broadcaster: Broadcaster;
}

export function createApp({
  config,
  store,
  services,
  broadcaster,
}: AppDependencies): Hono<{ Variables: AppVariables }> {
  const app = new Hono<{ Variables: AppVariables }>();

  app.use("*", async (c, next) => {
    c.set("config", config);
    c.set("store", store);
    c.set("deps", services);
    c.set("broadcaster", broadcaster);
    return next();
  });
  app.use("*", async (c, next) => {
    const route = c.req.path.split("/").slice(0, 3).join("/") || "/";
    const started = Date.now();
    await next();
    const labels = { method: c.req.method, route };
    metrics.counter("wikillm_requests_total", "HTTP requests processed", {
      ...labels,
      status: String(c.res.status),
    });
    metrics.observe(
      "wikillm_request_duration_seconds",
      "HTTP request latency",
      (Date.now() - started) / 1000,
      labels,
    );
  });

  const publicRead = async (): Promise<boolean> =>
    (await services.settings.get<boolean>("public_read")) ?? config.PUBLIC_READ;
  const rateLimitRpm = async (): Promise<number> =>
    (await services.settings.get<number>("rate_limit_rpm")) ??
    config.RATE_LIMIT_RPM;
  app.use("*", rateLimitMiddleware(rateLimitRpm));
  app.use(
    "*",
    authMiddleware(services.keys, () => publicRead()),
  );

  app.route("/health", health);
  app.route("/metrics", metricsRoute);
  app.route("/v1", rootRoute);
  app.route("/v1/settings", settingsRoute);
  app.route("/v1/keys", keysRoute);
  app.route("/v1/pages", pages);
  app.route("/v1/sources", sources);
  app.route("/v1/index", indexRoute);
  app.route("/v1/log", logRoute);
  app.route("/v1/search", search);
  app.route("/v1/changes", changes);
  app.route("/v1/events", events);
  app.route("/v1/ws", wsRoute);
  app.route("/v1/ingest", ingest);
  app.route("/v1/query", queryRoute);
  app.route("/v1/graph", graphRoute);
  app.route("/v1/okf", okfRoute);
  app.route("/v1/bundle", bundleRoute);
  app.route("/v1/connectors", connectorsRoute);
  app.route("/v1/projects", projectsRoute);
  app.route("/v1/admin", adminRoute);
  app.route("/v1/feedback", feedbackRoute);

  app.notFound(notFoundHandler);
  app.onError(errorHandler);

  return app;
}
