import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateQuery } from "../middleware/validate.js";
import { createSourceService } from "../services/sourceService.js";

const querySchema = z.object({
  folder: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(1000).optional(),
  cursor: z.string().optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(querySchema), async (c) => {
  const { folder, limit, cursor } = c.get("validatedQuery") as z.infer<
    typeof querySchema
  >;
  const service = createSourceService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.list(folder, limit, cursor);
  return c.json(result);
});

app.get("/:rel_path{.+}", async (c) => {
  const relPath = c.req.param("rel_path");
  const service = createSourceService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const source = await service.get(relPath);
  if (!source)
    return c.json(
      { error: "not_found", message: `Source not found: ${relPath}` },
      404,
    );
  return c.json(source);
});

app.post("/:rel_path{.+}", async (c) => {
  const relPath = c.req.param("rel_path");
  const force = c.req.query("force") === "true";
  const contentType = c.req.header("content-type") ?? "";
  let body: Buffer | string;
  if (contentType.startsWith("application/json")) {
    const json = await c.req.json();
    body = json.content ?? "";
  } else {
    body = Buffer.from(await c.req.arrayBuffer());
  }
  const service = createSourceService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.write({
    rel_path: relPath,
    content: body,
    force,
  });
  if (!result.success) {
    return c.json(
      {
        error: "exists",
        existingHash: result.existingHash,
        message: "Source already exists. Use ?force=true to overwrite.",
      },
      409,
    );
  }
  return c.json(result, 201);
});

app.delete("/:rel_path{.+}", async (c) => {
  const relPath = c.req.param("rel_path");
  const service = createSourceService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const deleted = await service.delete(relPath);
  if (!deleted)
    return c.json(
      { error: "not_found", message: `Source not found: ${relPath}` },
      404,
    );
  return c.json({ success: true });
});

export default app;
