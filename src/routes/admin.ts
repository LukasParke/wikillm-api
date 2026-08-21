import { Hono } from "hono";
import type { AppVariables } from "../app.js";

const app = new Hono<{ Variables: AppVariables }>();

app.use("*", async (c, next) => {
  if (c.get("auth").role !== "admin") {
    return c.json({ error: "forbidden" }, 403);
  }
  return next();
});

app.post("/reindex", async (c) => {
  const documents = await c.get("deps").pipeline.reindexAll();
  return c.json({ documents });
});

app.get("/stats", async (c) => {
  const stats = await c.get("store").statsOverview();
  return c.json(stats);
});

export default app;
