import { createHash, randomUUID } from "node:crypto";
import { readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";

export function hashContent(data: string | Buffer): string {
  return createHash("sha256").update(data).digest("hex");
}

export function atomicWriteSync(filePath: string, data: string | Buffer): void {
  const tmpPath = `${filePath}.${randomUUID()}.tmp`;
  writeFileSync(tmpPath, data, "utf8");
  renameSync(tmpPath, filePath);
}

export function atomicWrite(
  filePath: string,
  data: string | Buffer,
): Promise<void> {
  return Promise.resolve(atomicWriteSync(filePath, data));
}

export function readFileAtomic(filePath: string): {
  content: string;
  hash: string;
} {
  const content = readFileSync(filePath, "utf8");
  return { content, hash: hashContent(content) };
}

export function removeIfExists(filePath: string): void {
  try {
    unlinkSync(filePath);
  } catch (err: any) {
    if (err.code !== "ENOENT") throw err;
  }
}

export function cleanupTempFiles(dir: string): void {
  // Optional: scan directory for leftover *.tmp files and remove them.
  // Called on startup to recover from crashed writes.
  try {
    const { readdirSync, statSync } = require("node:fs");
    for (const entry of readdirSync(dir)) {
      const full = path.join(dir, entry);
      if (entry.endsWith(".tmp") && statSync(full).isFile()) {
        try {
          unlinkSync(full);
        } catch {
          // ignore
        }
      }
    }
  } catch {
    // ignore
  }
}
