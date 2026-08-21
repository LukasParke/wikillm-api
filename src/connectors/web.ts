import { parse } from "node-html-parser";
import type { ConnectorImpl, ConnectorPollResult } from "./types.js";
import { strListConfig } from "./types.js";

interface WebState {
  etags?: Record<string, string>;
}

/**
 * Web connector: fetches a configured URL list, extracts readable text, and
 * emits one document per URL. Uses conditional requests (If-None-Match) so
 * unchanged pages are skipped cheaply.
 */
export const webConnector: ConnectorImpl = {
  kind: "web",
  async poll(config, state): Promise<ConnectorPollResult> {
    const urls = strListConfig(config, "urls", []);
    if (urls.length === 0)
      throw new Error("web connector requires config.urls");
    const previous = (state ?? {}) as WebState;
    const etags: Record<string, string> = { ...(previous.etags ?? {}) };
    const docs = [];

    for (const url of urls) {
      const headers: Record<string, string> = {};
      if (etags[url]) headers["If-None-Match"] = etags[url];
      let response: Response;
      try {
        response = await fetch(url, {
          headers,
          signal: AbortSignal.timeout(30_000),
        });
      } catch {
        continue; // unreachable page: skip this cycle
      }
      if (response.status === 304) continue;
      if (!response.ok) continue;
      const etag = response.headers.get("etag");
      const html = await response.text();
      const extracted = extractReadable(html, url);
      docs.push({
        path: urlToPath(url),
        content: extracted.markdown,
        title: extracted.title,
        contentType: "text/markdown",
      });
      if (etag) etags[url] = etag;
    }

    return { docs, state: { etags } satisfies WebState };
  },
};

export function extractReadable(
  html: string,
  url: string,
): { title: string; markdown: string } {
  const root = parse(html);
  root
    .querySelectorAll("script,style,noscript,svg")
    .forEach((el) => el.remove());
  const title =
    root.querySelector("meta[property='og:title']")?.getAttribute("content") ??
    root.querySelector("title")?.text?.trim() ??
    url;
  const article =
    root.querySelector("article") ??
    root.querySelector("main") ??
    root.querySelector("body");
  const text = (article?.text ?? root.text ?? "")
    .replace(/[ \t]+/g, " ")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { title, markdown: `# ${title}\n\nSource: ${url}\n\n${text}\n` };
}

export function urlToPath(url: string): string {
  const parsed = new URL(url);
  const segments = [
    parsed.hostname,
    ...parsed.pathname.split("/").filter(Boolean),
  ];
  const last = segments[segments.length - 1] ?? "index";
  if (!last.endsWith(".md")) segments[segments.length - 1] = `${last}.md`;
  return segments.join("/").replace(/[^A-Za-z0-9._\-/]/g, "_");
}
