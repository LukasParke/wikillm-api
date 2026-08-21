import { existsSync, readFileSync, unlinkSync } from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import { ulid } from "ulidx";
import type { Config } from "../config.js";
import { atomicWrite, readFileAtomic } from "../fs/atomic.js";
import { pathLock } from "../fs/lock.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";
import { ensureParentDir, readPage } from "../fs/wiki.js";
import { actorFromSource } from "../okf/trust.js";
import type { Store } from "../store/types.js";
import type { IndexPipeline } from "./pipeline.js";
import type { SettingsService } from "./settingsService.js";
import { OkfStrictError } from "./settingsService.js";
import type { Operation, Page, Source } from "../types/index.js";

export interface PageWriteInput {
  rel_path: string;
  content: string;
  frontmatter?: Record<string, unknown>;
  ifMatch?: string | null;
}

export interface PageWriteResult {
  success: boolean;
  conflict?: { hash: string; content: string };
  page?: Page;
  operationId?: string;
}

export interface ServiceDeps {
  config: Config;
  store: Store;
  pipeline: IndexPipeline;
  settings: SettingsService;
}

/**
 * OKF strict mode: when enabled AND the bundle declares okf_version, every
 * concept write must carry a non-empty frontmatter type.
 */
export async function enforceOkfStrict(
  settings: SettingsService,
  wikiRoot: string,
  frontmatter: Record<string, unknown>,
): Promise<void> {
  const strict = await settings.get<boolean>("okf_strict");
  if (!strict) return;
  const rootIndex = path.join(wikiRoot, "index.md");
  if (!existsSync(rootIndex)) return;
  const raw = readFileSync(rootIndex, "utf8");
  if (!raw.includes("okf_version")) return;
  const type = frontmatter.type;
  if (typeof type !== "string" || type.trim() === "") {
    throw new OkfStrictError();
  }
}

