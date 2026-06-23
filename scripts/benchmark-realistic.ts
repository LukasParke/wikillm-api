#!/usr/bin/env bun
import { spawn } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";

const PORT = 4123;
const API_KEY = "bench-key";
const WARMUP_MS = 1500;

interface ScenarioResult {
  name: string;
  durationMs: number;
  requests: number;
  errors: number;
  throughput: number;
  latencyAvg: number;
  latencyP50: number;
  latencyP95: number;
  latencyP99: number;
}

interface ClientBehavior {
  name: string;
  weight: number;
  fn: (agent: http.Agent) => Promise<void>;
}

function percentile(sorted: number[], p: number): number {
  const idx = Math.max(0, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[idx];
}

function fmt(n: number): string {
  return Number.isFinite(n) ? n.toFixed(2) : "N/A";
}

function formatResult(r: ScenarioResult): string {
  return `
${r.name}
  Requests:      ${r.requests.toLocaleString()}
  Errors:        ${r.errors.toLocaleString()}
  Duration:      ${(r.durationMs / 1000).toFixed(2)}s
  Throughput:    ${r.throughput.toFixed(1)} req/s
  Latency avg:   ${fmt(r.latencyAvg)}ms
  Latency p50:   ${fmt(r.latencyP50)}ms
  Latency p95:   ${fmt(r.latencyP95)}ms
  Latency p99:   ${fmt(r.latencyP99)}ms
`;
}

function request(
  agent: http.Agent,
  method: string,
  reqPath: string,
  headers: Record<string, string>,
  body?: string,
): Promise<{ statusCode: number; body: string }> {
  return new Promise((resolve, reject) => {
    const allHeaders: Record<string, string> = body
      ? { ...headers, "Content-Length": String(Buffer.byteLength(body)) }
      : headers;
    const req = http.request(
      {
        agent,
        hostname: "127.0.0.1",
        port: PORT,
        method,
        path: reqPath,
        headers: allHeaders,
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          resolve({ statusCode: res.statusCode ?? 0, body: data });
        });
      },
    );
    req.on("error", reject);
    req.on("timeout", () => reject(new Error("timeout")));
    if (body) req.write(body);
    req.end();
  });
}

function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

function randomInt(min: number, max: number): number {
  return Math.floor(Math.random() * (max - min + 1)) + min;
}

function pick<T>(arr: T[]): T {
  return arr[Math.floor(Math.random() * arr.length)];
}

async function waitForServer(): Promise<void> {
  for (let i = 0; i < 50; i++) {
    try {
      const res = await fetch(`http://127.0.0.1:${PORT}/health`);
      if (res.ok) return;
    } catch {
      // not ready
    }
    await delay(100);
  }
  throw new Error("Server did not start");
}

