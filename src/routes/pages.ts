import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { createPageService } from "../services/pageService.js";
import { validateBody, validateQuery } from "../middleware/validate.js";
import { normalizeRelPath } from "../fs/paths.js";

const querySchema = z.object({
  folder: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(1000).optional(),
  cursor: z.string().optional(),
});

const writeSchema = z.object({
  content: z.string(),
  frontmatter: z.record(z.unknown()).optional(),
  ifMatch: z.string().optional().nullable(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(querySchema), async (c) => {
  const { folder, limit, cursor } = c.get("validatedQuery") as z.infer<
    typeof querySchema
  >;
  const service = createPageService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.list(folder, limit, cursor);
  return c.json(result);
});

app.get("/:rel_path{.+}", async (c) => {
  const relPath = c.req.param("rel_path");
  const service = createPageService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const page = await service.get(relPath);
  if (!page)
    return c.json(
      { error: "not_found", message: `Page not found: ${relPath}` },
      404,
    );
  return c.json(page);
});

app.put("/:rel_path{.+}", validateBody(writeSchema), async (c) => {
  const relPath = c.req.param("rel_path");
  const body = c.get("validatedBody") as z.infer<typeof writeSchema>;
  const service = createPageService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.write({
    rel_path: relPath,
    content: body.content,
    frontmatter: body.frontmatter,
    ifMatch: body.ifMatch ?? undefined,
  } as import("../types/index.js").PageWriteInput);
  if (!result.success) {
    return c.json(
      {
        error: "conflict",
        current: result.conflict,
      },
      409,
    );
  }
  return c.json(result, 200);
});

app.delete("/:rel_path{.+}", async (c) => {
  const relPath = c.req.param("rel_path");
  const ifMatch = c.req.header("if-match");
  const service = createPageService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.delete(relPath, ifMatch ?? undefined);
  if (!result.success) {
    if (result.conflict) {
      return c.json({ error: "conflict", current: result.conflict }, 409);
    }
    return c.json(
      { error: "not_found", message: `Page not found: ${relPath}` },
      404,
    );
  }
  return c.json({ success: true, operationId: result.operationId });
});

export default app;
