import type {
  ConnectorImpl,
  ConnectorPollResult,
  IncomingDoc,
} from "./types.js";
import { strConfig, strListConfig } from "./types.js";

interface GithubState {
  since?: string;
}

interface GhItem {
  number: number;
  title?: string;
  body?: string | null;
  state?: string;
  created_at: string;
  updated_at: string;
  user?: { login?: string };
  html_url?: string;
  pull_request?: unknown;
  tag_name?: string;
  name?: string;
  labels?: Array<{ name?: string }>;
}
type Kind = "issues" | "pulls" | "releases";

/**
 * GitHub connector: issues + pulls via the REST API, releases included.
 * Incremental by `since` watermark; emits one markdown doc per item.
 */
export const githubConnector: ConnectorImpl = {
  kind: "github",
  async poll(config, state): Promise<ConnectorPollResult> {
    const repo = strConfig(config, "repo");
    if (!repo) throw new Error("github connector requires config.repo");
    const token = strConfig(config, "token");
    const kinds = strListConfig(config, "include", [
      "issues",
      "pulls",
      "releases",
    ]).filter(
      (k): k is Kind => k === "issues" || k === "pulls" || k === "releases",
    );

    const previous = (state ?? {}) as GithubState;
    let since = previous.since ?? "1970-01-01T00:00:00Z";
    let newest = since;
    const docs: IncomingDoc[] = [];

    for (const kind of kinds) {
      if (kind === "releases") {
        const items = await fetchJson<GhItem[]>(
          `https://api.github.com/repos/${repo}/releases?per_page=30`,
          token,
        );
        for (const item of items ?? []) {
          if (item.created_at <= since && !(previous.since === undefined))
            continue;
          docs.push(releaseDoc(repo, item));
          newest = maxIso(newest, item.created_at);
        }
        continue;
      }
      // issues endpoint returns PRs too; split by pull_request marker
      const items = await fetchJson<GhItem[]>(
        `https://api.github.com/repos/${repo}/issues?state=all&sort=created&direction=desc&per_page=50`,
        token,
      );
      for (const item of items ?? []) {
        if (previous.since && item.created_at <= since) continue;
        const isPull = item.pull_request !== undefined;
        if (kind === "issues" && isPull) continue;
        if (kind === "pulls" && !isPull) continue;
        if (!previous.since || item.created_at > since) {
          docs.push(issueDoc(repo, item, isPull));
          newest = maxIso(newest, item.created_at);
        }
      }
    }

    return { docs, state: { since: newest } satisfies GithubState };
  },
};

function issueDoc(repo: string, item: GhItem, isPull: boolean): IncomingDoc {
  const kindDir = isPull ? "pulls" : "issues";
  const labels = (item.labels ?? [])
    .map((l) => l.name)
    .filter((n): n is string => typeof n === "string");
  const fm = [
    "---",
    `type: ${isPull ? "Pull Request" : "Issue"}`,
    `title: ${JSON.stringify(item.title ?? `#${item.number}`)}`,
    `tags: [${labels.map((l) => JSON.stringify(l)).join(", ")}]`,
    `state: ${item.state ?? "unknown"}`,
    `author: ${item.user?.login ?? "unknown"}`,
    `resource: ${item.html_url ?? ""}`,
    `created_at: "${item.created_at}"`,
    "---",
    "",
    `# ${item.title ?? `#${item.number}`}`,
    "",
    item.body ?? "",
  ].join("\n");
  return {
    path: `${repo}/${kindDir}/${item.number}.md`.replace("/", "__"),
    content: fm,
    title: item.title,
    contentType: "text/markdown",
    mtime: Date.parse(item.updated_at),
  };
}

function releaseDoc(repo: string, item: GhItem): IncomingDoc {
  const name = item.name ?? item.tag_name ?? "release";
  const content = [
    "---",
    "type: Release",
    `title: ${JSON.stringify(name)}`,
    `resource: ${item.html_url ?? ""}`,
    `created_at: "${item.created_at}"`,
    "---",
    "",
    item.body ?? "",
  ].join("\n");
  return {
    path: `${repo.replace("/", "__")}__releases__${(item.tag_name ?? name).replace(/[^A-Za-z0-9._-]/g, "_")}.md`,
    content,
    title: name,
    contentType: "text/markdown",
    mtime: Date.parse(item.created_at),
  };
}

async function fetchJson<T>(url: string, token?: string): Promise<T | null> {
  const headers: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "User-Agent": "wikillm-api",
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(url, {
    headers,
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) return null;
  return (await response.json()) as T;
}

function maxIso(a: string, b: string): string {
  return b > a ? b : a;
}
