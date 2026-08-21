import path from "node:path";
import chokidar from "chokidar";
import { isIgnoredPath, relativeToWiki } from "./paths.js";
import type { IndexPipeline } from "../services/pipeline.js";
import type { ChangeEvent } from "../types/index.js";

export interface WatcherCallbacks {
  onReady?: () => void;
}

/** Structural surface the watcher needs; satisfied by services/broadcaster. */
export interface WatcherBroadcaster {
  broadcast(event: ChangeEvent): void;
}

/**
 * Thin FS watcher: debounced events feed the shared index pipeline; pipeline
 * dedupes by hash and returns change events which are broadcast to clients.
 */
export function createWatcher(
  wikiRoot: string,
  pipeline: IndexPipeline,
  broadcaster: WatcherBroadcaster,
  callbacks: WatcherCallbacks = {},
) {
  const pending = new Map<string, string>();
  let flushTimer: ReturnType<typeof setTimeout> | undefined = undefined;

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

  watcher.on("add", (filePath) => queue(filePath));
  watcher.on("change", (filePath) => queue(filePath));
  watcher.on("unlink", (filePath) => queue(filePath));
  watcher.on("ready", () => callbacks.onReady?.());

  return {
    close: () => watcher.close(),
    flush: flushPending,
  };

  function queue(filePath: string) {
    const rel = relativeToWiki(wikiRoot, filePath);
    if (isIgnoredPath(rel)) return;
    pending.set(rel, filePath);
    clearTimeout(flushTimer);
    flushTimer = setTimeout(flushPending, 100);
  }

  async function flushPending() {
    clearTimeout(flushTimer);
    flushTimer = undefined;
    const rels = [...pending.keys()];
    pending.clear();
    for (const rel of rels) {
      try {
        const event = await pipeline.handleFileChange(rel);
        if (event) broadcast(event);
      } catch (err) {
        console.error(`watcher failed for ${rel}`, err);
      }
    }
  }

  function broadcast(data: ChangeEvent["data"]) {
    broadcaster.broadcast({ type: "change", data });
  }
}
