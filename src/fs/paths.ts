import { realpathSync, statSync } from "node:fs";
import path from "node:path";

const RESERVED_SEGMENTS = new Set([
  ".git",
  ".obsidian",
  "node_modules",
  ".trash",
]);
const ALLOWED_TOP_LEVEL = new Set([
  "agents.md",
  "claUDE.md",
  "agents.md",
  "claude.md",
  "index.md",
  "log.md",
]);

export function resolveWikiPath(wikiRoot: string, relPath: string): string {
  if (!relPath || typeof relPath !== "string") {
    throw new PathError("Missing path", "MISSING_PATH");
  }

  const decoded = decodeURIComponent(relPath);
  const normalized = path.normalize(decoded).replace(/\\/g, "/");

  if (normalized.startsWith("..") || normalized.startsWith("/")) {
    throw new PathError("Path traversal attempt", "TRAVERSAL");
  }

  const segments = normalized.split("/").filter((s) => s.length > 0);
  if (segments.length === 0) {
    throw new PathError("Empty path", "EMPTY_PATH");
  }

  for (const seg of segments) {
    if (seg === ".." || seg === ".") {
      throw new PathError("Path traversal attempt", "TRAVERSAL");
    }
    if (RESERVED_SEGMENTS.has(seg)) {
      throw new PathError(`Reserved segment: ${seg}`, "RESERVED");
    }
  }

  // Only allow direct access to top-level markdowns in the allowed set.
  if (
    segments.length === 1 &&
    !ALLOWED_TOP_LEVEL.has(segments[0].toLowerCase())
  ) {
    if (!segments[0].startsWith("wiki/") && !segments[0].startsWith("raw/")) {
      throw new PathError(
        "Path must be inside wiki/ or raw/",
        "INVALID_NAMESPACE",
      );
    }
  }

  const abs = path.resolve(wikiRoot, ...segments);
  const rootReal = safeRealpath(wikiRoot);
  const targetReal = safeRealpath(abs);

  if (!targetReal.startsWith(rootReal + path.sep) && targetReal !== rootReal) {
    throw new PathError("Resolved path escapes wiki root", "OUTSIDE_ROOT");
  }

  return targetReal;
}

function safeRealpath(p: string): string {
  try {
    return realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}

export function isUnderWikiRoot(wikiRoot: string, absPath: string): boolean {
  const root = safeRealpath(wikiRoot);
  const target = safeRealpath(absPath);
  return target.startsWith(root + path.sep) || target === root;
}

export function relativeToWiki(wikiRoot: string, absPath: string): string {
  const root = safeRealpath(wikiRoot);
  const target = safeRealpath(absPath);
  return path.relative(root, target).replace(/\\/g, "/");
}

export class PathError extends Error {
  constructor(
    message: string,
    public code: string,
  ) {
    super(message);
    this.name = "PathError";
  }
}

export function isIgnoredPath(relPath: string): boolean {
  const segments = relPath.split("/");
  for (const seg of segments) {
    if (RESERVED_SEGMENTS.has(seg)) return true;
    if (seg.endsWith(".tmp")) return true;
    if (seg.endsWith(".crdownload")) return true;
    if (seg === ".DS_Store") return true;
  }
  return false;
}

export function normalizeRelPath(relPath: string): string {
  return path.normalize(decodeURIComponent(relPath)).replace(/\\/g, "/");
}
