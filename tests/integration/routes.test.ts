import { describe, it, expect, beforeEach, afterEach } from "vitest";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync } from "node:fs";
import type { Hono } from "hono";
import { createApp } from "../../src/app.js";
import { loadConfig } from "../../src/config.js";
import { createStore } from "../../src/store/index.js";
import type { Store } from "../../src/store/types.js";
import { createServices } from "../../src/services/container.js";
import { createBroadcaster } from "../../src/services/broadcaster.js";

function makeEnv(): {
  root: string;
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
  process.env.DB_BACKEND = "sqlite";
  process.env.DB_PATH = dbPath;

  const config = loadConfig();
  return {
    root,
    config,
    cleanup: () => {
      rmSync(root, { recursive: true, force: true });
      delete process.env.WIKI_ROOT;
      delete process.env.PORT;
      delete process.env.HOST;
      delete process.env.API_KEYS;
      delete process.env.PUBLIC_READ;
      delete process.env.DB_BACKEND;
      delete process.env.DB_PATH;
    },
  };
}

const AUTH = {
  Authorization: "Bearer key1",
  "Content-Type": "application/json",
};

describe("route integration", () => {
  let env: ReturnType<typeof makeEnv>;
  let store: Store;

  beforeEach(async () => {
    env = makeEnv();
    store = await createStore(env.config);
  });

  afterEach(async () => {
    await store.close();
    env.cleanup();
  });

  async function app(): Promise<Hono<{ Variables: never }>> {
    const services = await createServices(env.config, store);
    return createApp({
      config: env.config,
      store,
      services,
      broadcaster: createBroadcaster(),
    }) as unknown as Hono<{ Variables: never }>;
  }

  it("GET /health", async () => {
    const res = await (await app()).request("/health");
    expect(res.status).toBe(200);
    const json = (await res.json()) as Record<string, unknown>;
    expect(json.status).toBe("ok");
    expect(json.wiki_root).toBe(env.root);
  });

  it("PUT and GET /v1/pages/:path stamps OKF attribution", async () => {
    const hono = await app();
    const put = await hono.request("/v1/pages/wiki/entities/OpenAI.md", {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({
        content: "# OpenAI\n\nA company.",
        frontmatter: { tags: ["ai"] },
      }),
    });
    expect(put.status).toBe(200);

    const get = await hono.request("/v1/pages/wiki/entities/OpenAI.md");
    expect(get.status).toBe(200);
    const json = (await get.json()) as {
      body: string;
      frontmatter: Record<string, unknown>;
      updated_by: string;
      hash: string;
    };
    expect(json.body.trim()).toBe("# OpenAI\n\nA company.");
    expect(json.frontmatter.tags).toEqual(["ai"]);
    expect(json.updated_by).toBe("test");
    expect(typeof json.hash).toBe("string");
    const generated = json.frontmatter.generated as { by?: string };
    expect(generated.by).toBe("test/wikillm-api");
  });

  it("returns 409 on stale write", async () => {
    const hono = await app();
    const put1 = await hono.request("/v1/pages/wiki/note.md", {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({ content: "v1" }),
    });
    expect(put1.status).toBe(200);

    const put2 = await hono.request("/v1/pages/wiki/note.md", {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({ content: "v2", ifMatch: "badhash" }),
    });
    expect(put2.status).toBe(409);
    const json = (await put2.json()) as {
      error: string;
      current: { content: string };
    };
    expect(json.error).toBe("conflict");
    expect(json.current.content.trim()).toBe("v1");
  });

  it("writes and reads raw sources", async () => {
    const hono = await app();
    const post = await hono.request("/v1/sources/raw/articles/example.md", {
      method: "POST",
      headers: { Authorization: "Bearer key1", "Content-Type": "text/plain" },
      body: "source content",
    });
    expect(post.status).toBe(201);

    const get = await hono.request("/v1/sources/raw/articles/example.md");
    expect(get.status).toBe(200);
    const json = (await get.json()) as { rel_path: string };
    expect(json.rel_path).toBe("raw/articles/example.md");
  });

  it("appends and reads log in OKF date-grouped format", async () => {
    const hono = await app();
    const post = await hono.request("/v1/log/append", {
      method: "POST",
      headers: AUTH,
      body: JSON.stringify({ message: "ingested article" }),
    });
    expect(post.status).toBe(201);

    const get = await hono.request("/v1/log");
    expect(get.status).toBe(200);
    const json = (await get.json()) as {
      entries: Array<{ message: string }>;
    };
    expect(json.entries[0].message).toBe("ingested article");
  });

  it("returns changes feed after page write", async () => {
    const hono = await app();
    await hono.request("/v1/pages/wiki/x.md", {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({ content: "hello" }),
    });

    const changes = await hono.request("/v1/changes");
    expect(changes.status).toBe(200);
    const json = (await changes.json()) as {
      changes: Array<{ rel_path: string }>;
    };
    expect(json.changes.length).toBeGreaterThan(0);
    expect(json.changes[0].rel_path).toBe("wiki/x.md");
  });

  it(
    "performs batch ingest and refreshes index",
    { timeout: 5000 },
    async () => {
      const hono = await app();
      const res = await hono.request("/v1/ingest", {
        method: "POST",
        headers: AUTH,
        body: JSON.stringify({
          source: {
            title: "Article A",
            rel_path: "raw/article-a.md",
            content: "# Article A",
          },
          operations: [
            {
              rel_path: "wiki/summaries/Article A.md",
              content: "Summary of A",
            },
            { rel_path: "wiki/entities/A.md", content: "Entity A" },
          ],
          logEntry: "Article A",
        }),
      });
      expect(res.status).toBe(200);
      const json = (await res.json()) as {
        success: boolean;
        results: Array<{ success: boolean }>;
      };
      expect(json.success).toBe(true);
      expect(json.results.every((r) => r.success)).toBe(true);

      const index = await hono.request("/v1/index");
      expect(index.status).toBe(200);
      const idxJson = (await index.json()) as { content: string };
      expect(idxJson.content).toContain("Article A");
    },
  );

  it("searches indexed pages via hybrid search route", async () => {
    const hono = await app();
    await hono.request("/v1/pages/wiki/kb/vector-search.md", {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({
        content: "# Vector search\n\nHNSW indexes power semantic retrieval.",
        frontmatter: { type: "Concept", tags: ["retrieval"] },
      }),
    });

    const res = await hono.request("/v1/search?q=HNSW%20semantic");
    expect(res.status).toBe(200);
    const json = (await res.json()) as {
      results: Array<{ rel_path: string }>;
    };
    expect(json.results.length).toBeGreaterThan(0);
    expect(json.results[0].rel_path).toBe("wiki/kb/vector-search.md");
  });
});
