import { describe, it, expect } from "vitest";
import {
  atomicWriteSync,
  readFileAtomic,
  hashContent,
} from "../../src/fs/atomic.js";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync, existsSync } from "node:fs";

function makeRoot(): string {
  const dir = path.join(
    os.tmpdir(),
    `wikillm-atomic-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  mkdirSync(dir, { recursive: true });
  return dir;
}

describe("atomic write", () => {
  it("writes atomically and reads back with hash", () => {
    const root = makeRoot();
    try {
      const file = path.join(root, "note.md");
      atomicWriteSync(file, "# Hello");
      const { content, hash } = readFileAtomic(file);
      expect(content).toBe("# Hello");
      expect(hash).toBe(hashContent("# Hello"));
      expect(existsSync(`${file}.`)).toBe(false); // no leftover tmp
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("overwrites existing files atomically", () => {
    const root = makeRoot();
    try {
      const file = path.join(root, "note.md");
      atomicWriteSync(file, "first");
      atomicWriteSync(file, "second");
      expect(readFileAtomic(file).content).toBe("second");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
