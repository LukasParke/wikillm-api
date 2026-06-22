import { existsSync, statSync } from "node:fs";
import path from "node:path";
import chokidar from "chokidar";
import type { Database } from "../db/client.js";
import {
  deletePageCache,
  insertChange,
  upsertPageCache,
} from "../db/client.js";
import type { ChangeEvent, ChangeType, Page } from "../types/index.js";
import { hashContent, readFileAtomic } from "./atomic.js";
import { isIgnoredPath, relativeToWiki } from "./paths.js";
import { readPage } from "./wiki.js";
import { ulid } from "ulidx";

export interface WatcherCallbacks {
  onChange?: (event: ChangeEvent) => void;
  onReady?: () => void;
}

export function createWatcher(
  wikiRoot: string,
  db: Database,
  callbacks: WatcherCallbacks = {},
) {
  const pending = new Map<
    string,
    { path: string; type: ChangeType; oldHash: string | null }
  >();
  let flushTimer: ReturnType<typeof setTimeout> | null = null;

  const watcher = chokidar.watch(wikiRoot, {
    ignored: [
      /(^|[/\\])\.git([/\\]|$)/,
      /(^|[/\\])\.obsidian([/\\]|$)/,
      /(^|[/\\])node_modules([/\\]|$)/,
      /(^|[/\\])\.trash([/\\]|$)/,
      /\.tmp$/,
      /\.crdownload$/,
      /\.DS_Store$/,
    ],
    ignoreInitial: true,
    persistent: true,
    awaitWriteFinish: {
      stabilityThreshold: 100,
      pollInterval: 50,
    },
  });

  watcher.on("add", (filePath) => queue(filePath, "created"));
  watcher.on("change", (filePath) => queue(filePath, "modified"));
  watcher.on("unlink", (filePath) => queue(filePath, "deleted"));
  watcher.on("ready", () => callbacks.onReady?.());

  return {
    close: () => watcher.close(),
    flush: flushPending,
  };

  function queue(filePath: string, type: ChangeType) {
    const rel = relativeToWiki(wikiRoot, filePath);
    if (isIgnoredPath(rel)) return;

    const cached = getCachedPage(db, rel);
    const oldHash = cached?.hash ?? null;
    pending.set(rel, { path: filePath, type, oldHash });
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = setTimeout(flushPending, 100);
  }

  function flushPending() {
    if (flushTimer) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    for (const [rel, evt] of pending) {
      processEvent(rel, evt);
    }
    pending.clear();
  }

  function processEvent(
    rel: string,
    evt: { path: string; type: ChangeType; oldHash: string | null },
  ) {
    let newHash: string | null = null;

    if (evt.type === "deleted") {
      deletePageCache(db, rel);
    } else if (existsSync(evt.path) && statSync(evt.path).isFile()) {
      const page = readPage(wikiRoot, rel);
      if (page) {
        upsertPageCache(db, page);
        newHash = page.hash;
      } else {
        const { hash } = readFileAtomic(evt.path);
        newHash = hash;
      }
    }

    const change: ChangeEvent = {
      type: "change",
      data: {
        id: ulid(),
        rel_path: rel,
        change_type: evt.type,
        old_hash: evt.oldHash,
        new_hash: newHash,
        source: null,
        operation_id: null,
        detected_at: new Date().toISOString(),
      },
    };

    insertChange(db, change.data);
    callbacks.onChange?.(change);
  }
}

function getCachedPage(db: Database, relPath: string): Page | undefined {
  const stmt = db.prepare("SELECT hash FROM page_cache WHERE rel_path = ?");
  const row = stmt.get(relPath) as any;
  return row ? ({ hash: row.hash } as Page) : undefined;
}

export function syncFullCache(wikiRoot: string, db: Database): void {
  const scanDir = (dir: string) => {
    const { readdirSync, statSync } = require("node:fs");
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      const rel = relativeToWiki(wikiRoot, full);
      if (isIgnoredPath(rel)) continue;
      if (entry.isDirectory()) {
        scanDir(full);
      } else if (entry.isFile()) {
        const page = readPage(wikiRoot, rel);
        if (page) {
          upsertPageCache(db, page);
        } else {
          const stat = statSync(full);
          const buf = require("node:fs").readFileSync(full);
          const hash = hashContent(buf);
          // We only cache markdown pages; skip non-markdown for now
          if (rel.endsWith(".md")) {
            const parsedPage = readPage(wikiRoot, rel);
            if (parsedPage) upsertPageCache(db, parsedPage);
          }
        }
      }
    }
  };
  scanDir(wikiRoot);
}
