import { existsSync, statSync } from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import type { Database } from "../db/client.js";
import {
  insertChange,
  insertOperation,
  upsertPageCache,
} from "../db/client.js";
import { atomicWrite, readFileAtomic } from "../fs/atomic.js";
import { pathLock } from "../fs/lock.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";
import { ensureParentDir, readPage } from "../fs/wiki.js";
import type { ChangeEvent, Operation, Page, Source } from "../types/index.js";
import { ulid } from "ulidx";

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

export function createPageService(
  wikiRoot: string,
  db: Database,
  source: Source,
) {
  return {
    async get(relPath: string): Promise<Page | null> {
      const normalized = normalizeRelPath(relPath);
      return readPage(wikiRoot, normalized);
    },

    async list(folder?: string, limit?: number, cursor?: string) {
      const { listPageCache } = await import("../db/client.js");
      return listPageCache(db, { folder, limit, cursor });
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

        const fm: Record<string, unknown> = { ...(input.frontmatter ?? {}) };
        if (!("updated_at" in fm)) {
          fm.updated_at = new Date().toISOString();
        }
        if (!("updated_by" in fm)) {
          fm.updated_by = source;
        }

        const fileContent = matter.stringify(input.content, fm);
        atomicWrite(absPath, fileContent);

        const stat = statSync(absPath);
        const page = readPage(wikiRoot, relPath)!;
        const operationId = ulid();
        const op: Operation = {
          id: operationId,
          created_at: new Date().toISOString(),
          source,
          action: exists ? "update" : "create",
          paths: [relPath],
          metadata: {
            oldHash: exists ? readFileAtomic(absPath).hash : null,
            newHash: page.hash,
          },
          parent_id: null,
        };
        insertOperation(db, op);
        upsertPageCache(db, page);

        // The watcher will also record this as external-ish; we pre-empt with source info
        const change: ChangeEvent = {
          type: "change",
          data: {
            id: ulid(),
            rel_path: relPath,
            change_type: exists ? "modified" : "created",
            old_hash: op.metadata?.oldHash as string | null,
            new_hash: page.hash,
            source: "api",
            operation_id: operationId,
            detected_at: op.created_at,
          },
        };
        insertChange(db, change.data);

        return { success: true, page, operationId };
      });
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

        const { unlinkSync } = await import("node:fs");
        unlinkSync(absPath);

        const operationId = ulid();
        const op: Operation = {
          id: operationId,
          created_at: new Date().toISOString(),
          source,
          action: "delete",
          paths: [normalized],
          metadata: { oldHash: hash },
          parent_id: null,
        };
        insertOperation(db, op);
        // page cache and change row will be handled by watcher; we can also do it eagerly
        const { deletePageCache } = await import("../db/client.js");
        deletePageCache(db, normalized);
        insertChange(db, {
          id: ulid(),
          rel_path: normalized,
          change_type: "deleted",
          old_hash: hash,
          new_hash: null,
          source: "api",
          operation_id: operationId,
          detected_at: op.created_at,
        });

        return { success: true, operationId };
      });
    },
  };
}
