import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateBody } from "../middleware/validate.js";
import { createLogService } from "../services/logService.js";

const appendSchema = z.object({
  message: z.string().min(1),
  kind: z.string().optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", async (c) => {
  const service = createLogService(c.get("deps"), c.get("source"));
  const result = await service.get();
  return c.json(result);
});

app.post("/append", validateBody(appendSchema), async (c) => {
  const body = c.get("validatedBody") as z.infer<typeof appendSchema>;
  const service = createLogService(c.get("deps"), c.get("source"));
  const result = await service.append(body.message, body.kind);
  return c.json(result, 201);
});

export default app;
