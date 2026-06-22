import { describe, it, expect, beforeEach, afterEach } from "vitest";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { createApp } from "../../src/app.js";
import { loadConfig } from "../../src/config.js";
import { createDatabase, migrate } from "../../src/db/client.js";
import { syncFullCache } from "../../src/fs/watcher.js";
import { createBroadcaster } from "../../src/services/broadcaster.js";

function makeEnv(): {
  root: string;
  dbPath: string;
  config: ReturnType<typeof loadConfig>;
  cleanup: () => void;
} {
  const root = path.join(
    os.tmpdir(),
    `wikillm-int-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  const dbPath = path.join(root, "test.db");
  mkdirSync(root, { recursive: true });

  process.env.WIKI_ROOT = root;
  process.env.PORT = "0";
  process.env.HOST = "127.0.0.1";
  process.env.API_KEYS = "test:key1";
  process.env.PUBLIC_READ = "true";
  process.env.DB_PATH = dbPath;

  const config = loadConfig();
  return {
    root,
    dbPath,
    config,
    cleanup: () => {
      rmSync(root, { recursive: true, force: true });
      delete process.env.WIKI_ROOT;
      delete process.env.PORT;
      delete process.env.HOST;
      delete process.env.API_KEYS;
      delete process.env.PUBLIC_READ;
      delete process.env.DB_PATH;
    },
  };
}

describe("route integration", () => {
  let env: ReturnType<typeof makeEnv>;
  let db: Awaited<ReturnType<typeof createDatabase>>;

  beforeEach(async () => {
    env = makeEnv();
    db = await createDatabase(env.dbPath);
    migrate(db);
    syncFullCache(env.root, db);
  });

  afterEach(() => {
    db.close();
    env.cleanup();
  });

  function app() {
    return createApp({
      config: env.config,
      db,
      broadcaster: createBroadcaster(),
    });
  }

  it("GET /health", async () => {
    const res = await app().request("/health");
    expect(res.status).toBe(200);
    const json = (await res.json()) as any;
    expect(json.status).toBe("ok");
    expect(json.wiki_root).toBe(env.root);
  });

  it("PUT and GET /v1/pages/:path", async () => {
    const hono = app();
    const put = await hono.request("/v1/pages/wiki/entities/OpenAI.md", {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer key1",
      },
      body: JSON.stringify({
        content: "# OpenAI\n\nA company.",
        frontmatter: { tags: ["ai"] },
      }),
    });
    expect(put.status).toBe(200);

    const get = await hono.request("/v1/pages/wiki/entities/OpenAI.md");
    expect(get.status).toBe(200);
    const json = (await get.json()) as any;
    expect(json.body.trim()).toBe("# OpenAI\n\nA company.");
    expect(json.frontmatter.tags).toEqual(["ai"]);
    expect(json.updated_by).toBe("test");
    expect(typeof json.hash).toBe("string");
  });

  it("returns 409 on stale write", async () => {
    const hono = app();
    const put1 = await hono.request("/v1/pages/wiki/note.md", {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer key1",
      },
      body: JSON.stringify({ content: "v1" }),
    });
    expect(put1.status).toBe(200);

    const put2 = await hono.request("/v1/pages/wiki/note.md", {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer key1",
      },
      body: JSON.stringify({ content: "v2", ifMatch: "badhash" }),
    });
    expect(put2.status).toBe(409);
    const json = (await put2.json()) as any;
    expect(json.error).toBe("conflict");
    expect(json.current.content.trim()).toBe("v1");
  });

  it("writes and reads raw sources", async () => {
    const hono = app();
    const put = await hono.request("/v1/sources/raw/articles/example.md", {
      method: "POST",
      headers: { Authorization: "Bearer key1", "Content-Type": "text/plain" },
      body: "source content",
    });
    expect(put.status).toBe(201);

    const get = await hono.request("/v1/sources/raw/articles/example.md");
    expect(get.status).toBe(200);
    const json = (await get.json()) as any;
    expect(json.rel_path).toBe("raw/articles/example.md");
  });

  it("appends and reads log", async () => {
    const hono = app();
    const post = await hono.request("/v1/log/append", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer key1",
      },
      body: JSON.stringify({ message: "ingested article" }),
    });
    expect(post.status).toBe(201);

    const get = await hono.request("/v1/log");
    expect(get.status).toBe(200);
    const json = (await get.json()) as any;
    expect(json.entries[0].message).toBe("ingested article");
  });

  it("returns changes feed", async () => {
    const hono = app();
    await hono.request("/v1/pages/wiki/x.md", {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer key1",
      },
      body: JSON.stringify({ content: "hello" }),
    });

    const changes = await hono.request("/v1/changes");
    expect(changes.status).toBe(200);
    const json = (await changes.json()) as any;
    expect(json.changes.length).toBeGreaterThan(0);
    expect(json.changes[0].rel_path).toBe("wiki/x.md");
  });

  it("performs batch ingest", { timeout: 5000 }, async () => {
    const hono = app();
    const res = await hono.request("/v1/ingest", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer key1",
      },
      body: JSON.stringify({
        source: {
          title: "Article A",
          rel_path: "raw/article-a.md",
          content: "# Article A",
        },
        operations: [
          { rel_path: "wiki/summaries/Article A.md", content: "Summary of A" },
          { rel_path: "wiki/entities/A.md", content: "Entity A" },
        ],
        logEntry: "Article A",
      }),
    });
    expect(res.status).toBe(200);
    const json = (await res.json()) as any;
    expect(json.success).toBe(true);
    expect(json.results.every((r: any) => r.success)).toBe(true);

    const index = await hono.request("/v1/index");
    expect(index.status).toBe(200);
    const idxJson = (await index.json()) as any;
    expect(idxJson.content).toContain("Article A");
  });
});
