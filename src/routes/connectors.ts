import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateBody } from "../middleware/validate.js";

const createSchema = z.object({
  kind: z.string().min(1),
  id: z.string().optional(),
  config: z.record(z.unknown()).default({}),
  enabled: z.boolean().default(true),
});

const app = new Hono<{ Variables: AppVariables }>();

app.use("*", async (c, next) => {
  if (c.get("auth").role !== "admin") {
    return c.json({ error: "forbidden" }, 403);
  }
  return next();
});

app.get("/", async (c) => {
  const connectors = await c.get("deps").connectors.listConnectors();
  return c.json({ connectors });
});

app.post("/", validateBody(createSchema), async (c) => {
  const body = c.get("validatedBody") as z.infer<typeof createSchema>;
  try {
    const cfg = await c.get("deps").connectors.put({
      id: body.id,
      kind: body.kind,
      config: body.config,
      enabled: body.enabled,
    });
    return c.json(cfg, 201);
  } catch (err) {
    return c.json(
      {
        error: "invalid_connector",
        message: err instanceof Error ? err.message : String(err),
      },
      400,
    );
  }
});

app.delete("/:id", async (c) => {
  const id = c.req.param("id") ?? "";
  const deleted = await c.get("deps").connectors.delete(id);
  if (!deleted) {
    return c.json(
      { error: "not_found", message: `Connector not found: ${id}` },
      404,
    );
  }
  return c.json({ success: true });
});

app.post("/:id/run", async (c) => {
  const id = c.req.param("id") ?? "";
  try {
    const docs = await c.get("deps").connectors.runConnector(id);
    return c.json({ docs });
  } catch (err) {
    return c.json(
      {
        error: "run_failed",
        message: err instanceof Error ? err.message : String(err),
      },
      400,
    );
  }
});

export default app;
