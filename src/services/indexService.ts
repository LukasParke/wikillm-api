import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import type { Database } from "../db/client.js";
import { insertOperation, listPageCache } from "../db/client.js";
import { atomicWrite } from "../fs/atomic.js";
import { resolveWikiPath } from "../fs/paths.js";
import type { Operation, Page, Source } from "../types/index.js";
import { ulid } from "ulidx";

export function createIndexService(
  wikiRoot: string,
  db: Database,
  source: Source,
) {
  const indexPath = path.join(wikiRoot, "index.md");

  return {
    async get(): Promise<{ content: string; pages: Page[] }> {
      let content = "";
      if (existsSync(indexPath)) {
        content = readFileSync(indexPath, "utf8");
      }
      const pages = listPageCache(db, { folder: "wiki", limit: 10000 }).items;
      return { content, pages };
    },

    async refresh(): Promise<{ operationId: string; pageCount: number }> {
      const pages = listPageCache(db, { folder: "wiki", limit: 10000 }).items;

      const byCategory = new Map<string, Page[]>();
      for (const page of pages) {
        const tags = Array.isArray(page.frontmatter.tags)
          ? page.frontmatter.tags
          : [];
        const category =
          (page.frontmatter.category as string) ??
          (tags[0] as string) ??
          "Uncategorized";
        const list = byCategory.get(category) ?? [];
        list.push(page);
        byCategory.set(category, list);
      }

      const lines: string[] = ["# Wiki Index", ""];
      lines.push(`Generated at ${new Date().toISOString()} by ${source}`);
      lines.push("");

      for (const [category, catPages] of Array.from(
        byCategory.entries(),
      ).sort()) {
        lines.push(`## ${category}`);
        lines.push("");
        for (const page of catPages.sort((a, b) =>
          a.rel_path.localeCompare(b.rel_path),
        )) {
          const title = page.title ?? page.rel_path;
          const summary = page.summary ? ` — ${page.summary}` : "";
          lines.push(`- [[${title}]] (${page.rel_path})${summary}`);
        }
        lines.push("");
      }

      const content = lines.join("\n");
      atomicWrite(indexPath, content);

      const operationId = ulid();
      const op: Operation = {
        id: operationId,
        created_at: new Date().toISOString(),
        source,
        action: "index_refresh",
        paths: ["index.md"],
        metadata: { pageCount: pages.length },
        parent_id: null,
      };
      insertOperation(db, op);

      return { operationId, pageCount: pages.length };
    },
  };
}