export function parseHumanActors(raw: string | undefined): Set<string> {
  return new Set(
    (raw ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
  );
}

export function createPageService(
  deps: ServiceDeps,
  source: Source,
): {
  get(relPath: string): Promise<Page | null>;
  list(
    folder?: string,
    limit?: number,
    cursor?: string,
  ): Promise<{ items: Page[]; nextCursor?: string }>;
  write(input: PageWriteInput): Promise<PageWriteResult>;
  batch(
    operations: Array<{
      rel_path: string;
      content?: string;
      frontmatter?: Record<string, unknown>;
      ifMatch?: string | null;
      delete?: boolean;
    }>,
  ): Promise<{
    success: boolean;
    results: Array<{
      rel_path: string;
      success: boolean;
      error?: string;
      conflict?: { hash: string; content: string };
    }>;
  }>;
  delete(relPath: string, ifMatch?: string): Promise<PageWriteResult>;
} {
  const { config, store, pipeline, settings } = deps;
  const wikiRoot = config.WIKI_ROOT;
  const actorFor = async (): Promise<string> =>
    actorFromSource(
      source,
      parseHumanActors(await settings.get<string>("human_actors")),
    );

  return {
    async get(relPath: string): Promise<Page | null> {
      return readPage(wikiRoot, normalizeRelPath(relPath));
    },

    async list(folder?: string, limit?: number, cursor?: string) {
      const result = await store.listDocuments({
        folder,
        kind: "page",
        limit,
        cursor,
      });
      return {
        items: result.items.map(documentToPageSummary),
        nextCursor: result.nextCursor,
      };
    },

    async write(input: PageWriteInput): Promise<PageWriteResult> {
      const relPath = normalizeRelPath(input.rel_path);
      const absPath = resolveWikiPath(wikiRoot, relPath);

      return pathLock.runExclusive(relPath, async () => {
        ensureParentDir(absPath);
        const exists = existsSync(absPath);

        if (exists) {
          const { content, hash } = readFileAtomic(absPath);
          if (
            input.ifMatch !== undefined &&
            input.ifMatch !== null &&
            input.ifMatch !== hash
          ) {
            return {
              success: false,
              conflict: { hash, content: matter(content).content },
            };
          }
        } else if (input.ifMatch) {
          // Tried to update a file that does not exist
          return { success: false, conflict: { hash: "", content: "" } };
        }
        const oldHash = exists ? readFileAtomic(absPath).hash : null;

        const now = new Date().toISOString();
        await enforceOkfStrict(settings, wikiRoot, input.frontmatter ?? {});
        const fm: Record<string, unknown> = { ...(input.frontmatter ?? {}) };
        if (!("updated_at" in fm)) fm.updated_at = now;
        if (!("updated_by" in fm)) fm.updated_by = source;
        if (!("generated" in fm)) {
          fm.generated = { by: await actorFor(), at: now };
        }

        atomicWrite(absPath, matter.stringify(input.content, fm));

        const page = readPage(wikiRoot, relPath);
        if (!page) throw new Error(`Post-write read failed for ${relPath}`);

        const operationId = ulid();
        const op: Operation = {
          id: operationId,
          created_at: now,
          source,
          action: exists ? "update" : "create",
          paths: [relPath],
          metadata: { oldHash, newHash: page.hash },
          parent_id: null,
        };
        await store.insertOperation(op);
        await pipeline.handleFileChange(relPath, {
          source: "api",
          operationId,
        });

        return { success: true, page, operationId };
      });
    },

    async batch(
      operations: Array<{
        rel_path: string;
        content?: string;
        frontmatter?: Record<string, unknown>;
        ifMatch?: string | null;
        delete?: boolean;
      }>,
    ): Promise<{
      success: boolean;
      results: Array<{
        rel_path: string;
        success: boolean;
        error?: string;
        conflict?: { hash: string; content: string };
      }>;
    }> {
      const relPaths = Array.from(
        new Set(operations.map((op) => normalizeRelPath(op.rel_path))),
      );
      const release = await pathLock.acquireMany(relPaths);
      try {
        // Preflight OCC for every write so failures happen before any mutation
        const preflight: Array<{
          rel_path: string;
          success: false;
          error: string;
        }> = [];
        for (const op of operations) {
          if (op.delete) continue;
          const rel = normalizeRelPath(op.rel_path);
          const abs = resolveWikiPath(wikiRoot, rel);
          if (!existsSync(abs)) {
            if (op.ifMatch) {
              preflight.push({
                rel_path: rel,
                success: false,
                error: "not_found",
              });
            }
            continue;
          }
          const { hash } = readFileAtomic(abs);
          if (op.ifMatch && op.ifMatch !== hash) {
            preflight.push({
              rel_path: rel,
              success: false,
              error: "conflict",
            });
          }
        }
        if (preflight.length > 0) {
          return { success: false, results: preflight };
        }

        const results: Array<{
          rel_path: string;
          success: boolean;
          error?: string;
        }> = [];

        for (const op of operations) {
          const rel = normalizeRelPath(op.rel_path);
          try {
            if (op.delete) {
              const abs = resolveWikiPath(wikiRoot, rel);
              if (!existsSync(abs)) {
                results.push({
                  rel_path: rel,
                  success: false,
                  error: "not_found",
                });
                continue;
              }
              const { hash } = readFileAtomic(abs);
              unlinkSync(abs);
              const operationId = ulid();
              await store.insertOperation({
                id: operationId,
                created_at: new Date().toISOString(),
                source,
                action: "delete",
                paths: [rel],
                metadata: { oldHash: hash },
                parent_id: null,
              });
              await pipeline.handleFileChange(rel, {
                source: "api",
                operationId,
              });
              results.push({ rel_path: rel, success: true });
              continue;
            }

            if (op.content === undefined) {
              results.push({
                rel_path: rel,
                success: false,
                error: "missing_content",
              });
              continue;
            }
            const abs = resolveWikiPath(wikiRoot, rel);
            ensureParentDir(abs);
            const existed = existsSync(abs);
            const stamp = new Date().toISOString();
            await enforceOkfStrict(settings, wikiRoot, op.frontmatter ?? {});
            const fm: Record<string, unknown> = { ...(op.frontmatter ?? {}) };
            if (!("updated_at" in fm)) fm.updated_at = stamp;
            if (!("updated_by" in fm)) fm.updated_by = source;
            if (!("generated" in fm)) {
              fm.generated = { by: await actorFor(), at: stamp };
            }
            atomicWrite(abs, matter.stringify(op.content, fm));
            const page = readPage(wikiRoot, rel);
            if (!page) throw new Error("post-write read failed");
            const operationId = ulid();
            await store.insertOperation({
              id: operationId,
              created_at: stamp,
              source,
              action: existed ? "update" : "create",
              paths: [rel],
              metadata: { newHash: page.hash },
              parent_id: null,
            });
            await pipeline.handleFileChange(rel, {
              source: "api",
              operationId,
            });
            results.push({ rel_path: rel, success: true });
          } catch (err) {
            results.push({
              rel_path: rel,
              success: false,
              error: (err as Error).message.slice(0, 200),
            });
          }
        }
        return { success: results.every((r) => r.success), results };
      } finally {
        release();
      }
    },

    async delete(relPath: string, ifMatch?: string): Promise<PageWriteResult> {
      const normalized = normalizeRelPath(relPath);
      const absPath = resolveWikiPath(wikiRoot, normalized);

      return pathLock.runExclusive(normalized, async () => {
        if (!existsSync(absPath)) {
          return { success: false };
        }
        const { content, hash } = readFileAtomic(absPath);
        if (ifMatch !== undefined && ifMatch !== hash) {
          return { success: false, conflict: { hash, content } };
        }

        unlinkSync(absPath);

        const now = new Date().toISOString();
        const operationId = ulid();
        const op: Operation = {
          id: operationId,
          created_at: now,
          source,
          action: "delete",
          paths: [normalized],
          metadata: { oldHash: hash },
          parent_id: null,
        };
        await store.insertOperation(op);
        await pipeline.handleFileChange(normalized, {
          source: "api",
          operationId,
        });

        return { success: true, operationId };
      });
    },
  };
}

function documentToPageSummary(doc: {
  rel_path: string;
  title: string | null;
  summary: string | null;
  frontmatter: Record<string, unknown>;
  word_count: number;
  outgoing_links: string[];
  hash: string;
  mtime: number;
  updated_at: string | null;
  updated_by: string | null;
}): Page {
  return {
    rel_path: doc.rel_path,
    abs_path: "",
    title: doc.title,
    summary: doc.summary,
    frontmatter: doc.frontmatter,
    body: "",
    word_count: doc.word_count,
    outgoing_links: doc.outgoing_links,
    hash: doc.hash,
    mtime: doc.mtime,
    updated_at: doc.updated_at,
    updated_by: doc.updated_by,
  };
}
