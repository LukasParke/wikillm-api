import { create as tarCreate, extract as tarExtract, Parser } from "tar";
import type { ReadEntry } from "tar";
import { existsSync, realpathSync } from "node:fs";
import path, { resolve, sep } from "node:path";
import { Hono } from "hono";
import type { AppVariables } from "../app.js";
import {
  resolveScopePrefixes,
  roleAtLeast,
} from "../services/projectService.js";

const app = new Hono<{ Variables: AppVariables }>();

app.get("/export", async (c) => {
  const auth = c.get("auth");
  if (!roleAtLeast(auth.role, "write")) {
    return c.json({ error: "forbidden" }, 403);
  }
  const root = realpathSync(c.get("config").WIKI_ROOT);
  const prefix = c.req.query("prefix") || undefined;
  const kind = c.req.query("kind") as "page" | "source" | "doc" | undefined;
  const origin = c.req.query("origin") || undefined;
  const since = c.req.query("since") || undefined;
  const project = c.req.query("project") || undefined;

  let entries: string[] | undefined; // undefined = whole bundle
  if (prefix || kind || origin || since || project) {
    const { store } = c.get("deps");
    const filters: Record<string, unknown> = {};
    if (project) {
      const prefixes = await resolveScopePrefixes(store, auth, project);
      if (prefixes[0] === "__none__") {
        return new Response("empty scope", { status: 200 });
      }
      if (prefixes[0] !== "*") filters.pathPrefixes = prefixes;
    }
    if (kind) filters.kinds = [kind];
    if (origin) filters.origins = [origin];

    // collect candidate paths from the index (connector docs have no file)
    const paths = new Set<string>();
    let cursor: string | undefined;
    do {
      const page = await store.listDocuments({
        limit: 1000,
        cursor,
        folder: prefix,
        filters: Object.keys(filters).length ? (filters as never) : undefined,
      });
      for (const doc of page.items) {
        if (doc.kind === "doc") continue;
        paths.add(doc.rel_path);
      }
      cursor = page.nextCursor;
    } while (cursor);

    // incremental: intersect with paths touched since the watermark
    if (since) {
      const changed = new Set<string>();
      let offset = 0;
      for (;;) {
        const batch = await store.listChanges({ since, limit: 1000 });
        if (batch.length === 0) break;
        for (const change of batch) changed.add(change.rel_path);
        offset += batch.length;
        if (batch.length < 1000) break;
        void offset;
      }
      for (const p of [...paths]) if (!changed.has(p)) paths.delete(p);
    }

    entries = [...paths].filter((p) => existsSync(path.join(root, p)));
    if (entries.length === 0) {
      return c.json(
        { error: "empty", message: "No files match the export filters" },
        404,
      );
    }
  }

  const pack = tarCreate(
    { gzip: true, sync: true, cwd: root },
    entries ?? ["."],
  );
  const chunks: Buffer[] = [];
  for (let chunk = pack.read(); chunk !== null; chunk = pack.read()) {
    chunks.push(chunk);
  }
  const buf = Buffer.concat(chunks);
  const suffix = entries ? `-${Date.now()}` : "";
  return new Response(new Uint8Array(buf), {
    headers: {
      "Content-Type": "application/gzip",
      "Content-Disposition": `attachment; filename="wikillm-bundle${suffix}.tar.gz"`,
      "X-Exported-Files": entries ? String(entries.length) : "all",
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
