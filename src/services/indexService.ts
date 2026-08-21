import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { ulid } from "ulidx";
import type { ServiceDeps } from "./pageService.js";
import { writeIndexFile } from "./bundleFiles.js";
import type { DocumentRecord } from "../store/types.js";
import type { Operation, Source } from "../types/index.js";

export function createIndexService(deps: ServiceDeps, source: Source) {
  const { config, store, pipeline } = deps;
  const wikiRoot = config.WIKI_ROOT;

  return {
    async get(): Promise<{ content: string; pages: DocumentRecord[] }> {
      const indexPath = path.join(wikiRoot, "index.md");
      const content = existsSync(indexPath)
        ? readFileSync(indexPath, "utf8")
        : "";
      const pages = await store.listDocuments({ folder: "wiki", limit: 10000 });
      return { content, pages: pages.items };
    },

    async refresh(): Promise<{ operationId: string; pageCount: number }> {
      const docs = await store.listDocuments({ folder: "wiki", limit: 10000 });
      writeIndexFile(wikiRoot, docs.items, source);

      const now = new Date().toISOString();
      const operationId = ulid();
      const op: Operation = {
        id: operationId,
        created_at: now,
        source,
        action: "index_refresh",
        paths: ["index.md"],
        metadata: { pageCount: docs.items.length },
        parent_id: null,
      };
      await store.insertOperation(op);
      await pipeline.handleFileChange("index.md", {
        source: "api",
        operationId,
      });

      return { operationId, pageCount: docs.items.length };
    },
  };
}
