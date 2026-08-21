import type { Context, Next } from "hono";
import { createMiddleware } from "hono/factory";
import type { KeyRegistry } from "../services/keyRegistry.js";

export interface AuthInfo {
  name: string;
  role: "admin" | "write" | "read";
  projects: string[];
}

const ANONYMOUS: AuthInfo = {
  name: "anonymous",
  role: "read",
  projects: ["*"],
};

/**
 * Bearer-key auth against the KeyRegistry (env bootstrap keys + DB-managed
 * keys). When public read is enabled (runtime setting), unauthenticated GETs
 * pass as anonymous.
 */
export function authMiddleware(
  registry: KeyRegistry,
  publicRead: () => boolean | Promise<boolean>,
) {
  return createMiddleware(async (c: Context, next: Next) => {
    const header = c.req.header("authorization") ?? "";
    const match = header.match(/^Bearer\s+(.+)$/i);
    if (match) {
      const auth = await registry.verify(match[1]);
      if (auth) {
        c.set("auth", auth);
        c.set("source", auth.name);
        return next();
      }
      if (!(await publicRead())) {
        return c.json(
          { error: "unauthorized", message: "Invalid API key" },
          401,
        );
      }
    }
    if (c.req.method === "GET" && (await publicRead())) {
      c.set("auth", ANONYMOUS);
      c.set("source", ANONYMOUS.name);
      return next();
    }
    return c.json(
      {
        error: "unauthorized",
        message: "Missing or invalid Authorization header",
      },
      401,
    );
  });
}