async function seedWiki(): Promise<void> {
  const agent = new http.Agent({ keepAlive: true });
  const authHeaders = {
    Authorization: `Bearer ${API_KEY}`,
    "Content-Type": "application/json",
  };

  for (let i = 1; i <= 20; i++) {
    await request(
      agent,
      "POST",
      `/v1/sources/raw/article-${i}.md`,
      { Authorization: `Bearer ${API_KEY}`, "Content-Type": "text/plain" },
      `# Source ${i}\n\nThis is realistic source content for article ${i}. It contains enough text to be representative of a clipped article or imported note. Lorem ipsum dolor sit amet, consectetur adipiscing elit.`,
    );
  }

  await request(
    agent,
    "PUT",
    "/v1/pages/wiki/overview.md",
    authHeaders,
    JSON.stringify({
      content:
        "# Overview\n\nThis wiki covers AI concepts, entities, and summaries. See [[Concepts]] and [[Entities]].",
      frontmatter: { category: "meta", tags: ["overview"] },
    }),
  );

  await request(
    agent,
    "PUT",
    "/v1/pages/wiki/concepts.md",
    authHeaders,
    JSON.stringify({
      content:
        "# Concepts\n\nCore concepts include LLMs, RAG, fine-tuning, and wiki maintenance.",
      frontmatter: { category: "concepts", tags: ["llm", "rag"] },
    }),
  );

  for (let i = 1; i <= 100; i++) {
    await request(
      agent,
      "PUT",
      `/v1/pages/wiki/entities/entity-${i}.md`,
      authHeaders,
      JSON.stringify({
        content: `# Entity ${i}\n\nEntity ${i} is described here. Links to [[Concepts]], [[Overview]], and [[entity-${(i % 100) + 1}]].`,
        frontmatter: {
          category: "entities",
          tags: [pick(["ai", "llm", "company", "person"])],
          source: `article-${(i % 20) + 1}.md`,
        },
      }),
    );
  }

  for (let i = 1; i <= 20; i++) {
    await request(
      agent,
      "PUT",
      `/v1/pages/wiki/summaries/article-${i}-summary.md`,
      authHeaders,
      JSON.stringify({
        content: `# Summary of Article ${i}\n\nThis page summarizes [[raw/article-${i}.md|Article ${i}]] and links to [[entity-${i}]].`,
        frontmatter: { category: "summaries", tags: ["summary"] },
      }),
    );
  }

  await request(agent, "POST", "/v1/index/refresh", authHeaders);
  await request(
    agent,
    "POST",
    "/v1/log/append",
    authHeaders,
    JSON.stringify({ message: "seeded benchmark wiki", prefix: "bench" }),
  );

  agent.destroy();
}

async function runScenario(options: {
  name: string;
  clients: number;
  durationMs: number;
  behaviors: ClientBehavior[];
  thinkTimeMs?: number;
}): Promise<ScenarioResult> {
  const latencies: number[] = [];
  let requests = 0;
  let errors = 0;
  let stop = false;

  const totalWeight = options.behaviors.reduce((s, b) => s + b.weight, 0);
  const pickBehavior = () => {
    let r = Math.random() * totalWeight;
    for (const b of options.behaviors) {
      r -= b.weight;
      if (r <= 0) return b;
    }
    return options.behaviors[options.behaviors.length - 1];
  };

  const workers = Array.from({ length: options.clients }, async () => {
    const agent = new http.Agent({ keepAlive: true, maxSockets: 10 });
    while (!stop) {
      const behavior = pickBehavior();
      const start = performance.now();
      try {
        await behavior.fn(agent);
        requests++;
      } catch {
        errors++;
      } finally {
        latencies.push(performance.now() - start);
      }
      if (options.thinkTimeMs && options.thinkTimeMs > 0) {
        await delay(randomInt(0, options.thinkTimeMs));
      }
    }
    agent.destroy();
  });

  await delay(options.durationMs);
  stop = true;
  await Promise.all(workers);

  const sorted = latencies.slice().sort((a, b) => a - b);
  return {
    name: options.name,
    durationMs: options.durationMs,
    requests,
    errors,
    throughput: requests / (options.durationMs / 1000),
    latencyAvg: sorted.reduce((a, b) => a + b, 0) / sorted.length,
    latencyP50: percentile(sorted, 50),
    latencyP95: percentile(sorted, 95),
    latencyP99: percentile(sorted, 99),
  };
}

