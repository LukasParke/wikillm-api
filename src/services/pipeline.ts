import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import matter from "gray-matter";
import { ulid } from "ulidx";
import {
  extractLinks,
  extractWikilinks,
  resolveLinkTarget,
} from "../okf/parse.js";
import { normalizeVerified } from "../okf/trust.js";
import {
  chunkCode,
  chunkMarkdown,
  detectLanguage,
} from "../ingest/chunkers.js";
import { hashContent, readFileAtomic } from "../fs/atomic.js";
import { isIgnoredPath, relativeToWiki } from "../fs/paths.js";
import { readPage } from "../fs/wiki.js";
import type { ChunkInput, DocumentInput, Store } from "../store/types.js";
import type { ChangeEvent } from "../types/index.js";
import type { RuntimeFlags } from "./container.js";

export interface FileAttribution {
  source: "api" | "external" | null;
  operationId: string | null;
}

const TEXT_CHUNKABLE_EXTENSIONS = new Set([
  ".txt",
  ".csv",
  ".json",
  ".yaml",
  ".yml",
  ".toml",
]);

/**
 * Single indexing path for every document (FS-backed or connector-materialized):
 * parse -> OKF extraction -> chunk -> store -> link edges -> embed queue.
 */
export class IndexPipeline {
  private wikiRoot: string;
  private embedQueue: string[] = [];
  private queued = new Set<string>();
  private draining = false;
  private changeEmitter: ((event: ChangeEvent) => void) | null = null;

  constructor(
    wikiRoot: string,
    private readonly store: Store,
    private readonly flags: RuntimeFlags,
    private readonly log: (msg: string, err?: unknown) => void = console.log,
  ) {
    this.wikiRoot = wikiRoot;
  }

  /** Wire live broadcasting for API-attributed changes (external ones flow
   * through the watcher to avoid double emission). */
  setChangeEmitter(emit: (event: ChangeEvent) => void): void {
    this.changeEmitter = emit;
  }

  /**
   * Index a file from WIKI_ROOT. Returns a change event when the document was
   * created/modified, or null when the content hash is unchanged (idempotent
   * replays from the watcher or boot scan produce no noise).
   */
  async handleFileChange(
    relPath: string,
    attribution: FileAttribution = { source: null, operationId: null },
  ): Promise<ChangeEvent["data"] | null> {
    const doc = await this.readFsDocument(relPath);
    if (!doc) {
      const existing = await this.store.getDocument(relPath);
      if (!existing || existing.origin !== "wiki") return null;
      await this.store.deleteDocument(relPath);
      return this.emit(relPath, "deleted", existing.hash, null, attribution);
    }
    const existing = await this.store.getDocument(relPath);
    if (existing && existing.hash === doc.hash && existing.origin === "wiki") {
      return null;
    }
    await this.indexDocument(doc);
    return this.emit(
      relPath,
      existing ? "modified" : "created",
      existing?.hash ?? null,
      doc.hash,
      attribution,
    );
  }

  /** Index content that is not backed by WIKI_ROOT (connector material). */
  async indexExternalContent(input: {
    relPath: string;
    content: string;
    origin: string;
    title?: string;
    contentType?: string;
    mtime?: number;
  }): Promise<void> {
    const content = input.content;
    const hash = hashContent(content);
    const existing = await this.store.getDocument(input.relPath);
    if (existing && existing.hash === hash) return;

    const isMd = input.relPath.endsWith(".md");
    const parsed = isMd ? parseMarkdown(content) : null;
    const body = parsed?.body ?? content;
    const frontmatter = parsed?.frontmatter ?? {};
    const links = parsed ? extractLinks(body) : [];
    const wikilinks = parsed ? extractWikilinks(body) : [];
    const resolvedTargets = [...links, ...wikilinks]
      .map((link) => resolveLinkTarget(link, input.relPath))
      .filter((t): t is string => t !== null)
      .map((t) => `/${t}`);

    const doc: DocumentInput = {
      rel_path: input.relPath,
      kind: "doc",
      origin: input.origin,
      title:
        input.title ??
        strField(frontmatter.title) ??
        basenameTitle(input.relPath),
      summary: strField(frontmatter.description),
      body,
      frontmatter,
      word_count: countWords(body),
      outgoing_links: dedupe(resolvedTargets),
      hash,
      mtime: input.mtime ?? Date.now(),
      content_type: input.contentType ?? "text/markdown",
      okf_type: strField(frontmatter.type),
      tags: strArray(frontmatter.tags),
      status: strField(frontmatter.status),
      stale_after: strField(frontmatter.stale_after),
      resource: strField(frontmatter.resource),
      generated_by: generatedBy(frontmatter),
      generated_at: generatedAt(frontmatter),
      verified: normalizeVerified(frontmatter.verified),
      provenance: provenance(frontmatter),
    };
    await this.indexDocument(doc);
  }

  async removeDocument(relPath: string): Promise<void> {
    await this.store.deleteDocument(relPath);
  }

