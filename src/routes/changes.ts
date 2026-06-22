import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { listChanges } from "../db/client.js";
import { validateQuery } from "../middleware/validate.js";

const querySchema = z.object({
  since: z.string().optional(),
  path: z.string().optional(),
  source: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(1000).optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(querySchema), async (c) => {
  const query = c.get("validatedQuery") as z.infer<typeof querySchema>;
  const changes = listChanges(c.get("db"), {
    since: query.since,
    path: query.path,
    source: query.source,
    limit: query.limit,
  });
  return c.json({ changes });
});

export default app;
