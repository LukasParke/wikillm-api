import type { Context, Next } from "hono";
import { createMiddleware } from "hono/factory";
import type { ApiKeyEntry } from "../config.js";

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

export function authMiddleware(
  apiKeys: Map<string, ApiKeyEntry>,
  publicRead: boolean,
) {
  return createMiddleware(async (c: Context, next: Next) => {
    const header = c.req.header("authorization") ?? "";
    const match = header.match(/^Bearer\s+(.+)$/i);
    if (match) {
      const entry = apiKeys.get(match[1]);
      if (entry) {
        const auth: AuthInfo = {
          name: entry.name,
          role: entry.role,
          projects: entry.projects,
        };
        c.set("auth", auth);
        c.set("source", entry.name);
        return next();
      }
      if (!publicRead) {
        return c.json(
          { error: "unauthorized", message: "Invalid API key" },
          401,
        );
      }
    }
    if (c.req.method === "GET" && publicRead) {
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
