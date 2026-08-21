import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type Mock,
} from "vitest";
import {
  createLlmProviderFromEnv,
  ProviderError,
  type LlmProvider,
} from "../../src/llm/provider.js";

type FetchMock = (input: string | URL, init?: RequestInit) => Promise<Response>;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const CHAT_BODY = {
  choices: [{ message: { role: "assistant", content: "hello there" } }],
};

const EMBED_BODY = {
  data: [
    { index: 1, embedding: [4, 5, 6] },
    { index: 0, embedding: [1, 2, 3] },
  ],
};

describe("createLlmProviderFromEnv", () => {
  const originalFetch = globalThis.fetch;
  let fetchMock: Mock<FetchMock>;

  beforeEach(() => {
    fetchMock = vi.fn<FetchMock>();
    globalThis.fetch = fetchMock as unknown as typeof fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  function providerWith(env: NodeJS.ProcessEnv): LlmProvider {
    const provider = createLlmProviderFromEnv({
      LLM_BASE_URL: "http://localhost:11434/v1",
      ...env,
    });
    if (provider === null) throw new Error("provider unexpectedly disabled");
    return provider;
  }

  it("returns null when base URL is missing or empty", () => {
    expect(createLlmProviderFromEnv({})).toBeNull();
    expect(createLlmProviderFromEnv({ LLM_BASE_URL: "" })).toBeNull();
    expect(createLlmProviderFromEnv({ LLM_BASE_URL: "   " })).toBeNull();
  });

  it("applies defaults for model and embed dims", () => {
    const provider = providerWith({});
    expect(provider.model).toBe("llama3.1");
    expect(provider.embedModel).toBeNull();
    expect(provider.embedDims).toBeNull();
  });

  it("parses EMBEDDING_DIMS and exposes embed model", () => {
    const provider = providerWith({
      LLM_EMBED_MODEL: "nomic-embed",
      EMBEDDING_DIMS: "768",
    });
    expect(provider.embedModel).toBe("nomic-embed");
    expect(provider.embedDims).toBe(768);
  });

  it("falls back to default dims on non-numeric EMBEDDING_DIMS", () => {
    const provider = providerWith({
      LLM_EMBED_MODEL: "nomic-embed",
      EMBEDDING_DIMS: "abc",
    });
    expect(provider.embedDims).toBe(1536);
  });

  it("strips trailing slash from base URL", async () => {
    const provider = createLlmProviderFromEnv({
      LLM_BASE_URL: "http://x.test/api/",
    });
    if (provider === null) throw new Error("provider unexpectedly disabled");
    fetchMock.mockResolvedValue(jsonResponse(200, CHAT_BODY));
    await provider.chat([{ role: "user", content: "hi" }]);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "http://x.test/api/chat/completions",
    );
  });

  it("chat returns message content on happy path", async () => {
    const provider = providerWith({});
    fetchMock.mockResolvedValue(jsonResponse(200, CHAT_BODY));
    const content = await provider.chat(
      [
        { role: "system", content: "be brief" },
        { role: "user", content: "hi" },
      ],
      { temperature: 0.2, maxTokens: 64 },
    );
    expect(content).toBe("hello there");

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://localhost:11434/v1/chat/completions");
    const body = JSON.parse(String(init.body)) as Record<string, unknown>;
    expect(body["model"]).toBe("llama3.1");
    expect(body["messages"]).toEqual([
      { role: "system", content: "be brief" },
      { role: "user", content: "hi" },
    ]);
    expect(body["temperature"]).toBe(0.2);
    expect(body["max_tokens"]).toBe(64);
  });

  it("sends bearer authorization when API key is set", async () => {
    const provider = providerWith({ LLM_API_KEY: "sk-secret" });
    fetchMock.mockResolvedValue(jsonResponse(200, CHAT_BODY));
    await provider.chat([{ role: "user", content: "hi" }]);

    const headers = new Headers(
      (fetchMock.mock.calls[0]?.[1] as RequestInit).headers,
    );
    expect(headers.get("authorization")).toBe("Bearer sk-secret");
  });

  it("omits authorization header without API key", async () => {
    const provider = providerWith({});
    fetchMock.mockResolvedValue(jsonResponse(200, CHAT_BODY));
    await provider.chat([{ role: "user", content: "hi" }]);

    const headers = new Headers(
      (fetchMock.mock.calls[0]?.[1] as RequestInit).headers,
    );
    expect(headers.get("authorization")).toBeNull();
  });

  it("retries on 500 then succeeds", async () => {
    const provider = providerWith({});
    fetchMock
      .mockResolvedValueOnce(jsonResponse(500, { error: "boom" }))
      .mockResolvedValueOnce(jsonResponse(500, { error: "boom again" }))
      .mockResolvedValueOnce(jsonResponse(200, CHAT_BODY));

    await expect(
      provider.chat([{ role: "user", content: "hi" }]),
    ).resolves.toBe("hello there");
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("retries on 429 and network errors before succeeding", async () => {
    const provider = providerWith({});
    fetchMock
      .mockResolvedValueOnce(jsonResponse(429, {}))
      .mockRejectedValueOnce(new TypeError("fetch failed"))
      .mockResolvedValueOnce(jsonResponse(200, CHAT_BODY));

    await expect(
      provider.chat([{ role: "user", content: "hi" }]),
    ).resolves.toBe("hello there");
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("exhausts retries on persistent 503 with ProviderError status", async () => {
    const provider = providerWith({});
    fetchMock.mockResolvedValue(jsonResponse(503, {}));
    const error = await provider
      .chat([{ role: "user", content: "hi" }])
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ProviderError);
    expect((error as ProviderError).status).toBe(503);
    expect(fetchMock).toHaveBeenCalledTimes(3); // initial + 2 retries
  });

  it("does not retry on 400", async () => {
    const provider = providerWith({});
    fetchMock.mockResolvedValue(jsonResponse(400, { error: "bad request" }));

    const error = await provider
      .chat([{ role: "user", content: "hi" }])
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ProviderError);
    expect((error as ProviderError).status).toBe(400);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("embed sorts by index when present", async () => {
    const provider = providerWith({ LLM_EMBED_MODEL: "nomic-embed" });
    fetchMock.mockResolvedValue(jsonResponse(200, EMBED_BODY));
    const vectors = await provider.embed(["a", "b"]);
    expect(vectors).toEqual([
      [1, 2, 3],
      [4, 5, 6],
    ]);

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://localhost:11434/v1/embeddings");
    const body = JSON.parse(String(init.body)) as Record<string, unknown>;
    expect(body["model"]).toBe("nomic-embed");
    expect(body["input"]).toEqual(["a", "b"]);
  });

  it("preserves response order when embeddings lack index", async () => {
    const provider = providerWith({ LLM_EMBED_MODEL: "nomic-embed" });
    fetchMock.mockResolvedValue(
      jsonResponse(200, { data: [{ embedding: [9] }, { embedding: [8] }] }),
    );
    await expect(provider.embed(["a", "b"])).resolves.toEqual([[9], [8]]);
  });

  it("rejects embed when no embedding model configured", async () => {
    const provider = providerWith({});
    await expect(provider.embed(["a"])).rejects.toThrowError(ProviderError);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("throws ProviderError on malformed chat response", async () => {
    const provider = providerWith({});
    fetchMock.mockResolvedValue(jsonResponse(200, { unexpected: true }));
    await expect(
      provider.chat([{ role: "user", content: "hi" }]),
    ).rejects.toThrowError(/Malformed chat completion/);
  });
});
