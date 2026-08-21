import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import type { ProjectInput } from "../store/types.js";
import { validateBody } from "../middleware/validate.js";

const putSchema = z.object({
  description: z.string().optional().nullable(),
  prefixes: z.array(z.string()).min(1),
  connectors: z.array(z.string()).optional(),
});

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", async (c) => {
  const projects = await c.get("deps").projects.list();
  return c.json({ projects });
});

app.put("/:name", validateBody(putSchema), async (c) => {
  const auth = c.get("auth");
  if (auth.role !== "admin") {
    return c.json({ error: "forbidden" }, 403);
  }
  const name = c.req.param("name") ?? "";
  const body = c.get("validatedBody") as z.infer<typeof putSchema>;
  const input: ProjectInput = {
    name,
    description: body.description,
    prefixes: body.prefixes,
    connectors: body.connectors,
  };
  await c.get("deps").projects.put(input);
  const project = await c.get("deps").projects.get(name);
  return c.json(project);
});

app.delete("/:name", async (c) => {
  const auth = c.get("auth");
  if (auth.role !== "admin") {
    return c.json({ error: "forbidden" }, 403);
  }
  const name = c.req.param("name") ?? "";
  const deleted = await c.get("deps").projects.delete(name);
  if (!deleted) {
    return c.json(
      { error: "not_found", message: `Project not found: ${name}` },
      404,
    );
  }
  return c.json({ success: true });
});

export default app;
