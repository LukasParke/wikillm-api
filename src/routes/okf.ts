import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";

const app = new Hono<{ Variables: AppVariables }>();

const validateSingleSchema = z.object({ content: z.string() });

app.post("/validate", async (c) => {
  let body: unknown;
  try {
    body = await c.req.json();
  } catch {
    body = null;
  }
  const parsed = validateSingleSchema.safeParse(body);
  if (parsed.success) {
    const issues = c.get("deps").okf.validateSingle(parsed.data.content);
    return c.json({ valid: issues.length === 0, issues });
  }
  const report = await c.get("deps").okf.validateWikiBundle();
  return c.json(report);
});

app.get("/layout", (c) => {
  return c.json({ profile: c.get("deps").okf.layoutProfile() });
});

export default app;
