import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateBody } from "../middleware/validate.js";
import { createIngestService } from "../services/ingestService.js";

const pageWriteSchema = z.object({
  rel_path: z.string(),
  content: z.string(),
  frontmatter: z.record(z.unknown()).optional(),
  ifMatch: z.string().optional().nullable(),
});

const ingestSchema = z.object({
  source: z.object({
    title: z.string(),
    rel_path: z.string(),
    content: z.string().optional(),
  }),
  operations: z.array(pageWriteSchema),
  logEntry: z.string().optional(),
  refreshIndex: z.boolean().optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.post("/", validateBody(ingestSchema), async (c) => {
  const body = c.get("validatedBody") as z.infer<typeof ingestSchema>;
  const service = createIngestService(
    c.get("config").WIKI_ROOT,
    c.get("db"),
    c.get("source"),
  );
  const result = await service.run({
    source: {
      ...body.source,
      content: body.source.content,
    },
    operations: body.operations,
    logEntry: body.logEntry,
    refreshIndex: body.refreshIndex,
  });
  if (!result.success) {
    return c.json({ error: "conflict", results: result.results }, 409);
  }
  return c.json(result, 200);
});

export default app;
