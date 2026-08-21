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
  const service = createSourceService(c.get("deps"), c.get("source"));
  const result = await service.list(folder, limit, cursor);
  return c.json(result);
});

app.get("/:rel_path{.+}", async (c) => {
  const contentMode = c.req.path.endsWith("/content");
  const rawRel = c.req.param("rel_path");
  const relPath = contentMode ? rawRel.slice(0, -"/content".length) : rawRel;
  const service = createSourceService(c.get("deps"), c.get("source"));
  const source = await service.get(relPath);
  if (!source)
    return c.json(
      { error: "not_found", message: `Source not found: ${relPath}` },
      404,
    );
  if (contentMode) {
    const { readSourceBuffer } = await import("../fs/wiki.js");
    const buffer = readSourceBuffer(c.get("config").WIKI_ROOT, relPath);
    if (!buffer) {
      return c.json(
        { error: "not_found", message: "File missing on disk" },
        404,
      );
    }
    return c.body(new Uint8Array(buffer), 200, {
      "Content-Type": source.content_type ?? "application/octet-stream",
      ETag: `"${source.hash}"`,
    });
  }
  return c.json(source);
});

app.post("/:rel_path{.+}", async (c) => {
  const relPath = c.req.param("rel_path");
  const force = c.req.query("force") === "true";
  const contentType = c.req.header("content-type") ?? "";
  const maxBytes =
    ((await c.get("deps").settings.get<number>("max_upload_mb")) ?? 100) *
    1024 *
    1024;
  const declared = Number(c.req.header("content-length") ?? 0);
  if (declared > maxBytes) {
    return c.json(
      { error: "too_large", message: `Upload exceeds ${maxBytes} bytes` },
      413,
    );
  }
  let body: Buffer | string;
  if (contentType.startsWith("application/json")) {
    const json = await c.req.json();
    body = json.content ?? "";
  } else {
    body = Buffer.from(await c.req.arrayBuffer());
    if (body.length > maxBytes) {
      return c.json(
        { error: "too_large", message: `Upload exceeds ${maxBytes} bytes` },
        413,
      );
    }
  }
  const service = createSourceService(c.get("deps"), c.get("source"));
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
  const service = createSourceService(c.get("deps"), c.get("source"));
  const deleted = await service.delete(relPath);
  if (!deleted)
    return c.json(
      { error: "not_found", message: `Source not found: ${relPath}` },
      404,
    );
  return c.json({ success: true });
});

export default app;
