import { describe, it, expect, beforeEach, afterEach } from "vitest";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync, existsSync } from "node:fs";
import type { Hono } from "hono";
import { createApp } from "../../src/app.js";
import { loadConfig } from "../../src/config.js";
import { createStore } from "../../src/store/index.js";
import type { Store } from "../../src/store/types.js";
import { createServices } from "../../src/services/container.js";
import { createBroadcaster } from "../../src/services/broadcaster.js";

function makeEnv(over: Record<string, string | undefined> = {}) {
  const root = path.join(
    os.tmpdir(),
    `wikillm-bulk-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  mkdirSync(root, { recursive: true });
  const env: Record<string, string | undefined> = {
    WIKI_ROOT: root,
    PORT: "0",
    HOST: "127.0.0.1",
    API_KEYS: "admin:adminkey:*:admin",
    PUBLIC_READ: "true",
    DB_BACKEND: "sqlite",
    DB_PATH: path.join(root, "test.db"),
    LOG_LEVEL: "error",
    ...over,
  };
  for (const [k, v] of Object.entries(env)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  return {
    config: loadConfig(),
    cleanup: () => {
      for (const k of Object.keys(env)) delete process.env[k];
      rmSync(root, { recursive: true, force: true });
    },
  };
}

const AUTH = {
  Authorization: "Bearer adminkey",
  "Content-Type": "application/json",
};

describe("documents listing, downloads, bulk ops, webhooks", () => {
  let hono: Hono<{ Variables: never }>;
  let store: Store;
  let cleanup: () => void;

  beforeEach(async () => {
    const env = makeEnv();
    cleanup = env.cleanup;
    store = await createStore(env.config);
    const services = await createServices(env.config, store);
    hono = createApp({
      config: env.config,
      store,
      services,
      broadcaster: createBroadcaster(),
    }) as unknown as Hono<{ Variables: never }>;
  });

  afterEach(async () => {
    await store.close();
    cleanup();
  });

  async function putPage(
    rel: string,
    content: string,
    frontmatter?: Record<string, unknown>,
  ) {
    return hono.request(`/v1/pages/${rel}`, {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({ content, frontmatter }),
    });
  }

  it("lists documents across kinds with filters", async () => {
    await putPage("wiki/entities/a.md", "# A", {
      type: "Concept",
      tags: ["x"],
    });
    await hono.request("/v1/sources/raw/doc.txt", {
      method: "POST",
      headers: {
        Authorization: "Bearer adminkey",
        "Content-Type": "text/plain",
      },
      body: "source body",
    });

    const all = await hono.request("/v1/documents");
    const allJson = (await all.json()) as {
      items: Array<{ rel_path: string }>;
    };
    const paths = allJson.items.map((i) => i.rel_path);
    expect(paths).toContain("wiki/entities/a.md");
    expect(paths).toContain("raw/doc.txt");

    const pagesOnly = (await (
      await hono.request("/v1/documents?kind=page")
    ).json()) as { items: Array<{ kind: string }> };
    expect(pagesOnly.items.every((i) => i.kind === "page")).toBe(true);

    const tagged = (await (
      await hono.request("/v1/documents?tags=x")
    ).json()) as { items: Array<{ rel_path: string }> };
    expect(tagged.items.map((i) => i.rel_path)).toEqual(["wiki/entities/a.md"]);
  });

  it("serves collection ETag with 304 revalidation", async () => {
    await putPage("wiki/e.md", "content");
    const first = await hono.request("/v1/documents");
    const etag = first.headers.get("etag");
    expect(etag).toBeTruthy();
    const second = await hono.request("/v1/documents", {
      headers: { "If-None-Match": etag! },
    });
    expect(second.status).toBe(304);
  });

  it("downloads raw page markdown and source bytes", async () => {
    await putPage("wiki/dl.md", "# Download me\n\nBody here.");
    await hono.request("/v1/sources/raw/files/bin.txt", {
      method: "POST",
      headers: {
        Authorization: "Bearer adminkey",
        "Content-Type": "text/plain",
      },
      body: "raw-bytes-123",
    });

    const raw = await hono.request("/v1/pages/wiki/dl.md/raw");
    expect(raw.status).toBe(200);
    expect(raw.headers.get("content-type")).toContain("text/markdown");
    expect(await raw.text()).toContain("# Download me");

    const srcContent = await hono.request(
      "/v1/sources/raw/files/bin.txt/content",
    );
    expect(srcContent.status).toBe(200);
    expect(await srcContent.text()).toBe("raw-bytes-123");

    const dispatch = await hono.request("/v1/documents/wiki/dl.md/content");
    expect(dispatch.status).toBe(200);
    expect(await dispatch.text()).toContain("Download me");
  });

  it("batch writes and deletes with preflight OCC", async () => {
    const put = await putPage("wiki/batch/existing.md", "v1");
    const putJson = (await put.json()) as { page: { hash: string } };

    const ok = await hono.request("/v1/pages/batch", {
      method: "POST",
      headers: AUTH,
      body: JSON.stringify({
        operations: [
          { rel_path: "wiki/batch/new.md", content: "created by batch" },
          {
            rel_path: "wiki/batch/existing.md",
            content: "v2",
            ifMatch: putJson.page.hash,
          },
          { rel_path: "wiki/batch/existing.md", delete: true },
        ],
      }),
    });
    expect(ok.status).toBe(200);
    expect(
      existsSync(path.join(makeWikiRoot(), "wiki/batch/existing.md")),
    ).toBe(false);

    function makeWikiRoot(): string {
      return process.env.WIKI_ROOT ?? "";
    }

    const conflict = await hono.request("/v1/pages/batch", {
      method: "POST",
      headers: AUTH,
      body: JSON.stringify({
        operations: [
          { rel_path: "wiki/batch/new2.md", content: "x", ifMatch: "badhash" },
        ],
      }),
    });
    expect(conflict.status).toBe(409);
    // preflight failure means nothing was written
    expect(
      existsSync(path.join(process.env.WIKI_ROOT ?? "", "wiki/batch/new2.md")),
    ).toBe(false);
  });

  it("bulk deletes pages and sources with per-op results", async () => {
    await putPage("wiki/del1.md", "one");
    await hono.request("/v1/sources/raw/del2.txt", {
      method: "POST",
      headers: {
        Authorization: "Bearer adminkey",
        "Content-Type": "text/plain",
      },
      body: "two",
    });

    const res = await hono.request("/v1/documents/delete", {
      method: "POST",
      headers: AUTH,
      body: JSON.stringify({
        rel_paths: ["wiki/del1.md", "raw/del2.txt", "nonexistent.md"],
      }),
    });
    expect(res.status).toBe(200);
    const json = (await res.json()) as {
      results: Array<{ rel_path: string; success: boolean; error?: string }>;
    };
    const byPath = new Map(json.results.map((r) => [r.rel_path, r]));
    expect(byPath.get("wiki/del1.md")?.success).toBe(true);
    expect(byPath.get("raw/del2.txt")?.success).toBe(true);
    expect(byPath.get("nonexistent.md")?.success).toBe(false);
    expect(byPath.get("nonexistent.md")?.error).toBe("not_found");
  });

  it("exports the graph as DOT", async () => {
    await putPage("wiki/g1.md", "see [[g2]] and [[g3]]", { type: "Note" });
    await putPage("wiki/g2.md", "backlink target", { type: "Note" });
    const res = await hono.request("/v1/graph/wiki/g1.md?format=dot");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toContain("graphviz");
    const dot = await res.text();
    expect(dot).toContain("digraph knowledge");
    expect(dot).toContain('"wiki/g1.md" -> "wiki/g2.md"');
  });

  it("exports filtered bundles incrementally", async () => {
    await putPage("wiki/inc/first.md", "first");
    const watermark = new Date().toISOString();
    await new Promise((r) => setTimeout(r, 50));
    await putPage("wiki/inc/second.md", "second");

    const res = await hono.request(
      `/v1/bundle/export?prefix=wiki/inc&since=${encodeURIComponent(watermark)}`,
      { headers: AUTH },
    );
    expect(res.status).toBe(200);
    expect(res.headers.get("x-exported-files")).toBe("1");

    const future = await hono.request(
      `/v1/bundle/export?prefix=wiki/inc&since=${encodeURIComponent(new Date(Date.now() + 60_000).toISOString())}`,
      { headers: AUTH },
    );
    expect(future.status).toBe(404);

    void putPage;
  });

  it("delivers signed webhooks with retries recorded", async () => {
    await hono.request("/v1/settings/webhook_secret", {
      method: "PUT",
      headers: AUTH,
      body: JSON.stringify({ value: "test-secret" }),
    });

    const received: Array<{ signature: boolean | null; body: string }> = [];
    const http = await import("node:http");
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => {
        received.push({
          signature: Boolean(req.headers["x-wikillm-signature"]),
          body,
        });
        res.writeHead(200).end("ok");
      });
    });
    await new Promise<void>((resolve, reject) => {
      server.once("listening", () => resolve());
      server.once("error", reject);
      server.listen(0, "127.0.0.1");
    });
    const address = server.address();
    const port = typeof address === "object" && address ? address.port : 0;
    try {
      const hook = await hono.request("/v1/webhooks", {
        method: "POST",
        headers: AUTH,
        body: JSON.stringify({
          url: `http://127.0.0.1:${port}/hook`,
          events: ["change"],
          prefixes: ["wiki/hooked/"],
        }),
      });
      console.log("HOOK CREATE:", hook.status, await hook.text());

      const putRes = await putPage("wiki/hooked/trigger.md", "webhook trigger");
      console.log("PUT:", putRes.status);
      const hooksAfter = await hono.request("/v1/webhooks", { headers: AUTH });
      console.log("WEBHOOKS AFTER:", await hooksAfter.text());
      const t0 = Date.now();
      while (received.length === 0 && Date.now() - t0 < 5000) {
        await new Promise((r) => setTimeout(r, 50));
      }
      expect(received.length).toBeGreaterThan(0);
      expect(received[0].signature).toBe(true);
      const parsed = JSON.parse(received[0].body) as {
        data: { rel_path: string };
      };
      expect(parsed.data.rel_path).toBe("wiki/hooked/trigger.md");

      const list = (await (
        await hono.request("/v1/webhooks", { headers: AUTH })
      ).json()) as { webhooks: Array<{ last_status: string }> };
      expect(list.webhooks[0].last_status).toBe("200");
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });
});
