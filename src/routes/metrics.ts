import { Hono } from "hono";
import type { AppVariables } from "../app.js";
import { metrics } from "../obs/metrics.js";

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", (c) => {
  return new Response(metrics.render(), {
    headers: { "Content-Type": "text/plain; version=0.0.4" },
  });
});

export default app;
