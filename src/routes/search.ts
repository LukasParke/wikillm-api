import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateQuery } from "../middleware/validate.js";
import { searchPageCache } from "../db/client.js";

const querySchema = z.object({
  q: z.string().min(1),
  in: z.enum(["title", "body", "frontmatter"]).optional(),
  limit: z.coerce.number().int().min(1).max(100).optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(querySchema), async (c) => {
  const {
    q,
    in: field,
    limit,
  } = c.get("validatedQuery") as z.infer<typeof querySchema>;
  const db = c.get("db");

  const fromCache = searchPageCache(db, q, field, limit ?? 20);

  // Body search: also scan files for pages whose body contains the query
  let bodyMatches = fromCache;
  if (field === "body") {
    const { listPageCache } = await import("../db/client.js");
    const { readFileSync } = await import("node:fs");
    const all = listPageCache(db, { folder: "wiki", limit: 10000 }).items;
    const term = q.toLowerCase();
    bodyMatches = all
      .filter((page) => {
        try {
          const content = readFileSync(page.abs_path, "utf8").toLowerCase();
          return content.includes(term);
        } catch {
          return false;
        }
      })
      .slice(0, limit ?? 20);
  }

  return c.json({ query: q, in: field ?? "all", results: bodyMatches });
});

export default app;
