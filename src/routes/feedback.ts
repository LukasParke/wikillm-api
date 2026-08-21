import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateBody } from "../middleware/validate.js";

const feedbackSchema = z.object({
  query_id: z.string().min(1),
  helpful: z.boolean(),
  comment: z.string().optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.post("/", validateBody(feedbackSchema), async (c) => {
  const body = c.get("validatedBody") as z.infer<typeof feedbackSchema>;
  await c.get("store").recordFeedback({
    query_id: body.query_id,
    helpful: body.helpful,
    comment: body.comment,
  });
  return c.json({ success: true }, 201);
});

export default app;
