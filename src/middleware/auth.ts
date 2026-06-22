import type { Context, Next } from "hono";
import { createMiddleware } from "hono/factory";

export function authMiddleware(
  apiKeys: Map<string, string>,
  publicRead: boolean,
) {
  return createMiddleware(async (c: Context, next: Next) => {
    if (c.req.method === "GET" && publicRead) {
      c.set("source", "anonymous");
      return next();
    }

    const header = c.req.header("authorization") ?? "";
    const match = header.match(/^Bearer\s+(.+)$/i);
    if (!match) {
      return c.json(
        {
          error: "unauthorized",
          message: "Missing or invalid Authorization header",
        },
        401,
      );
    }
    const key = match[1];
    const source = apiKeys.get(key);
    if (!source) {
      return c.json({ error: "unauthorized", message: "Invalid API key" }, 401);
    }
    c.set("source", source);
    return next();
  });
}

export function requireAuthMiddleware(apiKeys: Map<string, string>) {
  return createMiddleware(async (c: Context, next: Next) => {
    const header = c.req.header("authorization") ?? "";
    const match = header.match(/^Bearer\s+(.+)$/i);
    if (!match) {
      return c.json(
        {
          error: "unauthorized",
          message: "Missing or invalid Authorization header",
        },
        401,
      );
    }
    const source = apiKeys.get(match[1]);
    if (!source) {
      return c.json({ error: "unauthorized", message: "Invalid API key" }, 401);
    }
    c.set("source", source);
    return next();
  });
}
