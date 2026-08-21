import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { atomicWrite } from "../fs/atomic.js";
import type { DocumentRecord } from "../store/types.js";

/**
 * Shared writers for the two reserved bundle files (index.md, log.md),
 * formatted per OKF v0.2 §8 and §9. Used by the REST services and ingestion.
 */

export function renderIndexMarkdown(
  docs: DocumentRecord[],
  generator: string,
): string {
  const concepts = docs.filter((d) => d.kind === "page");
  const lines: string[] = ["# Index", ""];
  lines.push(`_Generated ${new Date().toISOString()} by ${generator}_`);
  lines.push("");

  const groups = new Map<string, DocumentRecord[]>();
  for (const doc of concepts) {
    const section =
      doc.okf_type ?? firstTag(doc) ?? directoryOf(doc.rel_path) ?? "Concepts";
    const list = groups.get(section) ?? [];
    list.push(doc);
    groups.set(section, list);
  }

  for (const [section, members] of [...groups.entries()].sort()) {
    lines.push(`## ${section}`, "");
    for (const doc of members.sort((a, b) =>
      a.rel_path.localeCompare(b.rel_path),
    )) {
      const title = doc.title ?? basenameTitle(doc.rel_path);
      const desc = doc.summary ? ` - ${doc.summary}` : "";
      lines.push(`* [${title}](${doc.rel_path})${desc}`);
    }
    lines.push("");
  }
  return lines.join("\n");
}

export function writeIndexFile(
  wikiRoot: string,
  docs: DocumentRecord[],
  generator: string,
): void {
  atomicWrite(
    path.join(wikiRoot, "index.md"),
    renderIndexMarkdown(docs, generator),
  );
}

export interface LogEntryParsed {
  date: string;
  kind: string;
  message: string;
}

const LOG_HEADING = /^## (\d{4}-\d{2}-\d{2})\s*$/;
const LOG_BULLET = /^\*\s+\*\*(\w+)\*\*: (.*)$/;

export function parseLog(content: string): LogEntryParsed[] {
  const entries: LogEntryParsed[] = [];
  let currentDate = "";
  for (const line of content.split("\n")) {
    const heading = line.match(LOG_HEADING);
    if (heading) {
      currentDate = heading[1];
      continue;
    }
    const bullet = line.match(LOG_BULLET);
    if (bullet && currentDate) {
      entries.push({ date: currentDate, kind: bullet[1], message: bullet[2] });
    }
  }
  return entries;
}
/** Prepend an entry under today's date heading, newest-first (OKF §9). */
export function appendLogEntry(
  wikiRoot: string,
  message: string,
  kind: string,
  now: Date,
): string {
  const logPath = path.join(wikiRoot, "log.md");
  const existing = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
  const today = now.toISOString().slice(0, 10);
  const bullet = `* **${kind}**: ${message}`;

  // Preserve any leading title block, then collect date-grouped bullets.
  const titleLines: string[] = [];
  const groups = new Map<string, string[]>();
  let currentDate: string | null = null;
  let inTitle = true;
  for (const line of existing.split("\n")) {
    if (inTitle && !LOG_HEADING.test(line)) {
      if (line.trim() !== "") titleLines.push(line);
      continue;
    }
    inTitle = false;
    const heading = line.match(LOG_HEADING);
    if (heading) {
      currentDate = heading[1];
      if (!groups.has(currentDate)) groups.set(currentDate, []);
      continue;
    }
    const entry = line.match(LOG_BULLET);
    if (entry && currentDate) groups.get(currentDate)!.push(line.trim());
  }

  const todays = groups.get(today) ?? [];
  todays.push(bullet);
  groups.set(today, todays);

  const out: string[] = [...titleLines];
  if (out.length > 0) out.push("");
  for (const [date, bullets] of groups) {
    out.push(`## ${date}`, ...bullets, "");
  }
  const content = out.join("\n").replace(/\n{3,}/g, "\n\n");
  atomicWrite(logPath, content.endsWith("\n") ? content : `${content}\n`);
  return bullet;
}

function firstTag(doc: DocumentRecord): string | null {
  return doc.tags.length > 0 ? doc.tags[0] : null;
}

function directoryOf(relPath: string): string | null {
  const dir = path.dirname(relPath);
  return dir === "." ? null : dir;
}

function basenameTitle(relPath: string): string {
  return path.basename(relPath, ".md").replace(/[-_]/g, " ");
}
