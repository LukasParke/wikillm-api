import { describe, it, expect, beforeEach, afterEach } from "vitest";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { createDatabase, migrate } from "../../src/db/client.js";
import { syncFullCache, createWatcher } from "../../src/fs/watcher.js";
import { createBroadcaster } from "../../src/services/broadcaster.js";
import type { ChangeEvent } from "../../src/types/index.js";

function makeRoot(): string {
  const dir = path.join(
    os.tmpdir(),
    `wikillm-watcher-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  mkdirSync(dir, { recursive: true });
  return dir;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe("file watcher", () => {
  let root: string;
  let db: Awaited<ReturnType<typeof createDatabase>>;
  let cleanup: () => void;

  beforeEach(async () => {
    root = makeRoot();
    db = await createDatabase(path.join(root, "test.db"));
    migrate(db);
    syncFullCache(root, db);
    cleanup = () => {
      db.close();
      rmSync(root, { recursive: true, force: true });
    };
  });

  afterEach(() => cleanup());

  it("detects external file creation and broadcasts", async () => {
    const events: ChangeEvent[] = [];
    const broadcaster = createBroadcaster();
    const watcher = createWatcher(root, db, {
      onChange: (e) => events.push(e),
      onReady: () => {},
    });
    await delay(200);

    mkdirSync(path.join(root, "wiki"), { recursive: true });
    writeFileSync(path.join(root, "wiki", "external.md"), "# External");

    await delay(300);

    expect(events.some((e) => e.data.rel_path === "wiki/external.md")).toBe(
      true,
    );
    await watcher.close();
  });

  it("ignores .obsidian files", async () => {
    const events: ChangeEvent[] = [];
    const watcher = createWatcher(root, db, {
      onChange: (e) => events.push(e),
      onReady: () => {},
    });
    await delay(200);

    mkdirSync(path.join(root, ".obsidian"), { recursive: true });
    writeFileSync(path.join(root, ".obsidian", "workspace.json"), "{}");

    await delay(300);

    expect(events.some((e) => e.data.rel_path.includes(".obsidian"))).toBe(
      false,
    );
    await watcher.close();
  });
});
