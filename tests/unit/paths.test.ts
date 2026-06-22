import { describe, it, expect } from "vitest";
import {
  resolveWikiPath,
  isIgnoredPath,
  PathError,
} from "../../src/fs/paths.js";
import path from "node:path";
import os from "node:os";
import { mkdirSync, writeFileSync, rmSync } from "node:fs";

function makeRoot(): string {
  const dir = path.join(
    os.tmpdir(),
    `wikillm-api-test-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  mkdirSync(dir, { recursive: true });
  return dir;
}

describe("resolveWikiPath", () => {
  it("resolves valid wiki paths", () => {
    const root = makeRoot();
    try {
      mkdirSync(path.join(root, "wiki", "entities"), { recursive: true });
      writeFileSync(
        path.join(root, "wiki", "entities", "OpenAI.md"),
        "# OpenAI",
      );
      const resolved = resolveWikiPath(root, "wiki/entities/OpenAI.md");
      expect(resolved).toBe(path.join(root, "wiki", "entities", "OpenAI.md"));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("rejects path traversal", () => {
    const root = makeRoot();
    try {
      expect(() => resolveWikiPath(root, "../secret.txt")).toThrow(PathError);
      expect(() => resolveWikiPath(root, "wiki/../secret.txt")).toThrow(
        PathError,
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("rejects reserved segments", () => {
    const root = makeRoot();
    try {
      expect(() => resolveWikiPath(root, ".obsidian/app.json")).toThrow(
        PathError,
      );
      expect(() => resolveWikiPath(root, "wiki/.git/config")).toThrow(
        PathError,
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("ignores temp files and metadata", () => {
    expect(isIgnoredPath("wiki/page.md.tmp")).toBe(true);
    expect(isIgnoredPath(".obsidian/workspace.json")).toBe(true);
    expect(isIgnoredPath("wiki/page.md")).toBe(false);
  });
});