  /** Full rebuild of the wiki-origin index from WIKI_ROOT. */
  async reindexAll(): Promise<number> {
    await this.store.deleteDerivedForOrigin("wiki");
    const files = walkFiles(this.wikiRoot);
    let count = 0;
    for (const rel of files) {
      try {
        await this.handleFileChange(rel);
        count += 1;
      } catch (err) {
        this.log(`reindex failed for ${rel}`, err);
      }
    }
    return count;
  }

  /** Remove all indexed documents for a connector origin. */
  async removeOriginDocuments(origin: string): Promise<void> {
    await this.store.deleteDerivedForOrigin(origin);
  }

  // ------------------------------------------------------------------

  async indexDocument(doc: DocumentInput): Promise<void> {
    await this.store.upsertDocument(doc);
    const record = await this.store.getDocument(doc.rel_path);
    if (!record) return;

    const chunks = buildChunks(record);
    await this.store.replaceChunks(record.id, chunks);

    const edgeTargets = record.outgoing_links
      .map((link) => link.replace(/^\//, ""))
      .filter((t) => t.length > 0 && t !== record.rel_path);
    await this.store.replaceEdges(record.rel_path, edgeTargets);

    const embedder = this.flags.embedder();
    if (embedder && chunks.length > 0) {
      this.enqueueEmbed(record.id);
    }
  }

  private enqueueEmbed(documentId: string): void {
    if (this.queued.has(documentId)) return;
    this.queued.add(documentId);
    this.embedQueue.push(documentId);
    void this.drainEmbeds();
  }

  private async drainEmbeds(): Promise<void> {
    if (this.draining) return;
    this.draining = true;
    try {
      while (this.embedQueue.length > 0) {
        const documentId = this.embedQueue.shift();
        if (!documentId) continue;
        this.queued.delete(documentId);
        if (await this.flags.distillEnabled()) {
          await this.distillDocument(documentId);
        }
        await this.embedDocument(documentId);
      }
    } finally {
      this.draining = false;
    }
  }

  /** Optional LLM distillation: extract a searchable question/summary per chunk. */
  private async distillDocument(documentId: string): Promise<void> {
    const llm = this.flags.llm();
    if (!llm) return;
    try {
      const chunks = await this.store.getChunksForDocument(documentId);
      for (const chunk of chunks.slice(0, 12)) {
        if (chunk.distilled) continue;
        const raw = await llm.chat(
          [
            {
              role: "system",
              content:
                'Extract from the passage: a search question an engineer would type, and a one-sentence summary. Respond ONLY with JSON {"question":"...","summary":"..."}.',
            },
            { role: "user", content: chunk.content.slice(0, 2000) },
          ],
          { temperature: 0, maxTokens: 200 },
        );
        const start = raw.indexOf("{");
        const end = raw.lastIndexOf("}");
        if (start === -1 || end <= start) continue;
        try {
          const parsed = JSON.parse(raw.slice(start, end + 1)) as {
            question?: unknown;
            summary?: unknown;
          };
          this.store.setChunkDistilled(chunk.id, {
            question:
              typeof parsed.question === "string" ? parsed.question : undefined,
            summary:
              typeof parsed.summary === "string" ? parsed.summary : undefined,
          });
        } catch {
          // malformed model output: skip this chunk
        }
      }
    } catch (err) {
      this.log(`distillation failed for document ${documentId}`, err);
    }
  }

  private async embedDocument(documentId: string): Promise<void> {
    const embedder = this.flags.embedder();
    if (!embedder) return;
    try {
      const chunks = await this.store.getChunksForDocument(documentId);
      if (chunks.length === 0) return;
      const vectors = await embedder.embed(
        chunks.map((c) => {
          const d = c.distilled;
          const prefix = [d?.question, d?.summary].filter(Boolean).join("\n");
          return prefix ? `${prefix}\n${c.content}` : c.content;
        }),
      );
      const embeddedAt = new Date().toISOString();
      await this.store.upsertEmbeddings(
        chunks.map((c, i) => ({ chunkId: c.id, vector: vectors[i] })),
        embedder.model,
        embeddedAt,
      );
    } catch (err) {
      this.log(`embedding failed for document ${documentId}`, err);
    }
  }

  private async readFsDocument(relPath: string): Promise<DocumentInput | null> {
    const absPath = path.join(this.wikiRoot, relPath);
    if (!existsSync(absPath) || !statSync(absPath).isFile()) return null;
    const stat = statSync(absPath);

    if (relPath.endsWith(".md")) {
      const page = readPage(this.wikiRoot, relPath);
      if (!page) return null;
      const raw = readFileSync(absPath, "utf8");
      const parsed = matter(raw);
      const links = extractLinks(page.body);
      const wikilinks = extractWikilinks(page.body);
      // Store pre-resolved, bundle-absolute targets so consumers (graph,
      // edges) never re-resolve raw link text.
      const resolvedTargets = [...links, ...wikilinks]
        .map((link) => resolveLinkTarget(link, relPath))
        .filter((t): t is string => t !== null)
        .map((t) => `/${t}`);
      return {
        rel_path: relPath,
        kind: "page",
        origin: "wiki",
        title: page.title,
        summary: page.summary,
        body: page.body,
        frontmatter: page.frontmatter,
        word_count: page.word_count,
        outgoing_links: dedupe(resolvedTargets),
        hash: page.hash,
        mtime: page.mtime,
        content_type: "text/markdown",
        okf_type: strField(parsed.data.type),
        tags: strArray(parsed.data.tags),
        status: strField(parsed.data.status),
        stale_after: strField(parsed.data.stale_after),
        resource: strField(parsed.data.resource),
        generated_by: generatedBy(parsed.data),
        generated_at: generatedAt(parsed.data),
        verified: normalizeVerified(parsed.data.verified),
        provenance: provenance(parsed.data),
        updated_at: page.updated_at,
        updated_by: page.updated_by,
      };
    }

    const { content } = readFileAtomic(absPath);
    const ext = path.extname(relPath).toLowerCase();
    const language = detectLanguage(relPath);
    const chunkable = TEXT_CHUNKABLE_EXTENSIONS.has(ext) || language !== null;
    return {
      rel_path: relPath,
      kind: "source",
      origin: "wiki",
      title: basenameTitle(relPath),
      summary: null,
      body: chunkable ? content : "",
      frontmatter: {},
      word_count: chunkable ? countWords(content) : 0,
      outgoing_links: [],
      hash: hashContent(content),
      mtime: Math.floor(stat.mtimeMs),
      content_type: inferContentType(ext),
    };
  }

  private emit(
    relPath: string,
    changeType: "created" | "modified" | "deleted",
    oldHash: string | null,
    newHash: string | null,
    attribution: FileAttribution,
  ): ChangeEvent["data"] {
    const change: ChangeEvent["data"] = {
      id: ulid(),
      rel_path: relPath,
      change_type: changeType,
      old_hash: oldHash,
      new_hash: newHash,
      source: attribution.source,
      operation_id: attribution.operationId,
      detected_at: new Date().toISOString(),
    };
    void this.store.insertChange(change);
    if (attribution.source === "api" && this.changeEmitter) {
      this.changeEmitter({ type: "change", data: change });
    }
    return change;
  }
}

function parseMarkdown(raw: string): {
  frontmatter: Record<string, unknown>;
  body: string;
} {
  const parsed = matter(raw);
  return {
    frontmatter: (parsed.data ?? {}) as Record<string, unknown>,
    body: parsed.content,
  };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function buildChunks(record: {
  kind: DocumentInput["kind"];
  rel_path: string;
  body: string;
}): ChunkInput[] {
  if (record.kind === "page") {
    return chunkMarkdown(record.body).map((c) => ({
      ordinal: c.ordinal,
      heading_path: c.headingPath,
      content: c.content,
    }));
  }
  const language = detectLanguage(record.rel_path);
  if (language === null && record.kind === "source") return [];
  return chunkCode(record.body, language).map((c) => ({
    ordinal: c.ordinal,
    heading_path: c.headingPath,
    content: c.content,
  }));
}

function walkFiles(root: string): string[] {
  const out: string[] = [];
  const visit = (dir: string) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      const rel = relativeToWiki(root, full);
      if (isIgnoredPath(rel)) continue;
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile()) out.push(rel);
    }
  };
  visit(root);
  return out;
}

