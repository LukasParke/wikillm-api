import matter from "gray-matter";

export interface ParsedDocument {
  frontmatter: Record<string, unknown>;
  body: string;
  links: string[];
  wikilinks: string[];
}

const MARKDOWN_LINK_RE =
  /!?\[[^\][]*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)/g;

const WIKILINK_RE = /\[\[([^\][|]+)(?:\|[^\][]*)?\]\]/g;

/** Extract markdown link and image targets, skipping external http(s)/mailto
 * URLs and anchor-only references; `#anchor` suffixes are stripped;
 * order-preserving dedupe. */
export function extractLinks(body: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  let match: RegExpExecArray | null;
  MARKDOWN_LINK_RE.lastIndex = 0;
  while ((match = MARKDOWN_LINK_RE.exec(body)) !== null) {
    let target = match[1];
    if (target.startsWith("<") && target.endsWith(">")) {
      target = target.slice(1, -1);
    }
    if (/^(https?:|mailto:)/i.test(target)) continue;
    const hash = target.indexOf("#");
    if (hash === 0) continue;
    if (hash > 0) target = target.slice(0, hash);
    if (target === "") continue;
    if (!seen.has(target)) {
      seen.add(target);
      out.push(target);
    }
  }
  return out;
}

/** Extract wikilink targets from `[[Target]]` and `[[Target|alias]]`;
 * order-preserving dedupe. */
export function extractWikilinks(body: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  let match: RegExpExecArray | null;
  WIKILINK_RE.lastIndex = 0;
  while ((match = WIKILINK_RE.exec(body)) !== null) {
    const target = match[1].trim();
    if (target === "") continue;
    if (!seen.has(target)) {
      seen.add(target);
      out.push(target);
    }
  }
  return out;
}

/** Resolve a link target to a bundle-relative candidate `.md` path.
 *
 * - Leading `/` targets are bundle-root-relative; others resolve against the
 *   directory of `sourceRelPath`.
 * - `.md` is appended when the final segment lacks an extension.
 * - Returns null for external/mailto/anchor-only links and for paths that
 *   escape the bundle root via `..`.
 */
export function resolveLinkTarget(
  link: string,
  sourceRelPath: string,
): string | null {
  if (link === "") return null;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(link)) return null;
  if (link.startsWith("#")) return null;
  const hash = link.indexOf("#");
  const stripped = hash >= 0 ? link.slice(0, hash) : link;
  if (stripped === "") return null;

  const rootRelative = stripped.startsWith("/");
  const stack: string[] = [];
  if (!rootRelative) {
    for (const part of sourceRelPath.split("/").slice(0, -1)) {
      if (part !== "" && part !== ".") stack.push(part);
    }
  }
  for (const part of stripped.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (stack.length === 0) return null;
      stack.pop();
    } else {
      stack.push(part);
    }
  }
  if (stack.length === 0) return null;
  const last = stack.length - 1;
  if (!/\.[^./]+$/.test(stack[last])) stack[last] += ".md";
  return stack.join("/");
}

export function parseMarkdownDocument(raw: string): ParsedDocument {
  const parsed = matter(raw);
  return {
    frontmatter: parsed.data,
    body: parsed.content,
    links: extractLinks(parsed.content),
    wikilinks: extractWikilinks(parsed.content),
  };
}
