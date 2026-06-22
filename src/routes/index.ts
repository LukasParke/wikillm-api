import { Hono } from "hono";
import type { AppVariables } from "../app.js";
import { createIndexService } from "../services/indexService.js";

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", async (c) => {
  const service = createIndexService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.get();
  return c.json(result);
});

app.post("/refresh", async (c) => {
  const service = createIndexService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.refresh();
  return c.json(result);
});

export default app;
