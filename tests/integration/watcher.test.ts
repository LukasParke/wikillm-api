import { describe, it, expect, beforeEach, afterEach } from "vitest";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { loadConfig } from "../../src/config.js";
import { createStore } from "../../src/store/index.js";
import type { Store } from "../../src/store/types.js";
import { IndexPipeline } from "../../src/services/pipeline.js";
import {
  createWatcher,
  type WatcherBroadcaster,
} from "../../src/fs/watcher.js";
import type { ChangeEvent } from "../../src/types/index.js";

function makeRoot(): string {
  const dir = path.join(
    os.tmpdir(),
    `wikillm-watcher-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  mkdirSync(dir, { recursive: true });
  return dir;
}

// chokidar + FS events are genuinely asynchronous at the platform level, so
// these integration checks await conditions on the real clock.
async function waitFor(
  predicate: () => boolean | Promise<boolean>,
  timeoutMs = 3000,
): Promise<boolean> {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (await predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return predicate();
}

class CollectingBroadcaster implements WatcherBroadcaster {
  readonly events: ChangeEvent[] = [];
  broadcast(event: ChangeEvent): void {
    this.events.push(event);
  }
}

describe("file watcher", () => {
  let root: string;
  let store: Store;
  let pipeline: IndexPipeline;
  let watcher: { close: () => unknown } | null = null;
  let broadcaster: CollectingBroadcaster;

  beforeEach(async () => {
    root = makeRoot();
    process.env.WIKI_ROOT = root;
    process.env.DB_BACKEND = "sqlite";
    process.env.API_KEYS = "test:key1";
    process.env.DB_PATH = path.join(root, "test.db");
    const config = loadConfig();
    store = await createStore(config);
    pipeline = new IndexPipeline(root, store, {
      llm: () => null,
      embedder: () => null,
      distillEnabled: async () => false,
    });
    broadcaster = new CollectingBroadcaster();
  });

  afterEach(async () => {
    watcher?.close();
    delete process.env.DB_BACKEND;
    delete process.env.WIKI_ROOT;
    delete process.env.API_KEYS;
    delete process.env.DB_PATH;
    rmSync(root, { recursive: true, force: true });
    await store.close();
  });

  it("detects external file creation and broadcasts", async () => {
    mkdirSync(path.join(root, "wiki"), { recursive: true });
    watcher = createWatcher(root, pipeline, broadcaster);
    await waitFor(() => broadcaster.events.length > 0, 1000); // chokidar ready settle

    writeFileSync(path.join(root, "wiki", "external.md"), "# External");

    const seen = await waitFor(() =>
      broadcaster.events.some((e) => e.data.rel_path === "wiki/external.md"),
    );
    expect(seen).toBe(true);
  });

  it("indexes external files into the document store", async () => {
    mkdirSync(path.join(root, "wiki"), { recursive: true });
    watcher = createWatcher(root, pipeline, broadcaster);
    await new Promise((resolve) => setTimeout(resolve, 300)); // watcher startup

    writeFileSync(path.join(root, "wiki", "indexed.md"), "# Indexed page");

    const indexed = await waitFor(async () => {
      const doc = await store.getDocument("wiki/indexed.md");
      return doc !== null && doc.hash.length === 64;
    }, 3000);
    expect(indexed).toBe(true);
  });

  it("ignores .obsidian files", async () => {
    watcher = createWatcher(root, pipeline, broadcaster);
    await new Promise((resolve) => setTimeout(resolve, 300));

    mkdirSync(path.join(root, ".obsidian"), { recursive: true });
    writeFileSync(path.join(root, ".obsidian", "workspace.json"), "{}");

    await new Promise((resolve) => setTimeout(resolve, 500));

    expect(
      broadcaster.events.some((e) => e.data.rel_path.includes(".obsidian")),
    ).toBe(false);
  });
});
