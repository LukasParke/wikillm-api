import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateQuery } from "../middleware/validate.js";

const depthSchema = z.object({
  depth: z.coerce.number().int().min(1).max(3).default(1),
  format: z.enum(["json", "dot"]).default("json"),
});

const pathSchema = depthSchema.extend({
  path: z.string().min(1),
});

function toDot(view: {
  nodes: Array<{ rel_path: string; title: string | null; exists: boolean }>;
  edges: Array<{ src: string; dst: string }>;
}): string {
  const lines = ["digraph knowledge {", "  rankdir=LR;"];
  for (const node of view.nodes) {
    const label = (node.title ?? node.rel_path).replace(/"/g, "'");
    const style = node.exists ? "" : ' [label="' + label + '", style=dashed]';
    lines.push(
      node.exists
        ? `  "${node.rel_path}" [label="${label}"];`
        : `  "${node.rel_path}"${style}`,
    );
  }
  for (const edge of view.edges) {
    lines.push(`  "${edge.src}" -> "${edge.dst}";`);
  }
  lines.push("}");
  return lines.join("\n");
}

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", validateQuery(pathSchema), async (c) => {
  const {
    path: relPath,
    depth,
    format,
  } = c.get("validatedQuery") as z.infer<typeof pathSchema>;
  const view = await c.get("deps").graph.neighbors(relPath, depth);
  if (format === "dot") {
    return c.body(toDot(view), 200, {
      "Content-Type": "text/vnd.graphviz; charset=utf-8",
    });
  }
  return c.json(view);
});

app.get("/:rel_path{.+}", validateQuery(depthSchema), async (c) => {
  const relPath = c.req.param("rel_path");
  const { depth, format } = c.get("validatedQuery") as z.infer<
    typeof depthSchema
  >;
  const view = await c.get("deps").graph.neighbors(relPath ?? "", depth);
  if (format === "dot") {
    return c.body(toDot(view), 200, {
      "Content-Type": "text/vnd.graphviz; charset=utf-8",
    });
  }
  return c.json(view);
});

export default app;
