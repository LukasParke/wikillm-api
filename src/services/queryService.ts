import { ulid } from "ulidx";
import type { SearchFilters, Store } from "../store/types.js";
import { LlmNotConfiguredError, type SearchService } from "./searchService.js";
import type { LlmProvider } from "../llm/provider.js";

export interface QueryAnswer {
  answer: string;
  citations: Array<{ rel_path: string; hash: string; quote: string }>;
  evidence: Array<{
    rel_path: string;
    heading_path: string | null;
    snippet: string;
  }>;
  toolsUsed: Array<{ name: string; query: string }>;
  mode: "query";
}

interface EvidenceItem {
  rel_path: string;
  kind: string;
  origin: string;
  title: string | null;
  heading_path: string | null;
  content: string;
  hash: string;
  mtime: number;
  score: number;
}

interface ToolPlanEntry {
  name: string;
  query: string;
}

const PLANNER_SYSTEM = [
  "You plan retrieval for a knowledge-base service.",
  "Available tools: search_pages (wiki + ingested docs), search_sources (raw source files), recent_changes (latest edits).",
  'Given a question, respond with ONLY JSON: {"tools":[{"name":"search_pages","query":"..."}]}.',
  "Pick 1-3 tool calls with precise search queries. Prefer search_pages by default.",
].join(" ");

const SYNTHESIS_SYSTEM = [
  "You answer questions strictly from the provided evidence.",
  "Cite sources inline using their exact path in parentheses like (wiki/example.md).",
  "If evidence is insufficient, say so plainly. Never invent facts.",
].join(" ");

const KNOWN_TOOLS = new Set([
  "search_pages",
  "search_sources",
  "recent_changes",
]);

export class QueryService {
  constructor(
    private readonly store: Store,
    private readonly llm: LlmProvider | null,
    private readonly search: SearchService,
  ) {}

  async answer(opts: {
    question: string;
    filters?: SearchFilters;
    source?: string | null;
  }): Promise<QueryAnswer> {
    const llm = this.llm;
    if (!llm) throw new LlmNotConfiguredError();
    const started = Date.now();

    let tools: ToolPlanEntry[] = [];
    try {
      const raw = await llm.chat(
        [
          { role: "system", content: PLANNER_SYSTEM },
          { role: "user", content: opts.question },
        ],
        { temperature: 0, maxTokens: 300 },
      );
      tools = parseToolPlan(raw);
    } catch {
      tools = [];
    }
    if (tools.length === 0) {
      tools = [{ name: "search_pages", query: opts.question }];
    }

    const evidence = await this.executeTools(tools, opts.filters);
    const citations = evidence.slice(0, 8).map((hit) => ({
      rel_path: hit.rel_path,
      hash: hit.hash,
      quote: hit.content.slice(0, 200),
    }));
    const evidenceBlock = evidence
      .slice(0, 8)
      .map(
        (hit, i) =>
          `[${i + 1}] ${hit.rel_path}${hit.heading_path ? ` :: ${hit.heading_path}` : ""} (hash ${hit.hash.slice(0, 12)})\n${hit.content.slice(0, 800)}`,
      )
      .join("\n\n");

    let answer: string;
    try {
      answer = await llm.chat(
        [
          { role: "system", content: SYNTHESIS_SYSTEM },
          {
            role: "user",
            content: `Question: ${opts.question}\n\nEvidence:\n${evidenceBlock || "(none)"}`,
          },
        ],
        { temperature: 0.2, maxTokens: 1200 },
      );
    } catch (err) {
      await this.record(
        opts.question,
        opts.source,
        started,
        0,
        false,
        String(err),
      );
      throw err;
    }

    await this.record(
      opts.question,
      opts.source,
      started,
      evidence.length,
      evidence.length === 0,
      null,
    );

    return {
      answer,
      citations,
      evidence: evidence.slice(0, 8).map((hit) => ({
        rel_path: hit.rel_path,
        heading_path: hit.heading_path,
        snippet: hit.content.slice(0, 240),
      })),
      toolsUsed: tools,
      mode: "query",
    };
  }

  private async executeTools(
    tools: ToolPlanEntry[],
    filters?: SearchFilters,
  ): Promise<EvidenceItem[]> {
    const settled = await Promise.allSettled(
      tools.map(async (tool): Promise<EvidenceItem[]> => {
        if (tool.name === "recent_changes") {
          const changes = await this.store.listChanges({ limit: 10 });
          return changes.map((change) => ({
            rel_path: change.rel_path,
            kind: "change",
            origin: change.source ?? "external",
            title: `${change.change_type}: ${change.rel_path}`,
            heading_path: null,
            content: `Change ${change.change_type} detected at ${change.detected_at}`,
            hash: change.new_hash ?? "",
            mtime: Date.parse(change.detected_at),
            score: 0,
          }));
        }
        const isSources = tool.name === "search_sources";
        const applied: SearchFilters | undefined = isSources
          ? { ...filters, kinds: ["source"] }
          : filters;
        const found = await this.search.search({
          q: tool.query,
          limit: 8,
          rerank: false,
          expandContext: false,
          filters: applied,
        });
        return found.results.map((result) => ({
          rel_path: result.rel_path,
          kind: result.kind,
          origin: result.origin,
          title: result.title,
          heading_path: result.heading_path,
          content: result.snippet,
          hash: result.hash,
          mtime: result.mtime,
          score: result.score,
        }));
      }),
    );
    const merged = settled.flatMap((s) =>
      s.status === "fulfilled" ? s.value : [],
    );
    const best = new Map<string, EvidenceItem>();
    for (const item of merged) {
      const key = `${item.rel_path}::${item.heading_path ?? ""}`;
      const existing = best.get(key);
      if (!existing || item.score > existing.score) best.set(key, item);
    }
    return [...best.values()].sort((a, b) => b.score - a.score);
  }

  private record(
    question: string,
    source: string | null | undefined,
    started: number,
    resultCount: number,
    zeroHit: boolean,
    error: string | null,
  ): Promise<void> {
    return this.store
      .recordQuery({
        id: ulid(),
        created_at: new Date().toISOString(),
        query: question,
        mode: "query",
        project: null,
        latency_ms: Date.now() - started,
        result_count: resultCount,
        zero_hit: zeroHit,
        top_paths: [],
        source: source ?? null,
        error,
      })
      .catch(() => undefined);
  }
}

export function parseToolPlan(raw: string): ToolPlanEntry[] {
  const start = raw.indexOf("{");
  const end = raw.lastIndexOf("}");
  if (start === -1 || end <= start) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw.slice(start, end + 1));
  } catch {
    return [];
  }
  const tools = (parsed as Record<string, unknown>).tools;
  if (!Array.isArray(tools)) return [];
  const out: ToolPlanEntry[] = [];
  for (const entry of tools) {
    if (typeof entry !== "object" || entry === null) continue;
    const name = (entry as Record<string, unknown>).name;
    if (typeof name !== "string" || !KNOWN_TOOLS.has(name)) continue;
    const query = (entry as Record<string, unknown>).query;
    out.push({
      name,
      query: typeof query === "string" ? query : "",
    });
  }
  return out;
}
