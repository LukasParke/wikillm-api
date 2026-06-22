import { existsSync, statSync } from "node:fs";
import path from "node:path";
import type { Database } from "../db/client.js";
import { insertChange, insertOperation } from "../db/client.js";
import { atomicWrite, hashContent, readFileAtomic } from "../fs/atomic.js";
import { pathLock } from "../fs/lock.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";
import { ensureParentDir, readSource } from "../fs/wiki.js";
import type {
  ChangeEvent,
  Operation,
  Source,
  SourceFile,
} from "../types/index.js";
import { ulid } from "ulidx";

export interface SourceWriteInput {
  rel_path: string;
  content: Buffer | string;
  force?: boolean;
}

export interface SourceWriteResult {
  success: boolean;
  source?: SourceFile;
  operationId?: string;
  existingHash?: string;
}

export function createSourceService(
  wikiRoot: string,
  db: Database,
  source: Source,
) {
  return {
    async get(relPath: string): Promise<SourceFile | null> {
      const normalized = normalizeRelPath(relPath);
      return readSource(wikiRoot, normalized);
    },

    async list(folder?: string, limit?: number, cursor?: string) {
      const { listSources } = await import("../fs/wiki.js");
      return listSources(wikiRoot, { folder, limit, cursor });
    },

    async write(input: SourceWriteInput): Promise<SourceWriteResult> {
      const relPath = normalizeRelPath(input.rel_path);
      if (!relPath.startsWith("raw/")) {
        throw new Error("Sources must be inside raw/");
      }
      const absPath = resolveWikiPath(wikiRoot, relPath);

      return pathLock.runExclusive(relPath, async () => {
        ensureParentDir(absPath);
        const exists = existsSync(absPath);
        let existingHash: string | undefined;

        if (exists) {
          const { hash } = readFileAtomic(absPath);
          existingHash = hash;
          if (!input.force) {
            return { success: false, existingHash: hash };
          }
        }

        const data = Buffer.isBuffer(input.content)
          ? input.content
          : Buffer.from(input.content, "utf8");
        atomicWrite(absPath, data);

        const operationId = ulid();
        const op: Operation = {
          id: operationId,
          created_at: new Date().toISOString(),
          source,
          action: exists ? "update" : "create",
          paths: [relPath],
          metadata: { existingHash, newHash: hashContent(data) },
          parent_id: null,
        };
        insertOperation(db, op);

        const change: ChangeEvent = {
          type: "change",
          data: {
            id: ulid(),
            rel_path: relPath,
            change_type: exists ? "modified" : "created",
            old_hash: existingHash ?? null,
            new_hash: hashContent(data),
            source: "api",
            operation_id: operationId,
            detected_at: op.created_at,
          },
        };
        insertChange(db, change.data);

        const sourceFile = readSource(wikiRoot, relPath)!;
        return { success: true, source: sourceFile, operationId };
      });
    },

    async delete(relPath: string): Promise<boolean> {
      const normalized = normalizeRelPath(relPath);
      const absPath = resolveWikiPath(wikiRoot, normalized);

      return pathLock.runExclusive(normalized, async () => {
        if (!existsSync(absPath)) return false;
        const { hash } = readFileAtomic(absPath);
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
        return true;
      });
    },
  };
}
