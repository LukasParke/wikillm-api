import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
} from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import type { Page, SourceFile } from "../types/index.js";
import { hashContent, readFileAtomic } from "./atomic.js";
import { isIgnoredPath, relativeToWiki } from "./paths.js";

export interface ListOptions {
  folder?: string;
  limit?: number;
  cursor?: string;
}

export interface ListResult<T> {
  items: T[];
  nextCursor?: string;
}

export function listPages(
  wikiRoot: string,
  opts: ListOptions = {},
): ListResult<Page> {
  const folder = opts.folder ?? "wiki";
  const limit = opts.limit ?? 50;
  const dir = path.join(wikiRoot, folder);
  if (!existsSync(dir)) return { items: [] };

  const files = walkMarkdown(dir, wikiRoot);
  const sorted = files.sort();
  const start = opts.cursor ? sorted.indexOf(opts.cursor) + 1 : 0;
  const page = sorted.slice(start, start + limit);
  const items = page
    .map((rel) => readPage(wikiRoot, rel))
    .filter((p): p is Page => p !== null);

  return {
    items,
    nextCursor:
      sorted.length > start + limit ? page[page.length - 1] : undefined,
  };
}

export function readPage(wikiRoot: string, relPath: string): Page | null {
  const abs = path.join(wikiRoot, relPath);
  if (!existsSync(abs)) return null;
  const stat = statSync(abs);
  const { content, hash } = readFileAtomic(abs);
  const parsed = matter(content);
  const body = parsed.content;
  const wordCount = body.split(/\s+/).filter((w) => w.length > 0).length;
  const outgoing = Array.from(new Set(body.match(/\[\[([^\]]+)\]\]/g) ?? []));

  return {
    rel_path: relPath,
    abs_path: abs,
    title: (parsed.data?.title as string) ?? inferTitle(relPath),
    summary: (parsed.data?.summary as string) ?? null,
    frontmatter: parsed.data ?? {},
    body,
    word_count: wordCount,
    outgoing_links: outgoing,
    hash,
    mtime: stat.mtimeMs,
    updated_at: (parsed.data?.updated_at as string) ?? null,
    updated_by: (parsed.data?.updated_by as string) ?? null,
  };
}

export function listSources(
  wikiRoot: string,
  opts: ListOptions = {},
): ListResult<SourceFile> {
  const folder = opts.folder ?? "raw";
  const limit = opts.limit ?? 50;
  const dir = path.join(wikiRoot, folder);
  if (!existsSync(dir)) return { items: [] };

  const files = walkFiles(dir, wikiRoot);
  const sorted = files.sort();
  const start = opts.cursor ? sorted.indexOf(opts.cursor) + 1 : 0;
  const page = sorted.slice(start, start + limit);
  const items = page
    .map((rel) => readSource(wikiRoot, rel))
    .filter((s): s is SourceFile => s !== null);

  return {
    items,
    nextCursor:
      sorted.length > start + limit ? page[page.length - 1] : undefined,
  };
}

export function readSource(
  wikiRoot: string,
  relPath: string,
): SourceFile | null {
  const abs = path.join(wikiRoot, relPath);
  if (!existsSync(abs)) return null;
  const stat = statSync(abs);
  const buf = readFileSync(abs);
  return {
    rel_path: relPath,
    abs_path: abs,
    content_type: inferContentType(relPath),
    size: stat.size,
    hash: hashContent(buf),
    mtime: stat.mtimeMs,
  };
}

export function readSourceBuffer(
  wikiRoot: string,
  relPath: string,
): Buffer | null {
  const abs = path.join(wikiRoot, relPath);
  if (!existsSync(abs)) return null;
  return readFileSync(abs);
}

export function ensureParentDir(filePath: string): void {
  const parent = path.dirname(filePath);
  if (!existsSync(parent)) {
    mkdirSync(parent, { recursive: true });
  }
}

function walkMarkdown(dir: string, wikiRoot: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    const rel = relativeToWiki(wikiRoot, full);
    if (isIgnoredPath(rel)) continue;
    if (entry.isDirectory()) {
      out.push(...walkMarkdown(full, wikiRoot));
    } else if (entry.isFile() && entry.name.endsWith(".md")) {
      out.push(rel);
    }
  }
  return out;
}

function walkFiles(dir: string, wikiRoot: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    const rel = relativeToWiki(wikiRoot, full);
    if (isIgnoredPath(rel)) continue;
    if (entry.isDirectory()) {
      out.push(...walkFiles(full, wikiRoot));
    } else if (entry.isFile()) {
      out.push(rel);
    }
  }
  return out;
}

function inferTitle(relPath: string): string {
  const base = path.basename(relPath, ".md");
  return base.replace(/[-_]/g, " ");
}

function inferContentType(relPath: string): string {
  const ext = path.extname(relPath).toLowerCase();
  switch (ext) {
    case ".md":
      return "text/markdown";
    case ".pdf":
      return "application/pdf";
    case ".png":
      return "image/png";
    case ".jpg":
    case ".jpeg":
      return "image/jpeg";
    case ".gif":
      return "image/gif";
    case ".webp":
      return "image/webp";
    case ".mp3":
      return "audio/mpeg";
    case ".mp4":
      return "video/mp4";
    default:
      return "application/octet-stream";
  }
}
