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

const batchSchema = z.object({
  operations: z
    .array(
      z.object({
        rel_path: z.string().min(1),
        content: z.string().optional(),
        frontmatter: z.record(z.unknown()).optional(),
        ifMatch: z.string().optional().nullable(),
        delete: z.boolean().optional(),
      }),
    )
    .min(1)
    .max(1000),
});

app.post("/batch", validateBody(batchSchema), async (c) => {
  const body = c.get("validatedBody") as z.infer<typeof batchSchema>;
  const service = createPageService(c.get("deps"), c.get("source"));
  const result = await service.batch(body.operations);
  if (!result.success) {
    return c.json({ error: "conflict", results: result.results }, 409);
  }
  return c.json(result, 200);
});

app.get("/", validateQuery(querySchema), async (c) => {
  const { folder, limit, cursor } = c.get("validatedQuery") as z.infer<
    typeof querySchema
  >;
  const service = createPageService(c.get("deps"), c.get("source"));
  const result = await service.list(folder, limit, cursor);
  return c.json(result);
});

app.get("/:rel_path{.+}", async (c) => {
  const rawMode = c.req.path.endsWith("/raw");
  const relPath = rawMode
    ? c.req.param("rel_path").slice(0, -"/raw".length)
    : c.req.param("rel_path");
  const service = createPageService(c.get("deps"), c.get("source"));
  const page = await service.get(relPath);
  if (!page)
    return c.json(
      { error: "not_found", message: `Page not found: ${relPath}` },
      404,
    );
  if (rawMode) {
    return c.body(page.body, 200, {
      "Content-Type": "text/markdown; charset=utf-8",
      ETag: `"${page.hash}"`,
    });
  }
  return c.json(page);
});

app.put("/:rel_path{.+}", validateBody(writeSchema), async (c) => {
  const relPath = c.req.param("rel_path");
  const body = c.get("validatedBody") as z.infer<typeof writeSchema>;
  const service = createPageService(c.get("deps"), c.get("source"));
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
  const service = createPageService(c.get("deps"), c.get("source"));
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