function strField(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

function strArray(v: unknown): string[] {
  return Array.isArray(v)
    ? v.filter((x): x is string => typeof x === "string")
    : [];
}

function generatedBy(fm: Record<string, unknown>): string | null {
  const gen = fm.generated;
  if (gen && typeof gen === "object" && "by" in gen) {
    const by = (gen as Record<string, unknown>).by;
    return typeof by === "string" ? by : null;
  }
  return null;
}

function generatedAt(fm: Record<string, unknown>): string | null {
  const gen = fm.generated;
  if (gen && typeof gen === "object" && "at" in gen) {
    const at = (gen as Record<string, unknown>).at;
    return typeof at === "string" ? at : null;
  }
  return null;
}

function provenance(
  fm: Record<string, unknown>,
): Array<Record<string, unknown>> | null {
  if (!Array.isArray(fm.sources)) return null;
  return fm.sources.filter(
    (entry): entry is Record<string, unknown> =>
      entry !== null && typeof entry === "object",
  );
}

function countWords(text: string): number {
  return text.split(/\s+/).filter(Boolean).length;
}

function basenameTitle(relPath: string): string {
  const base = path.basename(relPath, path.extname(relPath));
  return base.replace(/[-_]/g, " ");
}

function inferContentType(ext: string): string {
  const map: Record<string, string> = {
    ".txt": "text/plain",
    ".csv": "text/csv",
    ".json": "application/json",
    ".yaml": "text/yaml",
    ".yml": "text/yaml",
    ".toml": "text/plain",
    ".html": "text/html",
    ".pdf": "application/pdf",
  };
  return map[ext] ?? "application/octet-stream";
}

function dedupe(items: string[]): string[] {
  return [...new Set(items)];
}
