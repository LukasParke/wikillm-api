import { create as tarCreate, extract as tarExtract, Parser } from "tar";
import type { ReadEntry } from "tar";
import { existsSync, realpathSync } from "node:fs";
import { resolve, sep } from "node:path";
import { Hono } from "hono";
import type { AppVariables } from "../app.js";
import { roleAtLeast } from "../services/projectService.js";

const app = new Hono<{ Variables: AppVariables }>();

app.get("/export", (c) => {
  const auth = c.get("auth");
  if (!roleAtLeast(auth.role, "write")) {
    return c.json({ error: "forbidden" }, 403);
  }
  const root = realpathSync(c.get("config").WIKI_ROOT);
  const pack = tarCreate({ gzip: true, sync: true, cwd: root }, ["."]);
  const chunks: Buffer[] = [];
  for (let chunk = pack.read(); chunk !== null; chunk = pack.read()) {
    chunks.push(chunk);
  }
  const buf = Buffer.concat(chunks);
  return new Response(new Uint8Array(buf), {
    headers: {
      "Content-Type": "application/gzip",
      "Content-Disposition": 'attachment; filename="wikillm-bundle.tar.gz"',
    },
  });
});

app.post("/import", async (c) => {
  const auth = c.get("auth");
  if (auth.role !== "admin") {
    return c.json({ error: "forbidden" }, 403);
  }
  const root = realpathSync(c.get("config").WIKI_ROOT);
  const force = c.req.query("force") === "true";
  const buf = Buffer.from(await c.req.arrayBuffer());

  // Header pass: collect archive file paths without extracting.
  const paths: string[] = [];
  const parser = new Parser();
  const parsed = new Promise<void>((resolve, reject) => {
    parser.on("end", resolve);
    parser.on("error", reject);
  });
  parser.on("entry", (entry: ReadEntry) => {
    if (entry.type === "File") paths.push(entry.path);
    entry.resume();
  });
  parser.end(buf);
  await parsed;
  const conflicts: string[] = [];
  for (const p of paths) {
    const resolved = resolve(root, p);
    if (resolved !== root && !resolved.startsWith(root + sep)) {
      return c.json(
        { error: "invalid_entry", message: `Unsafe archive path: ${p}` },
        400,
      );
    }
    if (existsSync(resolved)) conflicts.push(p);
  }
  if (conflicts.length > 0 && !force) {
    return c.json({ error: "exists", conflicts }, 409);
  }

  let imported = 0;
  try {
    const ex = tarExtract({
      cwd: root,
      strict: true,
      onReadEntry: (entry: ReadEntry) => {
        const resolved = resolve(root, entry.path);
        if (resolved !== root && !resolved.startsWith(root + sep)) {
          throw new Error(`Unsafe archive path: ${entry.path}`);
        }
        if (entry.type === "File") imported += 1;
      },
    });
    const extracted = new Promise<void>((resolve, reject) => {
      ex.on("end", resolve);
      ex.on("error", reject);
    });
    ex.end(buf);
    await extracted;
  } catch (err) {
    return c.json(
      {
        error: "extract_failed",
        message: err instanceof Error ? err.message : String(err),
      },
      400,
    );
  }
  return c.json({ imported });
});

export default app;