async function main() {
  const wikiRoot = path.join(os.tmpdir(), `wikillm-bench-${Date.now()}`);
  mkdirSync(path.join(wikiRoot, "wiki"), { recursive: true });

  const server = spawn("bun", ["run", "src/index.ts"], {
    env: {
      ...process.env,
      WIKI_ROOT: wikiRoot,
      PORT: String(PORT),
      HOST: "127.0.0.1",
      API_KEYS: `bench:${API_KEY}`,
      PUBLIC_READ: "true",
      LOG_LEVEL: "warn",
      DB_PATH: path.join(wikiRoot, "wikillm-api.db"),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  server.stdout?.on("data", () => {});
  server.stderr?.on("data", () => {});

  try {
    await waitForServer();
    console.log("Seeding realistic wiki...");
    await seedWiki();
    console.log("Warming up...");
    await delay(WARMUP_MS);

    const authHeaders = {
      Authorization: `Bearer ${API_KEY}`,
      "Content-Type": "application/json",
    };

    const readBehaviors: ClientBehavior[] = [
      {
        name: "health",
        weight: 30,
        fn: async (agent) => {
          const res = await request(agent, "GET", "/health", {});
          if (res.statusCode !== 200) throw new Error("bad health");
        },
      },
      {
        name: "read popular page",
        weight: 30,
        fn: async (agent) => {
          const res = await request(
            agent,
            "GET",
            "/v1/pages/wiki/overview.md",
            {},
          );
          if (res.statusCode !== 200) throw new Error("bad read");
        },
      },
      {
        name: "read random entity",
        weight: 25,
        fn: async (agent) => {
          const res = await request(
            agent,
            "GET",
            `/v1/pages/wiki/entities/entity-${randomInt(1, 100)}.md`,
            {},
          );
          if (res.statusCode !== 200) throw new Error("bad read");
        },
      },
      {
        name: "list pages",
        weight: 10,
        fn: async (agent) => {
          const res = await request(
            agent,
            "GET",
            "/v1/pages?folder=wiki/entities&limit=50",
            {},
          );
          if (res.statusCode !== 200) throw new Error("bad list");
        },
      },
      {
        name: "search",
        weight: 5,
        fn: async (agent) => {
          const field = pick(["title", "body"]);
          const q = pick(["Entity", "LLM", "Overview", "summary"]);
          const res = await request(
            agent,
            "GET",
            `/v1/search?q=${encodeURIComponent(q)}&in=${field}&limit=20`,
            {},
          );
          if (res.statusCode !== 200) throw new Error("bad search");
        },
      },
    ];

    const writeBehaviors: ClientBehavior[] = [
      {
        name: "update popular page",
        weight: 40,
        fn: async (agent) => {
          const res = await request(
            agent,
            "PUT",
            "/v1/pages/wiki/overview.md",
            authHeaders,
            JSON.stringify({
              content: `# Overview\n\nUpdated at ${Date.now()}.`,
              frontmatter: { category: "meta", tags: ["overview"] },
            }),
          );
          if (res.statusCode !== 200) throw new Error("bad update");
        },
      },
      {
        name: "create unique note",
        weight: 40,
        fn: async (agent) => {
          const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
          const res = await request(
            agent,
            "PUT",
            `/v1/pages/wiki/notes/note-${id}.md`,
            authHeaders,
            JSON.stringify({
              content: `# Note ${id}\n\nDaily note linking to [[Overview]] and [[entity-${randomInt(1, 100)}]].`,
              frontmatter: { category: "notes", tags: ["daily"] },
            }),
          );
          if (res.statusCode !== 200) throw new Error("bad create");
        },
      },
      {
        name: "append log",
        weight: 10,
        fn: async (agent) => {
          const res = await request(
            agent,
            "POST",
            "/v1/log/append",
            authHeaders,
            JSON.stringify({
              message: `activity ${Math.random().toString(36).slice(2)}`,
            }),
          );
          if (res.statusCode !== 201) throw new Error("bad log");
        },
      },
      {
        name: "upload source",
        weight: 10,
        fn: async (agent) => {
          const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
          const res = await request(
            agent,
            "POST",
            `/v1/sources/raw/uploads/source-${id}.md`,
            {
              Authorization: `Bearer ${API_KEY}`,
              "Content-Type": "text/plain",
            },
            `# Source ${id}\n\nRealistic source content.`,
          );
          if (res.statusCode !== 201) throw new Error("bad source");
        },
      },
    ];

    const ingestBehaviors: ClientBehavior[] = [
      {
        name: "batch ingest",
        weight: 100,
        fn: async (agent) => {
          const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
          const res = await request(
            agent,
            "POST",
            "/v1/ingest",
            authHeaders,
            JSON.stringify({
              source: {
                title: `Article ${id}`,
                rel_path: `raw/article-${id}.md`,
                content: `# Article ${id}\n\nThis is a longer source document with multiple paragraphs of realistic text.`,
              },
              operations: [
                {
                  rel_path: `wiki/summaries/Article ${id}.md`,
                  content: `Summary of ${id} linking to [[entity-${randomInt(1, 100)}]].`,
                  frontmatter: { category: "summaries" },
                },
                {
                  rel_path: `wiki/entities/${id}.md`,
                  content: `Entity ${id} referencing [[Overview]].`,
                  frontmatter: { category: "entities" },
                },
              ],
              logEntry: `Article ${id}`,
            }),
          );
          if (res.statusCode !== 200) throw new Error("bad ingest");
        },
      },
    ];

    const results: ScenarioResult[] = [];

    results.push(
      await runScenario({
        name: "Read-heavy browsing | 100 clients | think 50ms",
        clients: 100,
        durationMs: 10_000,
        behaviors: readBehaviors,
        thinkTimeMs: 50,
      }),
    );

    results.push(
      await runScenario({
        name: "Mixed read/write | 50 clients | think 100ms",
        clients: 50,
        durationMs: 10_000,
        behaviors: [
          ...readBehaviors.map((b) => ({ ...b, weight: b.weight * 0.7 })),
          ...writeBehaviors.map((b) => ({ ...b, weight: b.weight * 0.3 })),
        ],
        thinkTimeMs: 100,
      }),
    );

    results.push(
      await runScenario({
        name: "Write-heavy editing | 25 clients | think 50ms",
        clients: 25,
        durationMs: 10_000,
        behaviors: writeBehaviors,
        thinkTimeMs: 50,
      }),
    );

    results.push(
      await runScenario({
        name: "Batch ingestion | 3 clients | think 200ms",
        clients: 3,
        durationMs: 10_000,
        behaviors: ingestBehaviors,
        thinkTimeMs: 200,
      }),
    );

    results.push(
      await runScenario({
        name: "Observer polling changes | 10 clients | think 500ms",
        clients: 10,
        durationMs: 10_000,
        behaviors: [
          {
            name: "get changes",
            weight: 80,
            fn: async (agent) => {
              const res = await request(
                agent,
                "GET",
                "/v1/changes?limit=50",
                {},
              );
              if (res.statusCode !== 200) throw new Error("bad changes");
            },
          },
          {
            name: "refresh index",
            weight: 20,
            fn: async (agent) => {
              const res = await request(
                agent,
                "POST",
                "/v1/index/refresh",
                authHeaders,
              );
              if (res.statusCode !== 200) throw new Error("bad refresh");
            },
          },
        ],
        thinkTimeMs: 500,
      }),
    );

    console.log("\n========== REALISTIC BENCHMARK RESULTS ==========");
    for (const r of results) {
      console.log(formatResult(r));
    }
    console.log("==================================================\n");

    console.log("System info:");
    console.log(`  OS:        ${os.type()} ${os.release()} ${os.arch()}`);
    console.log(`  CPUs:      ${os.cpus().length} x ${os.cpus()[0]?.model}`);
    console.log(
      `  Memory:    ${(os.totalmem() / 1024 / 1024 / 1024).toFixed(2)} GB`,
    );
    console.log(`  Bun:       ${Bun.version}`);
    console.log(`  Date:      ${new Date().toISOString()}`);
  } finally {
    server.kill("SIGTERM");
    await delay(500);
    try {
      server.kill("SIGKILL");
    } catch {
      // ignore
    }
    rmSync(wikiRoot, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
