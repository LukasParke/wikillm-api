/**
 * Pluggable text-embedding backends.
 *
 * Two providers:
 *  - api   : any OpenAI-compatible /embeddings endpoint (Cerebras, Ollama,
 *            OpenAI, LM Studio...) — requires network + API key optionally
 *  - onnx  : in-process ONNX inference via transformers.js/onnxruntime-node.
 *            Runs natively on CPU everywhere; on AMD Ryzen AI (Strix Halo)
 *            machines the underlying onnxruntime can target DirectML/NPU EPs
 *            where the platform provides them, so bulk ingestion offloads to
 *            the XDNA2 NPU without code changes here.
 */

export interface Embedder {
  /** model identifier reported in responses/logs */
  readonly model: string;
  readonly dims: number;
  embed(texts: string[]): Promise<number[][]>;
}

export class EmbedderError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "EmbedderError";
  }
}

export type EmbedderProviderKind = "none" | "api" | "onnx";

export interface EmbedderSelection {
  provider: EmbedderProviderKind;
  model: string;
  dims: number;
}

// ---------------------------------------------------------------------------
// API embedder (OpenAI-compatible)
// ---------------------------------------------------------------------------

export function createApiEmbedder(opts: {
  baseUrl: string;
  apiKey?: string;
  model: string;
  dims: number;
}): Embedder {
  const base = opts.baseUrl.replace(/\/$/, "");
  return {
    model: opts.model,
    dims: opts.dims,
    async embed(texts: string[]): Promise<number[][]> {
      if (texts.length === 0) return [];
      const response = await fetch(`${base}/embeddings`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(opts.apiKey ? { Authorization: `Bearer ${opts.apiKey}` } : {}),
        },
        body: JSON.stringify({ model: opts.model, input: texts }),
        signal: AbortSignal.timeout(60_000),
      });
      if (!response.ok) {
        throw new EmbedderError(
          `Embedding endpoint ${response.status}: ${await response.text().catch(() => "")}`,
        );
      }
      const json = (await response.json()) as {
        data?: Array<{ index?: number; embedding: number[] }>;
      };
      const data = json.data ?? [];
      if (data.length !== texts.length) {
        throw new EmbedderError(
          `Embedding count mismatch: ${data.length} for ${texts.length}`,
        );
      }
      if (data.every((d) => typeof d.index === "number")) {
        const sorted = [...data].sort(
          (a, b) => (a.index ?? 0) - (b.index ?? 0),
        );
        return sorted.map((d) => d.embedding);
      }
      return data.map((d) => d.embedding);
    },
  };
}

// ---------------------------------------------------------------------------
// ONNX embedder (in-process, transformers.js)
// ---------------------------------------------------------------------------

interface OnnxSessionLike {
  (
    texts: string[],
    opts: { pooling: string; normalize: boolean },
  ): Promise<{
    data: { length: number } | Float32Array;
    dims?: number[];
  }>;
}

/** Mean-pool + L2-normalize helper shared by tests. */
export function meanPoolNormalize(vectors: number[][]): number[][] {
  return vectors.map((vec) => {
    let norm = 0;
    for (const v of vec) norm += v * v;
    norm = Math.sqrt(norm) || 1;
    return vec.map((v) => v / norm);
  });
}

export function createOnnxEmbedder(opts: {
  model: string;
  dtype?: string;
  device?: string;
}): Embedder {
  let sessionPromise: Promise<OnnxSessionLike> | null = null;

  async function load(): Promise<OnnxSessionLike> {
    if (!sessionPromise) {
      sessionPromise = (async () => {
        // Optional native dependency: absent installs fail with a clear,
        // actionable message instead of breaking every boot.
        type TransformersModule = {
          env: { allowLocalModels: boolean };
          pipeline(
            type: string,
            model: string,
            options: Record<string, unknown>,
          ): Promise<unknown>;
        };
        let mod: TransformersModule;
        try {
          mod =
            (await import("@huggingface/transformers")) as TransformersModule;
        } catch {
          throw new EmbedderError(
            "ONNX embedder requested but @huggingface/transformers is not installed. Run: bun add @huggingface/transformers",
          );
        }
        mod.env.allowLocalModels = false;
        const extractor = await mod.pipeline("feature-extraction", opts.model, {
          dtype: opts.dtype ?? "q8",
          device: opts.device ?? "cpu",
        });
        return extractor as OnnxSessionLike;
      })();
      sessionPromise.catch(() => {
        sessionPromise = null;
      });
    }
    return sessionPromise;
  }

  let knownDims = 0;

  return {
    model: opts.model,
    get dims(): number {
      return knownDims;
    },
    async embed(texts: string[]): Promise<number[][]> {
      if (texts.length === 0) return [];
      const session = await load();
      const output = await session(texts, { pooling: "mean", normalize: true });
      const flat = Array.from(output.data as Float32Array);
      const total = flat.length;
      if (total === 0 || total % texts.length !== 0) {
        throw new EmbedderError("Unexpected ONNX output shape");
      }
      const dim = total / texts.length;
      knownDims = dim;
      const vectors: number[][] = [];
      for (let i = 0; i < texts.length; i += 1) {
        vectors.push(flat.slice(i * dim, (i + 1) * dim));
      }
      return meanPoolNormalize(vectors);
    },
  };
}

// ---------------------------------------------------------------------------
// Resolution: settings-driven selection
// ---------------------------------------------------------------------------

export interface EmbedderConfigSource {
  getProvider(): EmbedderProviderKind;
  getApiBaseUrl(): string;
  getApiKey(): string;
  getApiModel(): string;
  getOnnxModel(): string;
  getOnnxDtype(): string;
  getOnnxDevice(): string;
  getDimsFallback(): number;
}

export function resolveEmbedder(src: EmbedderConfigSource): Embedder | null {
  const provider = src.getProvider();
  if (provider === "none") return null;
  if (provider === "onnx") {
    return createOnnxEmbedder({
      model: src.getOnnxModel(),
      dtype: src.getOnnxDtype(),
      device: src.getOnnxDevice(),
    });
  }
  if (provider === "api") {
    const baseUrl = src.getApiBaseUrl();
    if (!baseUrl) return null;
    return createApiEmbedder({
      baseUrl,
      apiKey: src.getApiKey(),
      model: src.getApiModel(),
      dims: src.getDimsFallback(),
    });
  }
  return null;
}
