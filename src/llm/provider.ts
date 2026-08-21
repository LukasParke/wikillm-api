import { z } from "zod";

export interface ChatMessage {
  role: "system" | "user" | "assistant";
  content: string;
}

export interface ChatOptions {
  temperature?: number;
  maxTokens?: number;
}

export interface LlmProvider {
  readonly model: string;
  readonly embedModel: string | null;
  readonly embedDims: number | null;
  chat(messages: ChatMessage[], opts?: ChatOptions): Promise<string>;
  embed(texts: string[]): Promise<number[][]>;
}

export class ProviderError extends Error {
  constructor(
    message: string,
    public status?: number,
  ) {
    super(message);
    this.name = "ProviderError";
  }
}

const CHAT_TIMEOUT_MS = 30_000;
const EMBED_TIMEOUT_MS = 60_000;
const RETRY_DELAYS_MS = [250, 1000];

const ChatCompletionResponse = z.object({
  choices: z
    .array(
      z
        .object({ message: z.object({ content: z.string() }).passthrough() })
        .passthrough(),
    )
    .min(1),
});

const EmbeddingsResponse = z.object({
  data: z.array(
    z.object({
      index: z.number().int().optional(),
      embedding: z.array(z.number()),
    }),
  ),
});

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function postJson(
  url: string,
  apiKey: string | undefined,
  body: Record<string, unknown>,
  timeoutMs: number,
): Promise<unknown> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
  };
  if (apiKey !== undefined && apiKey !== "") {
    headers.authorization = `Bearer ${apiKey}`;
  }

  let lastError: ProviderError | null = null;
  for (let attempt = 0; attempt <= RETRY_DELAYS_MS.length; attempt++) {
    if (attempt > 0) {
      await delay(RETRY_DELAYS_MS[attempt - 1]);
    }
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(timeoutMs),
      });
    } catch (error) {
      // Network-level failure (TypeError from fetch, including timeout aborts).
      lastError = new ProviderError(
        `Request failed: ${error instanceof Error ? error.message : String(error)}`,
      );
      continue;
    }

    if (response.ok) {
      try {
        return (await response.json()) as unknown;
      } catch {
        throw new ProviderError(
          `Invalid JSON response from ${url}`,
          response.status,
        );
      }
    }

    lastError = new ProviderError(
      `Request failed with status ${response.status}`,
      response.status,
    );
    const retryable =
      response.status === 429 ||
      (response.status >= 500 && response.status < 600);
    if (!retryable) {
      throw lastError;
    }
  }
  throw lastError ?? new ProviderError("Request failed");
}

function parseEnvInt(value: string | undefined, fallback: number): number {
  if (value === undefined || value.trim() === "") return fallback;
  const parsed = Number.parseInt(value, 10);
  return Number.isNaN(parsed) ? fallback : parsed;
}

function nonEmpty(value: string | undefined): string | null {
  return value !== undefined && value.trim() !== "" ? value : null;
}

export function createLlmProviderFromEnv(
  env: NodeJS.ProcessEnv,
): LlmProvider | null {
  const rawBase = env.LLM_BASE_URL;
  if (rawBase === undefined || rawBase.trim() === "") {
    return null;
  }
  const base = rawBase.replace(/\/+$/, "");
  const apiKey = env.LLM_API_KEY;
  const model = nonEmpty(env.LLM_MODEL) ?? "llama3.1";
  const embedModel = nonEmpty(env.LLM_EMBED_MODEL);
  const embedDims =
    embedModel === null ? null : parseEnvInt(env.EMBEDDING_DIMS, 1536);

  return {
    model,
    embedModel,
    embedDims,
    async chat(messages: ChatMessage[], opts?: ChatOptions): Promise<string> {
      const body: Record<string, unknown> = { model, messages };
      if (opts?.temperature !== undefined) body.temperature = opts.temperature;
      if (opts?.maxTokens !== undefined) body.max_tokens = opts.maxTokens;
      const payload = await postJson(
        `${base}/chat/completions`,
        apiKey,
        body,
        CHAT_TIMEOUT_MS,
      );
      const parsed = ChatCompletionResponse.safeParse(payload);
      if (!parsed.success) {
        throw new ProviderError("Malformed chat completion response");
      }
      return parsed.data.choices[0].message.content;
    },
    async embed(texts: string[]): Promise<number[][]> {
      if (embedModel === null) {
        throw new ProviderError("No embedding model configured");
      }
      const payload = await postJson(
        `${base}/embeddings`,
        apiKey,
        { model: embedModel, input: texts },
        EMBED_TIMEOUT_MS,
      );
      const parsed = EmbeddingsResponse.safeParse(payload);
      if (!parsed.success) {
        throw new ProviderError("Malformed embeddings response");
      }
      const entries = parsed.data.data;
      if (entries.some((entry) => entry.index === undefined)) {
        return entries.map((entry) => entry.embedding);
      }
      return [...entries]
        .sort((a, b) => (a.index as number) - (b.index as number))
        .map((entry) => entry.embedding);
    },
  };
}
