import { existsSync } from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import type { Database } from "../db/client.js";
import {
  insertChange,
  insertOperation,
  upsertPageCache,
} from "../db/client.js";
import { atomicWrite, hashContent, readFileAtomic } from "../fs/atomic.js";
import { pathLock } from "../fs/lock.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";
import { ensureParentDir, readPage } from "../fs/wiki.js";
import type {
  ChangeEvent,
  Operation,
  Page,
  PageWriteInput,
  Source,
} from "../types/index.js";
import { ulid } from "ulidx";

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

export function createIngestService(
  wikiRoot: string,
  db: Database,
  source: Source,
) {
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

          atomicWrite(abs, matter.stringify(op.content, fm));

          const page = readPage(wikiRoot, rel)!;
          upsertPageCache(db, page);
          insertChange(db, {
            id: ulid(),
            rel_path: rel,
            change_type: existed ? "modified" : "created",
            old_hash: null,
            new_hash: page.hash,
            source: "api",
            operation_id: parentId,
            detected_at: now,
          });
          results.push({ rel_path: rel, success: true });
        }

        // Append log entry directly
        if (input.logEntry) {
          const logPath = path.join(wikiRoot, "log.md");
          let existing = "";
          if (existsSync(logPath)) {
            existing = (await import("node:fs")).readFileSync(logPath, "utf8");
            if (existing.length > 0 && !existing.endsWith("\n"))
              existing += "\n";
          }
          const entryLine = `## [${now}] ingest | ${source} | ${input.logEntry}`;
          atomicWrite(logPath, existing + entryLine + "\n\n");
        }

        // Refresh index directly
        if (input.refreshIndex !== false) {
          const { listPageCache } = await import("../db/client.js");
          const pages = listPageCache(db, {
            folder: "wiki",
            limit: 10000,
          }).items;
          const lines = [
            "# Wiki Index",
            "",
            `Generated at ${now} by ${source}`,
            "",
          ];
          const byCategory = new Map<string, typeof pages>();
          for (const page of pages) {
            const tags = Array.isArray(page.frontmatter.tags)
              ? page.frontmatter.tags
              : [];
            const category =
              (page.frontmatter.category as string) ??
              (tags[0] as string) ??
              "Uncategorized";
            const list = byCategory.get(category) ?? [];
            list.push(page);
            byCategory.set(category, list);
          }
          for (const [category, catPages] of Array.from(
            byCategory.entries(),
          ).sort()) {
            lines.push(`## ${category}`, "");
            for (const page of catPages.sort((a, b) =>
              a.rel_path.localeCompare(b.rel_path),
            )) {
              const title = page.title ?? page.rel_path;
              const summary = page.summary ? ` — ${page.summary}` : "";
              lines.push(`- [[${title}]] (${page.rel_path})${summary}`);
            }
            lines.push("");
          }
          atomicWrite(path.join(wikiRoot, "index.md"), lines.join("\n"));
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
        insertOperation(db, op);

        return { success: true, operationId: parentId, results };
      } finally {
        release();
      }
    },
  };
}
