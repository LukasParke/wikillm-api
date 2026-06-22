import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import type { Database } from "../db/client.js";
import { insertOperation } from "../db/client.js";
import { atomicWrite } from "../fs/atomic.js";
import type { Operation, Source } from "../types/index.js";
import { ulid } from "ulidx";

export function createLogService(
  wikiRoot: string,
  db: Database,
  source: Source,
) {
  const logPath = path.join(wikiRoot, "log.md");

  return {
    async get(): Promise<{ content: string; entries: LogEntry[] }> {
      let content = "";
      if (existsSync(logPath)) {
        content = readFileSync(logPath, "utf8");
      }
      const entries = parseLogEntries(content);
      return { content, entries };
    },

    async append(
      message: string,
      prefix?: string,
    ): Promise<{ operationId: string; entry: string }> {
      const timestamp = new Date().toISOString();
      const entryLine = `## [${timestamp}] ${prefix ?? source} | ${message}`;

      let existing = "";
      if (existsSync(logPath)) {
        existing = readFileSync(logPath, "utf8");
        if (existing.length > 0 && !existing.endsWith("\n")) {
          existing += "\n";
        }
      }

      const newContent = existing + entryLine + "\n\n";
      atomicWrite(logPath, newContent);

      const operationId = ulid();
      const op: Operation = {
        id: operationId,
        created_at: timestamp,
        source,
        action: "log_append",
        paths: ["log.md"],
        metadata: { entry: entryLine },
        parent_id: null,
      };
      insertOperation(db, op);

      return { operationId, entry: entryLine };
    },
  };
}

export interface LogEntry {
  timestamp: string;
  source: string;
  message: string;
  raw: string;
}

function parseLogEntries(content: string): LogEntry[] {
  const lines = content.split("\n");
  const entries: LogEntry[] = [];
  const regex = /^##\s+\[([^\]]+)\]\s+(.+?)\s+\|\s+(.+)$/;
  for (const line of lines) {
    const match = line.match(regex);
    if (match) {
      entries.push({
        timestamp: match[1],
        source: match[2].trim(),
        message: match[3].trim(),
        raw: line,
      });
    }
  }
  return entries.reverse(); // newest first
}
