import type { Store } from "../store/types.js";
import type { ChunkHit, SearchFilters } from "../store/types.js";
import { deriveTrustTier } from "../okf/trust.js";
import type { RuntimeFlags } from "./container.js";

export interface SearchHit {
  rel_path: string;
  title: string | null;
  kind: string;
  origin: string;
  okf_type: string | null;
  tags: string[];
  status: string | null;
  stale_after: string | null;
  trust: "unverified" | "machine-confirmed" | "human-reviewed";
  hash: string;
  mtime: number;
  heading_path: string | null;
  snippet: string;
  score: number;
  context?: Array<{
    ordinal: number;
    heading_path: string | null;
    content: string;
  }>;
}

export type SearchMode = "hybrid" | "fts";

export interface SearchOptions {
  q: string;
  limit?: number;
  filters?: SearchFilters;
  /** disable LLM rerank for this call */
  rerank?: boolean;
  /** attach neighboring chunks for winners (default true) */
  expandContext?: boolean;
}

export interface SearchResult {
  results: SearchHit[];
  mode: SearchMode;
  latencyMs: number;
}

/** Reciprocal Rank Fusion over ranked key lists (Cerebras/SIGIR K=60 recipe). */
export function rrfFuse(
  lists: string[][],
  k = 60,
): Array<{ key: string; score: number }> {
  const scores = new Map<string, number>();
  for (const list of lists) {
    list.forEach((key, index) => {
      const contribution = 1 / (k + index + 1);
      scores.set(key, (scores.get(key) ?? 0) + contribution);
    });
  }
  return [...scores.entries()]
    .map(([key, score]) => ({ key, score }))
    .sort((a, b) => b.score - a.score);
}

/** Age-decay factor: 1.0 today, e^-1 at 30 days. */
export function recencyBoost(mtime: number, now = Date.now()): number {
  const ageDays = Math.max(0, (now - mtime) / 86_400_000);
  return Math.exp(-ageDays / 30);
}

export class LlmNotConfiguredError extends Error {
  constructor(message = "No LLM provider configured") {
    super(message);
    this.name = "LlmNotConfiguredError";
  }
}

const RERANK_PROMPT = [
  "You are a search result reranker.",
  "Rate each document's relevance to the query on a scale of 0-10.",
  "Respond with ONLY a JSON array of numbers, one per document, in order.",
].join(" ");

export class SearchService {
  constructor(
    private readonly store: Store,
    private readonly flags: RuntimeFlags,
  ) {}

  async search(opts: SearchOptions): Promise<SearchResult> {
    const started = Date.now();
    const limit = opts.limit ?? 20;
    const candidateDepth = Math.max(limit * 4, 40);

    const ftsHits = await this.store.searchFts(opts.q, {
      limit: candidateDepth,
      filters: opts.filters,
    });

    let vectorHits: ChunkHit[] = [];
    const llm = this.flags.llm();
    if (llm?.embedModel && this.store.supportsVector()) {
      try {
        const [vector] = await llm.embed([opts.q]);
        vectorHits = await this.store.searchVector(vector, {
          limit: candidateDepth,
          filters: opts.filters,
        });
      } catch {
        // embedding failure degrades to FTS-only
      }
    }

    const byId = new Map<string, ChunkHit>();
    for (const hit of [...ftsHits, ...vectorHits]) byId.set(hit.chunk_id, hit);

    const fused = rrfFuse([
      ftsHits.map((h) => h.chunk_id),
      vectorHits.map((h) => h.chunk_id),
    ]);

    const lowerQ = opts.q.toLowerCase();
    const scored = fused
      .map(({ key, score }) => {
        const hit = byId.get(key)!;
        const decay = 1 + 0.15 * recencyBoost(hit.mtime);
        const titleBonus =
          hit.title && hit.title.toLowerCase().includes(lowerQ) ? 0.05 : 0;
        return { hit, score: score * decay + titleBonus };
      })
      .slice(0, 60);

    const finalOrder = await this.maybeRerank(
      opts.q,
      scored,
      opts.rerank !== false,
    );

    const winners = finalOrder.slice(0, limit);
    const results: SearchHit[] = [];
    for (const { hit, score } of winners) {
      const expanded =
        opts.expandContext === false
          ? undefined
          : await this.expandContext(hit);
      results.push(toSearchHit(hit, score, expanded));
    }

    return {
      results,
      mode: vectorHits.length > 0 ? "hybrid" : "fts",
      latencyMs: Date.now() - started,
    };
  }

  private async maybeRerank(
    q: string,
    candidates: Array<{ hit: ChunkHit; score: number }>,
    enabled: boolean,
  ): Promise<Array<{ hit: ChunkHit; score: number }>> {
    const llm = this.flags.llm();
    if (!enabled || !llm || candidates.length < 2) return candidates;
    const shortlist = candidates.slice(0, 20);
    try {
      const docs = shortlist
        .map(
          (c, i) =>
            `[${i}] ${c.hit.title ?? c.hit.rel_path}${c.hit.heading_path ? ` :: ${c.hit.heading_path}` : ""}\n${c.hit.content.slice(0, 500)}`,
        )
        .join("\n\n");
      const raw = await llm.chat(
        [
          { role: "system", content: RERANK_PROMPT },
          { role: "user", content: `Query: ${q}\n\n${docs}` },
        ],
        { temperature: 0, maxTokens: 200 },
      );
      const scores = JSON.parse(extractJsonArray(raw)) as unknown;
      if (!Array.isArray(scores) || scores.length !== shortlist.length) {
        return candidates;
      }
      return shortlist
        .map((candidate, i) => ({
          hit: candidate.hit,
          score: numOr(scores[i], candidate.score),
        }))
        .sort((a, b) => b.score - a.score);
    } catch {
      return candidates;
    }
  }

  private async expandContext(
    hit: ChunkHit,
  ): Promise<
    | Array<{ ordinal: number; heading_path: string | null; content: string }>
    | undefined
  > {
    const chunks = await this.store.getChunksForDocument(hit.document_id);
    const index = chunks.findIndex((c) => c.id === hit.chunk_id);
    if (index === -1 || chunks.length < 2) return undefined;
    const neighbors = [chunks[index - 1], chunks[index + 1]].filter(
      (c) => c !== undefined,
    );
    if (neighbors.length === 0) return undefined;
    return neighbors.map((c) => ({
      ordinal: c.ordinal,
      heading_path: c.heading_path ?? null,
      content: c.content,
    }));
  }
}

function toSearchHit(
  hit: ChunkHit,
  score: number,
  context: SearchHit["context"],
): SearchHit {
  return {
    rel_path: hit.rel_path,
    title: hit.title,
    kind: hit.kind,
    origin: hit.origin,
    okf_type: hit.okf_type,
    tags: hit.tags,
    status: hit.status,
    stale_after: hit.stale_after,
    trust: deriveTrustTier(hit.verified),
    hash: hit.hash,
    mtime: hit.mtime,
    heading_path: hit.heading_path,
    snippet:
      hit.content.length > 280 ? `${hit.content.slice(0, 280)}…` : hit.content,
    score: Number(score.toFixed(6)),
    context,
  };
}

function extractJsonArray(raw: string): string {
  const start = raw.indexOf("[");
  const end = raw.lastIndexOf("]");
  if (start === -1 || end === -1 || end <= start) return raw.trim();
  return raw.slice(start, end + 1);
}

function numOr(value: unknown, fallback: number): number {
  const n = typeof value === "number" ? value : Number(value);
  return Number.isFinite(n) ? n : fallback;
}
