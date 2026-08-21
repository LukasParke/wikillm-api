import { describe, it, expect, afterAll, beforeAll } from "vitest";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { SqliteStore, createSqliteDatabase } from "../../src/store/sqlite.js";
import { PostgresStore } from "../../src/store/pg.js";
import type { DocumentInput, Store } from "../../src/store/types.js";
import type { Config } from "../../src/config.js";

function sampleDoc(overrides: Partial<DocumentInput> = {}): DocumentInput {
  return {
    rel_path: "wiki/entities/openai.md",
    kind: "page",
    origin: "wiki",
    title: "OpenAI",
    summary: "An AI company.",
    body: "# OpenAI\n\nOpenAI builds large language models and the GPT family.",
    frontmatter: { type: "Company", tags: ["ai"] },
    word_count: 10,
    outgoing_links: ["[GPT](../concepts/gpt.md)"],
    hash: "a".repeat(64),
    mtime: 1_700_000_000_000,
    content_type: "text/markdown",
    okf_type: "Company",
    tags: ["ai"],
    status: "stable",
    stale_after: null,
    resource: null,
    generated_by: "human:test",
    generated_at: "2026-01-01T00:00:00Z",
    verified: [{ by: "human:test", at: "2026-01-02T00:00:00Z" }],
    provenance: null,
    ...overrides,
  };
}

async function makeStore(): Promise<Store> {
  const dir = mkdtempSync(path.join(tmpdir(), "wikillm-store-"));
  const dbPath = path.join(dir, "test.db");
  const db = await createSqliteDatabase(dbPath);
  const store = new SqliteStore(db);
  await store.migrate();
  return store;
}

async function makePgStore(): Promise<Store> {
  return PostgresStore.connect(process.env.TEST_PG_URL!);
}

const backends: Array<[string, () => Promise<Store>]> = [["sqlite", makeStore]];
if (process.env.TEST_PG_URL) backends.push(["postgres", makePgStore]);

