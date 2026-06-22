import { z } from "zod";
import type { Context, Next } from "hono";
import { createMiddleware } from "hono/factory";

export function validateBody(schema: z.ZodTypeAny) {
  return createMiddleware(async (c: Context, next: Next) => {
    let body: unknown;
    try {
      body = await c.req.json();
    } catch {
      return c.json({ error: "validation", message: "Invalid JSON body" }, 400);
    }
    const parsed = schema.safeParse(body);
    if (!parsed.success) {
      return c.json({ error: "validation", issues: parsed.error.issues }, 400);
    }
    c.set("validatedBody", parsed.data);
    return next();
  });
}

export function validateQuery(schema: z.ZodTypeAny) {
  return createMiddleware(async (c: Context, next: Next) => {
    const query = Object.fromEntries(new URL(c.req.url).searchParams);
    const parsed = schema.safeParse(query);
    if (!parsed.success) {
      return c.json({ error: "validation", issues: parsed.error.issues }, 400);
    }
    c.set("validatedQuery", parsed.data);
    return next();
  });
}
