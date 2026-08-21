import { describe, it, expect, beforeEach, afterEach } from "vitest";
import path from "node:path";
import os from "node:os";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import type { Hono } from "hono";
import { createApp } from "../../src/app.js";
import { loadConfig } from "../../src/config.js";
import { createStore } from "../../src/store/index.js";
import type { Store } from "../../src/store/types.js";
import { createServices } from "../../src/services/container.js";
import { createBroadcaster } from "../../src/services/broadcaster.js";

interface TestEnv {
  root: string;
  cleanup: () => void;
}

function makeEnv(over: Record<string, string | undefined>): TestEnv {
  const root = path.join(
    os.tmpdir(),
    `wikillm-ac-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  );
  mkdirSync(root, { recursive: true });
  const env: Record<string, string | undefined> = {
    WIKI_ROOT: root,
    PORT: "0",
    HOST: "127.0.0.1",
    PUBLIC_READ: "true",
    DB_PATH: path.join(root, "test.db"),
    LOG_LEVEL: "error",
    ...over,
  };
  for (const [k, v] of Object.entries(env)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  return {
    root,
    cleanup: () => {
      for (const k of Object.keys(env)) delete process.env[k];
      rmSync(root, { recursive: true, force: true });
    },
  };
}

async function buildApp(): Promise<{
  hono: Hono<{ Variables: never }>;
  store: Store;
  env: TestEnv;
}> {
  const config = loadConfig();
  const store = await createStore(config);
  const services = await createServices(config, store);
  const hono = createApp({
    config,
    store,
    services,
    broadcaster: createBroadcaster(),
  }) as unknown as Hono<{ Variables: never }>;
  return {
    hono,
    store,
    env: { root: config.WIKI_ROOT, cleanup: () => undefined },
  };
}

describe("agent control: bootstrap, settings, keys", () => {
  let cleanupFns: Array<() => void> = [];

  afterEach(() => {
    for (const fn of cleanupFns) fn();
    cleanupFns = [];
  });

  function track(env: TestEnv): void {
    cleanupFns.push(env.cleanup);
  }

  it("bootstraps an admin key when no API_KEYS exist and self-describes", async () => {
    const env = makeEnv({
      API_KEYS: undefined,
      BOOTSTRAP_ADMIN_KEY: "boot-admin-123",
    });
    track(env);
    const { hono, store } = await buildApp();

    const self = await hono.request("/v1");
    expect(self.status).toBe(200);
    const selfJson = (await self.json()) as { endpoints: string[] };
    expect(selfJson.endpoints.length).toBeGreaterThan(10);

    // bootstrap key authenticates as admin
    const auth = { Authorization: "Bearer boot-admin-123" };
    const settings = await hono.request("/v1/settings", { headers: auth });
    expect(settings.status).toBe(200);
    const list = (await settings.json()) as {
      settings: Array<{ key: string }>;
    };
    expect(list.settings.some((s) => s.key === "llm_base_url")).toBe(true);

    // anonymous writes are still rejected
    const anonPut = await hono.request("/v1/pages/wiki/x.md", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content: "nope" }),
    });
    expect(anonPut.status).toBe(401);
    await store.close();
  });

  it("creates keys via API; new key works immediately; deletion revokes", async () => {
    const env = makeEnv({ API_KEYS: "admin:adminkey:*:admin" });
    track(env);
    const { hono, store } = await buildApp();
    const auth = {
      Authorization: "Bearer adminkey",
      "Content-Type": "application/json",
    };

    const created = await hono.request("/v1/keys", {
      method: "POST",
      headers: auth,
      body: JSON.stringify({ name: "agent-x", role: "write", scope: ["*"] }),
    });
    expect(created.status).toBe(201);
    const createdJson = (await created.json()) as {
      secret: string;
      name: string;
    };
    expect(createdJson.secret.startsWith("wk_")).toBe(true);

    // plaintext works but is not recoverable from listings
    const write = await hono.request("/v1/pages/wiki/made-by-agent.md", {
      method: "PUT",
      headers: {
        Authorization: `Bearer ${createdJson.secret}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ content: "agent wrote this" }),
    });
    expect(write.status).toBe(200);

    const listed = await hono.request("/v1/keys", { headers: auth });
    const listedJson = (await listed.json()) as {
      keys: Array<Record<string, unknown>>;
    };
    expect(listedJson.keys[0]).not.toHaveProperty("secret");
    expect(listedJson.keys[0]).not.toHaveProperty("key_hash");

    const del = await hono.request(`/v1/keys/${createdJson.name}`, {
      method: "DELETE",
      headers: auth,
    });
    expect(del.status).toBe(200);
    const revoked = await hono.request("/v1/pages/wiki/again.md", {
      method: "PUT",
      headers: {
        Authorization: `Bearer ${createdJson.secret}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ content: "revoked" }),
    });
    expect(revoked.status).toBe(401);
    await store.close();
  });

  it("hot-applies public_read without restart", async () => {
    const env = makeEnv({
      API_KEYS: "admin:adminkey:*:admin",
      PUBLIC_READ: "true",
    });
    track(env);
    const { hono, store } = await buildApp();
    const auth = {
      Authorization: "Bearer adminkey",
      "Content-Type": "application/json",
    };

    const anonBefore = await hono.request("/v1/pages");
    expect(anonBefore.status).toBe(200);

    const put = await hono.request("/v1/settings/public_read", {
      method: "PUT",
      headers: auth,
      body: JSON.stringify({ value: false }),
    });
    expect(put.status).toBe(200);

    const anonAfter = await hono.request("/v1/pages");
    expect(anonAfter.status).toBe(401);
    const adminAfter = await hono.request("/v1/pages", { headers: auth });
    expect(adminAfter.status).toBe(200);

    await hono.request("/v1/settings/public_read", {
      method: "DELETE",
      headers: auth,
    });
    const anonRestored = await hono.request("/v1/pages");
    expect(anonRestored.status).toBe(200);
    await store.close();
  });

  it("enforces okf_strict only when the bundle declares okf_version", async () => {
    const env = makeEnv({
      API_KEYS: "admin:adminkey:*:admin",
      OKF_STRICT: "false",
    });
    track(env);
    writeFileSync(
      path.join(env.root, "index.md"),
      'okf_version: "0.2"\n\n# Index\n',
    );
    const { hono, store } = await buildApp();
    const auth = {
      Authorization: "Bearer adminkey",
      "Content-Type": "application/json",
    };

    await hono.request("/v1/settings/okf_strict", {
      method: "PUT",
      headers: auth,
      body: JSON.stringify({ value: true }),
    });

    const rejected = await hono.request("/v1/pages/wiki/no-type.md", {
      method: "PUT",
      headers: auth,
      body: JSON.stringify({ content: "no type here" }),
    });
    expect(rejected.status).toBe(422);
    const rejectedJson = (await rejected.json()) as { error: string };
    expect(rejectedJson.error).toBe("okf_strict");

    const accepted = await hono.request("/v1/pages/wiki/typed.md", {
      method: "PUT",
      headers: auth,
      body: JSON.stringify({
        content: "typed concept",
        frontmatter: { type: "Note" },
      }),
    });
    expect(accepted.status).toBe(200);
    await store.close();
  });
});
