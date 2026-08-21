import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";

const app = new Hono<{ Variables: AppVariables }>();

function isAdmin(c: { get: (k: "auth") => { role: string } }): boolean {
  return c.get("auth").role === "admin";
}

app.get("/", async (c) => {
  if (!isAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const keys = await c.get("store").listApiKeys();
  return c.json({
    keys: keys.map((k) => ({
      name: k.name,
      key_prefix: k.key_prefix,
      role: k.role,
      scope: k.scope,
      created_at: k.created_at,
      created_by: k.created_by,
    })),
    note: "Env-configured API_KEYS are not listed; DB-managed keys shown.",
  });
});

const createSchema = z.object({
  name: z.string().min(1).max(64).optional(),
  role: z.enum(["admin", "write", "read"]).default("write"),
  scope: z.array(z.string()).min(1).default(["*"]),
});

app.post("/", async (c) => {
  if (!isAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const parsed = createSchema.safeParse(await c.req.json());
  if (!parsed.success) {
    return c.json({ error: "validation", issues: parsed.error.issues }, 400);
  }
  try {
    const created = await c
      .get("deps")
      .keys.createKey({ ...parsed.data, createdBy: c.get("source") });
    // The plaintext secret is returned exactly once and never stored.
    return c.json(created, 201);
  } catch (err) {
    return c.json({ error: "conflict", message: (err as Error).message }, 409);
  }
});

app.delete("/:name", async (c) => {
  if (!isAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const name = c.req.param("name");
  const removed = await c.get("deps").keys.deleteKey(name);
  if (!removed) {
    return c.json({ error: "not_found", message: `Unknown key: ${name}` }, 404);
  }
  return c.json({ success: true, deleted: name });
});

export default app;
