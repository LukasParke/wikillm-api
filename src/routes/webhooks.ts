import { Hono } from "hono";
import { z } from "zod";
import { ulid } from "ulidx";
import type { AppVariables } from "../app.js";

const app = new Hono<{ Variables: AppVariables }>();

function isAdmin(c: { get: (k: "auth") => { role: string } }): boolean {
  return c.get("auth").role === "admin";
}

app.get("/", async (c) => {
  if (!isAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const hooks = await c.get("store").listWebhooks();
  return c.json({
    webhooks: hooks.map((h) => ({
      id: h.id,
      url: h.url,
      events: h.events,
      prefixes: h.prefixes,
      enabled: h.enabled,
      last_status: h.last_status,
      last_attempt_at: h.last_attempt_at,
      created_at: h.created_at,
    })),
  });
});

const createSchema = z.object({
  url: z.string().url(),
  events: z.array(z.literal("change")).min(1).default(["change"]),
  prefixes: z.array(z.string()).min(1).default(["*"]),
  enabled: z.boolean().default(true),
});

app.post("/", async (c) => {
  if (!isAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const parsed = createSchema.safeParse(await c.req.json());
  if (!parsed.success) {
    return c.json({ error: "validation", issues: parsed.error.issues }, 400);
  }
  const store = c.get("store");
  const hook = {
    id: `wh-${ulid().slice(-8).toLowerCase()}`,
    url: parsed.data.url,
    events: parsed.data.events,
    prefixes: parsed.data.prefixes,
    enabled: parsed.data.enabled,
    last_status: null,
    last_attempt_at: null,
    created_at: new Date().toISOString(),
  };
  await store.putWebhook(hook);
  return c.json(hook, 201);
});

app.delete("/:id", async (c) => {
  if (!isAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const removed = await c.get("store").deleteWebhook(c.req.param("id"));
  if (!removed) {
    return c.json({ error: "not_found", message: "Unknown webhook" }, 404);
  }
  return c.json({ success: true });
});

export default app;
