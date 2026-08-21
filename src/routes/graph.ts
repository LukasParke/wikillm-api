import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateQuery } from "../middleware/validate.js";

const depthSchema = z.object({
  depth: z.coerce.number().int().min(1).max(3).default(1),
});

const pathSchema = depthSchema.extend({
  path: z.string().min(1),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(pathSchema), async (c) => {
  const { path: relPath, depth } = c.get("validatedQuery") as z.infer<
    typeof pathSchema
  >;
  const view = await c.get("deps").graph.neighbors(relPath, depth);
  return c.json(view);
});

app.get("/:rel_path{.+}", validateQuery(depthSchema), async (c) => {
  const relPath = c.req.param("rel_path");
  const { depth } = c.get("validatedQuery") as z.infer<typeof depthSchema>;
  const view = await c.get("deps").graph.neighbors(relPath ?? "", depth);
  return c.json(view);
});

export default app;
