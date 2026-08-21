import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import type { SearchFilters } from "../store/types.js";
import {
  ForbiddenError,
  resolveScopePrefixes,
} from "../services/projectService.js";
import { validateQuery } from "../middleware/validate.js";

const querySchema = z.object({
  q: z.string().min(1),
  limit: z.coerce.number().int().min(1).max(100).optional(),
  type: z.enum(["page", "source", "doc"]).optional(),
  tags: z.string().optional(),
  status: z.string().optional(),
  trust: z
    .enum(["unverified", "machine-confirmed", "human-reviewed"])
    .optional(),
  fresh: z.coerce.boolean().optional(),
  origin: z.string().optional(),
  project: z.string().optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(querySchema), async (c) => {
  const { q, limit, type, tags, status, trust, fresh, origin, project } = c.get(
    "validatedQuery",
  ) as z.infer<typeof querySchema>;

  try {
    let pathPrefixes: string[] | undefined;
    if (project !== undefined) {
      pathPrefixes = await resolveScopePrefixes(
        c.get("store"),
        c.get("auth"),
        project,
      );
    }

    const filters: SearchFilters = {
      kinds: type ? [type] : undefined,
      tags: tags
        ? tags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean)
        : undefined,
      statuses: status ? [status] : undefined,
      trustMin: trust,
      freshOnly: fresh,
      origins: origin ? [origin] : undefined,
      pathPrefixes,
    };

    const r = await c.get("deps").search.search({ q, limit, filters });
    return c.json({
      query: q,
      mode: r.mode,
      latency_ms: r.latencyMs,
      results: r.results,
    });
  } catch (err) {
    if (err instanceof ForbiddenError) {
      return c.json({ error: "forbidden", message: err.message }, 403);
    }
    throw err;
  }
});

export default app;
