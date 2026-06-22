import { Hono } from "hono";
import type { AppVariables } from "../app.js";

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", (c) => {
  const config = c.get("config");
  return c.json({
    status: "ok",
    version: "0.1.0",
    wiki_root: config.WIKI_ROOT,
    public_read: config.PUBLIC_READ,
  });
});

export default app;
