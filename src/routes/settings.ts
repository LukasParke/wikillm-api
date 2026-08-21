import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import type { AuthInfo } from "../services/projectService.js";
import {
  ImmutableSettingError,
  UnknownSettingError,
} from "../services/settingsService.js";

const app = new Hono<{ Variables: AppVariables }>();

function requireAdmin(c: { get: (k: "auth") => AuthInfo }): boolean {
  return c.get("auth").role === "admin";
}

// List every setting with metadata, live value, and override state.
app.get("/", async (c) => {
  const settings = c.get("deps").settings;
  return c.json({ settings: await settings.describe() });
});

app.get("/:key", async (c) => {
  const key = c.req.param("key");
  const all = await c.get("deps").settings.describe();
  const entry = all.find((s) => s.key === key);
  if (!entry) {
    return c.json(
      { error: "not_found", message: `Unknown setting: ${key}` },
      404,
    );
  }
  return c.json(entry);
});

const setSchema = z.object({ value: z.unknown() });

app.put("/:key", async (c) => {
  if (!requireAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const key = c.req.param("key");
  const parsed = setSchema.safeParse(await c.req.json());
  if (!parsed.success) {
    return c.json({ error: "validation", issues: parsed.error.issues }, 400);
  }
  try {
    const result = await c
      .get("deps")
      .settings.set(key, parsed.data.value, c.get("source"));
    return c.json({
      key,
      value:
        key === "llm_api_key"
          ? typeof parsed.data.value === "string" &&
            parsed.data.value.length > 0
            ? "<set>"
            : "<unset>"
          : parsed.data.value,
      reindex_required: result.reindexRequired || undefined,
      applied: true,
    });
  } catch (err) {
    if (err instanceof UnknownSettingError) {
      return c.json({ error: "not_found", message: err.message }, 404);
    }
    if (err instanceof ImmutableSettingError) {
      return c.json({ error: "immutable", message: err.message }, 405);
    }
    return c.json(
      { error: "invalid_value", message: (err as Error).message },
      400,
    );
  }
});

app.delete("/:key", async (c) => {
  if (!requireAdmin(c)) return c.json({ error: "forbidden" }, 403);
  const key = c.req.param("key");
  try {
    const removed = await c.get("deps").settings.reset(key, c.get("source"));
    return removed
      ? c.json({ key, reset: true })
      : c.json({ key, reset: false, message: "No override existed" });
  } catch (err) {
    if (err instanceof UnknownSettingError) {
      return c.json({ error: "not_found", message: err.message }, 404);
    }
    if (err instanceof ImmutableSettingError) {
      return c.json({ error: "immutable", message: err.message }, 405);
    }
    return c.json(
      { error: "invalid_setting", message: (err as Error).message },
      400,
    );
  }
});

export default app;
