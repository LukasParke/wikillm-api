import type { Context, Next } from "hono";
import { HTTPException } from "hono/http-exception";
import { PathError } from "../fs/paths.js";

export async function errorHandler(err: Error, c: Context) {
  if (err instanceof HTTPException) {
    return c.json({ error: "http", message: err.message }, err.status);
  }
  if (err instanceof PathError) {
    return c.json({ error: "path", code: err.code, message: err.message }, 400);
  }
  if (err instanceof SyntaxError) {
    return c.json({ error: "parse", message: err.message }, 400);
  }
  // Log unexpected errors
  console.error(err);
  return c.json({ error: "internal", message: "Internal server error" }, 500);
}

export async function notFoundHandler(c: Context) {
  return c.json({ error: "not_found", message: "Route not found" }, 404);
}
