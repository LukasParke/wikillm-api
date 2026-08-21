import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import {
  ForbiddenError,
  resolveScopePrefixes,
} from "../services/projectService.js";
import { LlmNotConfiguredError } from "../services/searchService.js";
import { validateBody } from "../middleware/validate.js";

const askSchema = z.object({
  question: z.string().min(1),
  project: z.string().optional(),
  rerank: z.boolean().default(true),
});

const app = new Hono<{ Variables: AppVariables }>();

app.post("/", validateBody(askSchema), async (c) => {
  const body = c.get("validatedBody") as z.infer<typeof askSchema>;
  try {
    const filters = {
      pathPrefixes: await resolveScopePrefixes(
        c.get("store"),
        c.get("auth"),
        body.project,
      ),
    };
    const answer = await c.get("deps").query.answer({
      question: body.question,
      filters,
      source: c.get("source"),
    });
    return c.json(answer);
  } catch (err) {
    if (err instanceof LlmNotConfiguredError) {
      return c.json({ error: "llm_not_configured", message: err.message }, 503);
    }
    if (err instanceof ForbiddenError) {
      return c.json({ error: "forbidden", message: err.message }, 403);
    }
    throw err;
  }
});

export default app;
