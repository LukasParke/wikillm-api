import { existsSync, unlinkSync } from "node:fs";
import { ulid } from "ulidx";
import type { Config } from "../config.js";
import { atomicWrite, hashContent, readFileAtomic } from "../fs/atomic.js";
import { pathLock } from "../fs/lock.js";
import { normalizeRelPath, resolveWikiPath } from "../fs/paths.js";
import { ensureParentDir, listSources, readSource } from "../fs/wiki.js";
import type { ServiceDeps } from "./pageService.js";
import type { Operation, Source, SourceFile } from "../types/index.js";

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

export function createSourceService(deps: ServiceDeps, source: Source) {
  const { config, store, pipeline } = deps;
  const wikiRoot = config.WIKI_ROOT;

  return {
    async get(relPath: string): Promise<SourceFile | null> {
      return readSource(wikiRoot, normalizeRelPath(relPath));
    },

    async list(folder?: string, limit?: number, cursor?: string) {
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

        const now = new Date().toISOString();
        const operationId = ulid();
        const op: Operation = {
          id: operationId,
          created_at: now,
          source,
          action: exists ? "update" : "create",
          paths: [relPath],
          metadata: { existingHash, newHash: hashContent(data) },
          parent_id: null,
        };
        await store.insertOperation(op);
        await pipeline.handleFileChange(relPath, {
          source: "api",
          operationId,
        });

        const sourceFile = readSource(wikiRoot, relPath);
        return { success: true, source: sourceFile ?? undefined, operationId };
      });
    },

    async delete(relPath: string): Promise<boolean> {
      const normalized = normalizeRelPath(relPath);
      const absPath = resolveWikiPath(wikiRoot, normalized);

      return pathLock.runExclusive(normalized, async () => {
        if (!existsSync(absPath)) return false;
        const { hash } = readFileAtomic(absPath);
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
        return true;
      });
    },
  };
}
