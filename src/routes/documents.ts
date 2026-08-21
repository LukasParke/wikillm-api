import { Hono } from "hono";
import { z } from "zod";
import type { AppVariables } from "../app.js";
import { validateQuery } from "../middleware/validate.js";
import { resolveScopePrefixes } from "../services/projectService.js";
import { readPage, readSourceBuffer } from "../fs/wiki.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";

const app = new Hono<{ Variables: AppVariables }>();

const listSchema = z.object({
  kind: z.enum(["page", "source", "doc"]).optional(),
  origin: z.string().optional(),
  folder: z.string().optional(),
  type: z.string().optional(),
  tags: z.string().optional(),
  status: z.string().optional(),
  trust: z
    .enum(["unverified", "machine-confirmed", "human-reviewed"])
    .optional(),
  fresh: z.coerce.boolean().optional(),
  project: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(1000).optional(),
  cursor: z.string().optional(),
});

interface DocumentFilters {
  okf_types?: string[];
  tags?: string[];
  statuses?: string[];
  trustMin?: "unverified" | "machine-confirmed" | "human-reviewed";
  freshOnly?: boolean;
  pathPrefixes?: string[];
}

function parseFilters(query: {
  type?: string;
  tags?: string;
  status?: string;
  trust?: "unverified" | "machine-confirmed" | "human-reviewed";
  fresh?: boolean;
}): DocumentFilters | undefined {
  const filters: ReturnType<typeof parseFilters> = {};
  if (query.type)
    filters.okf_types = query.type
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
  if (query.tags)
    filters.tags = query.tags
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
  if (query.status)
    filters.statuses = query.status
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
  if (query.trust) filters.trustMin = query.trust;
  if (query.fresh) filters.freshOnly = true;
  return Object.keys(filters).length > 0 ? filters : undefined;
}

/** General listing across every indexed document. */
app.get("/", validateQuery(listSchema), async (c) => {
  const query = c.get("validatedQuery") as z.infer<typeof listSchema>;
  const { store } = c.get("deps");
  const auth = c.get("auth");

  const prefixes = await resolveScopePrefixes(store, auth, query.project);
  if (prefixes[0] === "__none__") return c.json({ items: [] });
  const filters: DocumentFilters = parseFilters(query) ?? {};
  if (prefixes[0] !== "*") {
    // scope prefixes ride the same filter machinery
    filters.pathPrefixes = prefixes;
  }

  const result = await store.listDocuments({
    folder: query.folder,
    kind: query.kind,
    origin: query.origin,
    limit: query.limit,
    cursor: query.cursor,
    filters,
  });

  const etag = await store.collectionFingerprint(query.folder || undefined);
  const etagValue = `W/"${etag.count}-${etag.maxMtime}"`;
  if (c.req.header("if-none-match") === etagValue) {
    return c.body(null, 304);
  }
  c.header("ETag", etagValue);

  return c.json({
    items: result.items.map((d) => ({
      rel_path: d.rel_path,
      kind: d.kind,
      origin: d.origin,
      title: d.title,
      okf_type: d.okf_type,
      tags: d.tags,
      status: d.status,
      stale_after: d.stale_after,
      trust:
        d.verified && d.verified.length > 0
          ? d.verified.some((v) => v.by.startsWith("human:"))
            ? "human-reviewed"
            : "machine-confirmed"
          : "unverified",
      hash: d.hash,
      mtime: d.mtime,
      updated_at: d.updated_at,
      updated_by: d.updated_by,
    })),
    nextCursor: result.nextCursor,
  });
});

/** Raw content dispatch by kind (pages → markdown, sources → bytes). */
app.get("/:rel_path{.+}", async (c) => {
  if (!c.req.path.endsWith("/content")) {
    return c.json(
      { error: "not_found", message: "Use /v1/documents?... to list" },
      404,
    );
  }
  const relPath = normalizeRelPath(
    c.req.param("rel_path").slice(0, -"/content".length),
  );
  const { config, store } = c.get("deps");
  const wikiRoot = config.WIKI_ROOT;

  const doc = await store.getDocument(relPath);
  if (!doc) {
    return c.json(
      { error: "not_found", message: `Document not found: ${relPath}` },
      404,
    );
  }

  if (doc.kind === "page") {
    const page = readPage(wikiRoot, relPath);
    if (!page)
      return c.json({ error: "not_found", message: "File missing" }, 404);
    return c.body(page.body, 200, {
      "Content-Type": "text/markdown; charset=utf-8",
      ETag: `"${page.hash}"`,
    });
  }
  if (doc.kind === "source") {
    const buffer = readSourceBuffer(wikiRoot, relPath);
    if (!buffer)
      return c.json({ error: "not_found", message: "File missing" }, 404);
    return c.body(new Uint8Array(buffer), 200, {
      "Content-Type": doc.content_type ?? "application/octet-stream",
      ETag: `"${doc.hash}"`,
    });
  }
  // connector doc: body lives in the index, not the filesystem
  return c.body(doc.body, 200, {
    "Content-Type": doc.content_type ?? "text/markdown; charset=utf-8",
    ETag: `"${doc.hash}"`,
  });
});

const bulkDeleteSchema = z.object({
  rel_paths: z.array(z.string().min(1)).min(1).max(1000),
});

/** Bulk delete with per-op results. Connector docs are connector-managed. */
app.post("/delete", async (c) => {
  const parsed = bulkDeleteSchema.safeParse(await c.req.json());
  if (!parsed.success) {
    return c.json({ error: "validation", issues: parsed.error.issues }, 400);
  }
  const { config, store, pipeline } = c.get("deps");
  const wikiRoot = config.WIKI_ROOT;

  const results = [];
  for (const rel of parsed.data.rel_paths) {
    const relPath = normalizeRelPath(rel);
    const doc = await store.getDocument(relPath);
    if (!doc) {
      results.push({ rel_path: relPath, success: false, error: "not_found" });
      continue;
    }
    if (doc.kind === "doc") {
      results.push({
        rel_path: relPath,
        success: false,
        error: "connector_managed",
        message: `Delete via connector ${doc.origin}`,
      });
      continue;
    }
    const absPath = resolveWikiAbs(wikiRoot, relPath);
    if (!absPath) {
      results.push({
        rel_path: relPath,
        success: false,
        error: "invalid_path",
      });
      continue;
    }
    const { unlinkSync } = await import("node:fs");
    try {
      unlinkSync(absPath);
    } catch (err) {
      results.push({ rel_path: relPath, success: false, error: String(err) });
      continue;
    }
    await pipeline.handleFileChange(relPath, {
      source: "api",
      operationId: null,
    });
    results.push({ rel_path: relPath, success: true });
  }
  return c.json({ results });
});

function resolveWikiAbs(wikiRoot: string, relPath: string): string | null {
  try {
    return resolveWikiPath(wikiRoot, relPath);
  } catch {
    return null;
  }
}

export default app;
