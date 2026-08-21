import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { ulid } from "ulidx";
import type { ServiceDeps } from "./pageService.js";
import { appendLogEntry, parseLog } from "./bundleFiles.js";
import type { Operation, Source } from "../types/index.js";

export interface LogEntry {
  date: string;
  kind: string;
  message: string;
}

export function createLogService(deps: ServiceDeps, source: Source) {
  const { config, store, pipeline } = deps;
  const wikiRoot = config.WIKI_ROOT;

  return {
    async get(): Promise<{ content: string; entries: LogEntry[] }> {
      const logPath = path.join(wikiRoot, "log.md");
      const content = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
      return { content, entries: parseLog(content).reverse() };
    },

    async append(
      message: string,
      kind?: string,
    ): Promise<{ operationId: string; entry: string }> {
      const now = new Date();
      const entry = appendLogEntry(wikiRoot, message, kind ?? "Update", now);

      const operationId = ulid();
      const op: Operation = {
        id: operationId,
        created_at: now.toISOString(),
        source,
        action: "log_append",
        paths: ["log.md"],
        metadata: { entry },
        parent_id: null,
      };
      await store.insertOperation(op);
      await pipeline.handleFileChange("log.md", {
        source: "api",
        operationId,
      });

      return { operationId, entry };
    },
  };
}