describe.each(backends)("store backend: %s", (name, factory) => {
  let store: Store;

  beforeAll(async () => {
    store = await (factory as () => Promise<Store>)();
    await store.migrate();
  });

  afterAll(async () => {
    await store.close();
    if (name === "sqlite") {
      // temp dirs are cleaned by the OS; nothing to do here
    }
  });

  it("upserts and reads documents with OKF fields", async () => {
    await store.upsertDocument(sampleDoc());
    const doc = await store.getDocument("wiki/entities/openai.md");
    expect(doc).not.toBeNull();
    expect(doc!.title).toBe("OpenAI");
    expect(doc!.okf_type).toBe("Company");
    expect(doc!.tags).toEqual(["ai"]);
    expect(doc!.verified).toEqual([
      { by: "human:test", at: "2026-01-02T00:00:00Z" },
    ]);
  });

  it("keeps document id stable across upserts", async () => {
    const first = await store.getDocument("wiki/entities/openai.md");
    await store.upsertDocument({ ...sampleDoc(), summary: "Updated." });
    const second = await store.getDocument("wiki/entities/openai.md");
    expect(first!.id).toBe(second!.id);
    expect(second!.summary).toBe("Updated.");
  });

  it("paginates listDocuments by rel_path cursor", async () => {
    for (const p of ["wiki/a.md", "wiki/b.md", "wiki/c.md"]) {
      await store.upsertDocument({ ...sampleDoc(), rel_path: p });
    }
    const page1 = await store.listDocuments({ folder: "wiki", limit: 2 });
    expect(page1.items.map((d) => d.rel_path)).toEqual([
      "wiki/a.md",
      "wiki/b.md",
    ]);
    expect(page1.nextCursor).toBe("wiki/b.md");
    const page2 = await store.listDocuments({
      folder: "wiki",
      limit: 2,
      cursor: page1.nextCursor,
    });
    expect(page2.items.length).toBeGreaterThan(0);
  });

  it("replaces chunks and finds them via full-text search", async () => {
    await store.upsertDocument(sampleDoc());
    const doc = await store.getDocument("wiki/entities/openai.md");
    await store.replaceChunks(doc!.id, [
      {
        ordinal: 0,
        heading_path: "OpenAI",
        content:
          "OpenAI builds large language models such as GPT for language understanding.",
      },
      {
        ordinal: 1,
        heading_path: "OpenAI > History",
        content: "The organization was founded to develop safe AGI systems.",
      },
    ]);

    const hits = await store.searchFts("GPT language", {
      limit: 5,
      filters: { kinds: ["page"] },
    });
    expect(hits.length).toBeGreaterThan(0);
    expect(hits[0].rel_path).toBe("wiki/entities/openai.md");

    const scoped = await store.searchFts("AGI", {
      limit: 5,
      filters: { pathPrefixes: ["raw"] },
    });
    expect(scoped).toHaveLength(0);
  });

  it("tracks backlinks through the edge table", async () => {
    await store.replaceEdges("wiki/concepts/gpt.md", [
      "wiki/entities/openai.md",
    ]);
    await store.replaceEdges("wiki/overview.md", ["wiki/entities/openai.md"]);
    const links = await store.backlinks("wiki/entities/openai.md");
    expect(links.sort()).toEqual(["wiki/concepts/gpt.md", "wiki/overview.md"]);
    await store.replaceEdges("wiki/concepts/gpt.md", []);
    const after = await store.backlinks("wiki/entities/openai.md");
    expect(after).toEqual(["wiki/overview.md"]);
  });

  it("stores connector config and watermark state", async () => {
    const now = new Date().toISOString();
    await store.putConnector({
      id: "git-docs",
      kind: "git",
      config: { url: "https://example.com/repo.git" },
      enabled: true,
      created_at: now,
      updated_at: now,
    });
    const conn = await store.getConnector("git-docs");
    expect(conn?.kind).toBe("git");
    await store.setConnectorState("git-docs", { commit: "abc123" });
    const state = await store.getConnectorState("git-docs");
    expect(state).toEqual({ commit: "abc123" });
    expect(await store.deleteConnector("git-docs")).toBe(true);
    expect(await store.deleteConnector("git-docs")).toBe(false);
  });

  it("manages projects", async () => {
    await store.putProject({
      name: "compiler",
      description: "Compiler team scope",
      prefixes: ["wiki/compiler"],
      connectors: [],
    });
    const project = await store.getProject("compiler");
    expect(project?.prefixes).toEqual(["wiki/compiler"]);
    await store.putProject({
      name: "compiler",
      prefixes: ["wiki/compiler", "raw/compiler"],
      connectors: [],
    });
    expect((await store.getProject("compiler"))?.prefixes).toHaveLength(2);
    expect(
      await store.listProjects().then((ps) => ps.map((p) => p.name)),
    ).toContain("compiler");
    expect(await store.deleteProject("compiler")).toBe(true);
  });

  it("records query analytics and feedback", async () => {
    const before = await store.statsOverview();
    await store.recordQuery({
      id: `q-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      created_at: new Date().toISOString(),
      query: "what is gpt",
      mode: "hybrid",
      project: null,
      latency_ms: 12,
      result_count: 3,
      zero_hit: false,
      top_paths: ["wiki/entities/openai.md"],
      source: "test",
      error: null,
    });
    await store.recordFeedback({ query_id: "q1", helpful: true });
    const after = await store.statsOverview();
    expect(after.queries).toBe(before.queries + 1);
    expect(after.feedback_total).toBe(before.feedback_total + 1);
    expect(after.feedback_helpful).toBe(before.feedback_helpful + 1);
  });

  it("supports vector search where the backend can", async () => {
    if (!store.supportsVector()) return; // SQLite is FTS-only by design
    await store.upsertDocument(
      sampleDoc({ rel_path: "wiki/vec/target.md" }),
    );
    const doc = await store.getDocument("wiki/vec/target.md");
    await store.replaceChunks(doc!.id, [
      { ordinal: 0, heading_path: null, content: "semantic target chunk" },
    ]);
    const chunks = await store.getChunksForDocument(doc!.id);
    const dims = Number(process.env.EMBEDDING_DIMS ?? 1536);
    const base = Array.from({ length: dims }, (_, i) => (i % 7) / 7);
    await store.upsertEmbeddings(
      [{ chunkId: chunks[0].id, vector: base }],
      "test-model",
      new Date().toISOString(),
    );
    const hits = await store.searchVector(base, { limit: 5 });
    expect(hits.length).toBeGreaterThan(0);
    expect(hits[0].rel_path).toBe("wiki/vec/target.md");
    expect(hits[0].score).toBeGreaterThan(0.999);
    const unembedded = await store.listUnembeddedChunks(10);
    expect(unembedded.find((c) => c.document_id === doc!.id)).toBeUndefined();
  });

  it("filters search results by trust, freshness, tags, and type", async () => {
    const mk = (
      rel_path: string,
      over: Partial<DocumentInput>,
    ): DocumentInput => ({
      ...sampleDoc({ rel_path }),
      body: `Filterable body ${rel_path} with unique term zanzibar.`,
      ...over,
    });
    await store.upsertDocument(
      mk("wiki/t/human.md", {
        okf_type: "Concept",
        tags: ["alpha"],
        status: "stable",
        verified: [{ by: "human:luke", at: "2026-01-01T00:00:00Z" }],
      }),
    );
    await store.upsertDocument(
      mk("wiki/t/machine.md", {
        okf_type: "Note",
        tags: ["beta"],
        status: "draft",
        verified: [{ by: "process:nightly", at: "2026-01-01T00:00:00Z" }],
      }),
    );
    await store.upsertDocument(
      mk("wiki/t/stale.md", {
        verified: null,
        okf_type: "Concept",
        tags: ["alpha"],
        status: "deprecated",
        stale_after: "2000-01-01T00:00:00Z",
      }),
    );

    // search operates over chunks; index one per document
    for (const rel of [
      "wiki/t/human.md",
      "wiki/t/machine.md",
      "wiki/t/stale.md",
    ]) {
      const doc = await store.getDocument(rel);
      if (!doc) throw new Error(`fixture missing: ${rel}`);
      await store.replaceChunks(doc.id, [
        { ordinal: 0, heading_path: null, content: `zanzibar ${rel}` },
      ]);
    }
    // stale.md must not inherit the fixture's default human verification
    await store.upsertDocument({
      ...(await store.getDocument("wiki/t/stale.md"))!,
      verified: null,
    });

    const human = await store.searchFts("zanzibar", {
      limit: 10,
      filters: { trustMin: "human-reviewed" },
    });
    expect(human.map((h) => h.rel_path)).toEqual(["wiki/t/human.md"]);

    const fresh = await store.searchFts("zanzibar", {
      limit: 10,
      filters: { freshOnly: true },
    });
    expect(fresh.map((h) => h.rel_path).sort()).toEqual([
      "wiki/t/human.md",
      "wiki/t/machine.md",
    ]);

    const tagged = await store.searchFts("zanzibar", {
      limit: 10,
      filters: { tags: ["alpha"] },
    });
    expect(tagged.map((h) => h.rel_path).sort()).toEqual([
      "wiki/t/human.md",
      "wiki/t/stale.md",
    ]);

    const typed = await store.searchFts("zanzibar", {
      limit: 10,
      filters: { okf_types: ["Note"] },
    });
    expect(typed.map((h) => h.rel_path)).toEqual(["wiki/t/machine.md"]);

    const drafted = await store.searchFts("zanzibar", {
      limit: 10,
      filters: { statuses: ["stable"] },
    });
    expect(drafted.map((h) => h.rel_path)).toEqual(["wiki/t/human.md"]);
  });

  it("deletes derived data by origin", async () => {
    await store.upsertDocument({
      ...sampleDoc(),
      rel_path: "ext/x.md",
      origin: "web-x",
    });
    await store.upsertDocument({ ...sampleDoc(), rel_path: "wiki/keep.md" });
    const ext = await store.getDocument("ext/x.md");
    if (ext)
      await store.replaceChunks(ext.id, [
        { ordinal: 0, heading_path: null, content: "external doc text" },
      ]);
    await store.deleteDerivedForOrigin("web-x");
    expect(await store.getDocument("ext/x.md")).toBeNull();
    expect(await store.getDocument("wiki/keep.md")).not.toBeNull();
  });
});
