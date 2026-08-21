export interface Chunk {
  ordinal: number;
  headingPath: string | null;
  content: string;
}

export interface ChunkOptions {
  maxChars?: number;
  minChars?: number;
}

interface ResolvedOptions {
  maxChars: number;
  minChars: number;
}

interface RawChunk {
  headingPath: string | null;
  content: string;
}

const DEFAULT_MAX_CHARS = 1200;
const DEFAULT_MIN_CHARS = 200;

const ATX_HEADING = /^(#{1,6})\s+(.+?)\s*#*\s*$/;

function resolveOptions(opts: ChunkOptions): ResolvedOptions {
  const maxChars =
    typeof opts.maxChars === "number" &&
    Number.isFinite(opts.maxChars) &&
    opts.maxChars > 0
      ? Math.floor(opts.maxChars)
      : DEFAULT_MAX_CHARS;
  const minChars =
    typeof opts.minChars === "number" &&
    Number.isFinite(opts.minChars) &&
    opts.minChars > 0
      ? Math.floor(opts.minChars)
      : DEFAULT_MIN_CHARS;
  return { maxChars, minChars };
}

/**
 * Split text into pieces of at most maxChars characters.
 * Tier 1: paragraphs (blank-line separated). Tier 2: lines. Tier 3: hard slices.
 */
function splitToSize(text: string, maxChars: number): string[] {
  const trimmed = text.trim();
  if (trimmed.length === 0) return [];
  return splitTier(trimmed, maxChars);
}

function splitTier(text: string, maxChars: number): string[] {
  if (text.length <= maxChars) return [text];
  const parts = text
    .split(/\n{2,}/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  if (parts.length > 1)
    return packPieces(
      parts.flatMap((part) => splitTier(part, maxChars)),
      "\n\n",
      maxChars,
    );
  const lines = text.split("\n").map((line) => line.trimEnd());
  if (lines.length > 1)
    return packPieces(
      lines.flatMap((line) => splitLine(line, maxChars)),
      "\n",
      maxChars,
    );
  return splitLine(text, maxChars);
}

function splitLine(line: string, maxChars: number): string[] {
  if (line.length <= maxChars) return [line];
  const slices: string[] = [];
  for (let i = 0; i < line.length; i += maxChars) {
    slices.push(line.slice(i, i + maxChars));
  }
  return slices;
}

function packPieces(
  pieces: string[],
  separator: string,
  maxChars: number,
): string[] {
  const out: string[] = [];
  let buffer = "";
  for (const piece of pieces) {
    if (buffer.length === 0) {
      buffer = piece;
    } else if (buffer.length + separator.length + piece.length <= maxChars) {
      buffer += separator + piece;
    } else {
      out.push(buffer);
      buffer = piece;
    }
  }
  if (buffer.length > 0) out.push(buffer);
  return out;
}

// ---------------------------------------------------------------------------
// Markdown chunking
// ---------------------------------------------------------------------------

interface SectionAccumulator {
  path: string[] | null;
  lines: string[];
}

function splitMarkdownSections(body: string): RawChunk[] {
  const sections: RawChunk[] = [];
  const stack: Array<{ level: number; title: string }> = [];
  let current: SectionAccumulator = { path: null, lines: [] };

  const flush = (): void => {
    const content = current.lines.join("\n").trim();
    if (content.length > 0) {
      sections.push({
        headingPath: current.path === null ? null : current.path.join(" > "),
        content,
      });
    }
  };

  for (const line of body.split("\n")) {
    const heading = line.match(ATX_HEADING);
    if (heading === null) {
      current.lines.push(line);
      continue;
    }
    flush();
    const level = heading[1].length;
    const title = heading[2].trim();
    while (stack.length > 0 && stack[stack.length - 1].level >= level)
      stack.pop();
    stack.push({ level, title });
    current = { path: stack.map((entry) => entry.title), lines: [] };
  }
  flush();
  return sections;
}

function mergeSmallChunks(
  chunks: RawChunk[],
  minChars: number,
  maxChars: number,
): RawChunk[] {
  const merged: RawChunk[] = [];
  for (const chunk of chunks) {
    const previous = merged[merged.length - 1];
    if (
      previous !== undefined &&
      chunk.content.length < minChars &&
      previous.headingPath === chunk.headingPath &&
      previous.content.length + chunk.content.length <= maxChars * 1.5
    ) {
      previous.content = `${previous.content}\n\n${chunk.content}`;
    } else {
      merged.push({ headingPath: chunk.headingPath, content: chunk.content });
    }
  }
  return merged;
}

export function chunkMarkdown(body: string, opts: ChunkOptions = {}): Chunk[] {
  const { maxChars, minChars } = resolveOptions(opts);
  const sections = splitMarkdownSections(body);
  const merged = mergeSmallChunks(sections, minChars, maxChars);
  const raw: RawChunk[] = [];
  for (const chunk of merged) {
    if (chunk.content.length <= maxChars) {
      raw.push(chunk);
    } else {
      for (const piece of splitToSize(chunk.content, maxChars)) {
        raw.push({ headingPath: chunk.headingPath, content: piece });
      }
    }
  }
  return raw.map((chunk, index) => ({
    ordinal: index,
    headingPath: chunk.headingPath,
    content: chunk.content,
  }));
}

// ---------------------------------------------------------------------------
// Code chunking
// ---------------------------------------------------------------------------

const LANGUAGE_BY_EXT: Record<string, string> = {
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "jsx",
  py: "python",
  go: "go",
  rs: "rust",
  java: "java",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  hpp: "cpp",
  cs: "csharp",
  rb: "ruby",
  php: "php",
  sh: "shell",
  sql: "sql",
  md: "markdown",
  json: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  html: "html",
  css: "css",
};

export function detectLanguage(filename: string): string | null {
  const base = filename.split(/[\\/]/).pop() ?? filename;
  const dot = base.lastIndexOf(".");
  if (dot <= 0) return null;
  return LANGUAGE_BY_EXT[base.slice(dot + 1).toLowerCase()] ?? null;
}

interface DeclMatch {
  indent: number;
  name: string;
  callable: boolean;
  lineIndex: number;
}

interface DeclNode extends DeclMatch {
  children: DeclNode[];
  spanEnd: number;
}

// Keyword-style declarations: export? abstract? async? pub? class/function/struct/impl/fn/func/def Name
const KEYWORD_DECL =
  /^[ \t]*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?:pub\s+)?(class|function|struct|impl|fn|func|def)\s+([A-Za-z_$][A-Za-z0-9_$]*)/;

// Visibility-modifier methods (C-family/TS): private [static] [async] name(
const MODIFIER_DECL =
  /^[ \t]*(?:public|private|protected)(?:\s+(?:static|readonly|async|override|final|abstract))*\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^>\n]*>)?\(/;
// Plain indented methods (C-family/TS): name(params): RetType {
const METHOD_DECL =
  /^[ \t]+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^>\n]*>)?\([^;{}]*\)\s*(?::\s*[^{;=]+)?\{/;

const RESERVED_NAMES: Record<string, true> = {
  if: true,
  for: true,
  while: true,
  switch: true,
  catch: true,
  return: true,
  new: true,
  function: true,
};

const CALLABLE_KEYWORDS: Record<string, true> = {
  function: true,
  fn: true,
  func: true,
  def: true,
};

const COMMENT_LINE = /^[ \t]*(\/\/|\/\*|\*|#)/;

function scanDecls(lines: string[], languageHint: string | null): DeclMatch[] {
  const pythonOnly = languageHint === "python";
  const decls: DeclMatch[] = [];
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line === undefined || COMMENT_LINE.test(line)) continue;

    const keywordMatch = pythonOnly ? null : line.match(KEYWORD_DECL);
    if (keywordMatch !== null) {
      const keyword = keywordMatch[1];
      const name = keywordMatch[2];
      if (keyword === undefined || name === undefined) continue;
      decls.push({
        indent: line.length - line.trimStart().length,
        name,
        callable: CALLABLE_KEYWORDS[keyword] === true,
        lineIndex: i,
      });
      continue;
    }

    const pyMatch = line.match(
      /^[ \t]*(?:async\s+)?(def|class)\s+([A-Za-z_][A-Za-z0-9_]*)/,
    );
    if (pyMatch !== null) {
      const keyword = pyMatch[1];
      const name = pyMatch[2];
      if (keyword === undefined || name === undefined) continue;
      decls.push({
        indent: line.length - line.trimStart().length,
        name,
        callable: keyword === "def",
        lineIndex: i,
      });
      continue;
    }
    if (pythonOnly) continue;
    const modifierMatch = line.match(MODIFIER_DECL);
    if (modifierMatch !== null) {
      const name = modifierMatch[1];
      if (name === undefined) continue;
      decls.push({
        indent: line.length - line.trimStart().length,
        name,
        callable: true,
        lineIndex: i,
      });
      continue;
    }

    const methodMatch = line.match(METHOD_DECL);
    if (
      methodMatch !== null &&
      !line.includes("=>") &&
      methodMatch[1] !== undefined &&
      RESERVED_NAMES[methodMatch[1]] !== true
    ) {
      decls.push({
        indent: line.length - line.trimStart().length,
        name: methodMatch[1],
        callable: true,
        lineIndex: i,
      });
    }
  }
  return decls;
}

function buildTree(decls: DeclMatch[], totalLines: number): DeclNode[] {
  const roots: DeclNode[] = [];
  const stack: DeclNode[] = [];
  for (let i = 0; i < decls.length; i++) {
    const decl = decls[i];
    if (decl === undefined) continue;
    let spanEnd = totalLines;
    for (let j = i + 1; j < decls.length; j++) {
      const later = decls[j];
      if (later !== undefined && later.indent <= decl.indent) {
        spanEnd = later.lineIndex;
        break;
      }
    }
    const node: DeclNode = { ...decl, spanEnd, children: [] };
    while (stack.length > 0 && stack[stack.length - 1].indent >= node.indent)
      stack.pop();
    const parent = stack[stack.length - 1];
    if (parent === undefined) roots.push(node);
    else parent.children.push(node);
    stack.push(node);
  }
  return roots;
}

function displayName(node: DeclMatch): string {
  return node.callable ? `${node.name}()` : node.name;
}

function pushChunk(
  content: string,
  headingPath: string | null,
  out: RawChunk[],
  maxChars: number,
): void {
  if (content.length <= maxChars) {
    const trimmed = content.trim();
    if (trimmed.length > 0) out.push({ headingPath, content: trimmed });
    return;
  }
  for (const piece of splitToSize(content, maxChars)) {
    out.push({ headingPath, content: piece });
  }
}

function emitNode(
  lines: string[],
  node: DeclNode,
  ancestors: string[],
  out: RawChunk[],
  maxChars: number,
): void {
  const path = [...ancestors, displayName(node)];
  const headingPath = path.join(" > ");

  if (node.children.length > 0) {
    let cursor = node.lineIndex;
    for (const child of node.children) {
      const head = lines.slice(cursor, child.lineIndex).join("\n");
      pushChunk(head, headingPath, out, maxChars);
      emitNode(lines, child, path, out, maxChars);
      cursor = child.spanEnd;
    }
    const tail = lines.slice(cursor, node.spanEnd).join("\n");
    pushChunk(tail, headingPath, out, maxChars);
    return;
  }

  const body = lines.slice(node.lineIndex, node.spanEnd).join("\n");
  pushChunk(body, headingPath, out, maxChars);
}

export function chunkCode(
  content: string,
  languageHint: string | null,
  opts: ChunkOptions = {},
): Chunk[] {
  const { maxChars } = resolveOptions(opts);
  const lines = content.split("\n");
  const decls = scanDecls(lines, languageHint);

  if (decls.length === 0) {
    const raw: RawChunk[] = [];
    pushChunk(content, null, raw, maxChars);
    return raw.map((chunk, index) => ({
      ordinal: index,
      headingPath: chunk.headingPath,
      content: chunk.content,
    }));
  }

  const roots = buildTree(decls, lines.length);
  const raw: RawChunk[] = [];
  const first = decls[0];
  if (first !== undefined) {
    pushChunk(lines.slice(0, first.lineIndex).join("\n"), null, raw, maxChars);
  }
  for (const root of roots) emitNode(lines, root, [], raw, maxChars);
  return raw.map((chunk, index) => ({
    ordinal: index,
    headingPath: chunk.headingPath,
    content: chunk.content,
  }));
}
