import { existsSync } from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import { ulid } from "ulidx";
import type { ServiceDeps } from "./pageService.js";
import { actorFromSource } from "../okf/trust.js";
import { parseHumanActors } from "./pageService.js";
import { appendLogEntry, writeIndexFile } from "./bundleFiles.js";
import { atomicWrite, readFileAtomic } from "../fs/atomic.js";
import { pathLock } from "../fs/lock.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";
import { ensureParentDir } from "../fs/wiki.js";
import type { Operation, PageWriteInput, Source } from "../types/index.js";

export interface IngestInput {
  source: { title: string; rel_path: string; content?: Buffer | string };
  operations: PageWriteInput[];
  logEntry?: string;
  refreshIndex?: boolean;
}

export interface IngestResult {
  success: boolean;
  operationId: string;
  results: {
    rel_path: string;
    success: boolean;
    conflict?: { hash: string; content: string };
  }[];
}

export function createIngestService(deps: ServiceDeps, source: Source) {
  const { config, store, pipeline } = deps;
  const wikiRoot = config.WIKI_ROOT;
  const actor = actorFromSource(source, parseHumanActors(config.HUMAN_ACTORS));

  return {
    async run(input: IngestInput): Promise<IngestResult> {
      const parentId = ulid();
      const opRelPaths = input.operations.map((op) =>
        normalizeRelPath(op.rel_path),
      );
      if (input.source.content !== undefined) {
        opRelPaths.push(normalizeRelPath(input.source.rel_path));
      }
      const allRelPaths = Array.from(new Set(opRelPaths));

      const release = await pathLock.acquireMany(allRelPaths);
      try {
        // Preflight OCC checks for page operations
        const preflight: IngestResult["results"] = [];
        for (const op of input.operations) {
          const rel = normalizeRelPath(op.rel_path);
          const absPath = path.join(wikiRoot, rel);
          let currentHash: string | null = null;
          let currentContent = "";
          if (existsSync(absPath)) {
            const { content, hash } = readFileAtomic(absPath);
            currentHash = hash;
            currentContent = matter(content).content;
          }
          if (op.ifMatch !== undefined && op.ifMatch !== null) {
            if (!existsSync(absPath)) {
              preflight.push({
                rel_path: rel,
                success: false,
                conflict: { hash: "", content: "" },
              });
            } else if (currentHash !== op.ifMatch) {
              preflight.push({
                rel_path: rel,
                success: false,
                conflict: { hash: currentHash!, content: currentContent },
              });
            }
          }
        }

        if (preflight.some((r) => !r.success)) {
          return { success: false, operationId: parentId, results: preflight };
        }

        const now = new Date().toISOString();
        const results: IngestResult["results"] = [];

        // Write source if provided
        if (input.source.content !== undefined) {
          const rel = normalizeRelPath(input.source.rel_path);
          const abs = resolveWikiPath(wikiRoot, rel);
          ensureParentDir(abs);
          const data = Buffer.isBuffer(input.source.content)
            ? input.source.content
            : Buffer.from(input.source.content, "utf8");
          atomicWrite(abs, data);
          await pipeline.handleFileChange(rel, {
            source: "api",
            operationId: parentId,
          });
        }

        // Write pages directly without re-acquiring locks
        for (const op of input.operations) {
          const rel = normalizeRelPath(op.rel_path);
          const abs = resolveWikiPath(wikiRoot, rel);
          ensureParentDir(abs);
          const existed = existsSync(abs);

          const fm: Record<string, unknown> = { ...(op.frontmatter ?? {}) };
          if (!("updated_at" in fm)) fm.updated_at = now;
          if (!("updated_by" in fm)) fm.updated_by = source;
          if (!("generated" in fm)) fm.generated = { by: actor, at: now };

          atomicWrite(abs, matter.stringify(op.content, fm));
          await pipeline.handleFileChange(rel, {
            source: "api",
            operationId: parentId,
          });
          results.push({ rel_path: rel, success: true });
        }

        // Append log entry directly
        if (input.logEntry) {
          appendLogEntry(
            wikiRoot,
            `${source} | ${input.logEntry}`,
            "Ingest",
            new Date(now),
          );
          await pipeline.handleFileChange("log.md", {
            source: "api",
            operationId: parentId,
          });
        }

        // Refresh index directly
        if (input.refreshIndex !== false) {
          const docs = await store.listDocuments({
            folder: "wiki",
            limit: 10000,
          });
          writeIndexFile(wikiRoot, docs.items, source);
          await pipeline.handleFileChange("index.md", {
            source: "api",
            operationId: parentId,
          });
        }

        const op: Operation = {
          id: parentId,
          created_at: now,
          source,
          action: "ingest",
          paths: allRelPaths,
          metadata: { sourceTitle: input.source.title },
          parent_id: null,
        };
        await store.insertOperation(op);

        return { success: true, operationId: parentId, results };
      } finally {
        release();
      }
    },
  };
}
