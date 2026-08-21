import { z } from "zod";
import type { Source } from "./types/index.js";

const boolish = (defaultValue: boolean) =>
  z
    .string()
    .optional()
    .transform((v) =>
      v === undefined || v === "" ? defaultValue : v.toLowerCase() !== "false",
    );

const configSchema = z.object({
  WIKI_ROOT: z.string().min(1, "WIKI_ROOT is required"),
  PORT: z.coerce.number().int().min(0).default(3000),
  HOST: z.string().default("0.0.0.0"),
  API_KEYS: z
    .string()
    .min(1, "API_KEYS is required")
    .transform((s) => parseApiKeys(s)),
  PUBLIC_READ: boolish(true),
  DB_PATH: z.string().optional(),
  LOG_LEVEL: z
    .enum(["trace", "debug", "info", "warn", "error"])
    .default("info"),

  // Index store backend
  DB_BACKEND: z.enum(["auto", "sqlite", "postgres"]).default("auto"),
  DATABASE_URL: z.string().optional(),

  // Bundle layout profile
  LAYOUT: z.enum(["auto", "okf", "wikillm"]).default("auto"),
  /** Enforce OKF conformance on API writes (type required when bundle declares okf_version) */
  OKF_STRICT: boolish(false),
  /** Source names treated as human actors for OKF attribution */
  HUMAN_ACTORS: z.string().optional(),

  // Embeddings / LLM (OpenAI-compatible endpoints; Cerebras, Ollama, LM Studio, OpenAI...)
  LLM_BASE_URL: z.string().optional(),
  LLM_API_KEY: z.string().optional(),
  LLM_MODEL: z.string().default("llama3.1"),
  LLM_EMBED_MODEL: z.string().optional(),
  EMBEDDING_DIMS: z.coerce.number().int().min(64).max(4096).default(1536),
  /** Distill chunks (question/summary extraction) when an LLM is configured */
  LLM_DISTILL: boolish(false),

  // Connectors
  CONNECTOR_POLL_SECONDS: z.coerce.number().int().min(5).default(300),

  // Governance
  /** Requests per minute per identity; 0 disables limiting */
  RATE_LIMIT_RPM: z.coerce.number().int().min(0).default(0),
});

export interface ApiKeyEntry {
  name: string;
  key: string;
  /** project names this key may access; ["*"] = all */
  projects: string[];
  role: "admin" | "write" | "read";
}

/**
 * API_KEYS grammar: `name:key[:scope[:role]]`
 * - scope: comma-separated project names or `*` (default `*`)
 * - role: admin | write | read (default write)
 */
export function parseApiKeys(raw: string): Map<string, ApiKeyEntry> {
  const map = new Map<string, ApiKeyEntry>();
  for (const part of raw.split(",")) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const segments = trimmed.split(":");
    if (segments.length < 2) {
      throw new Error(
        `Invalid API_KEYS entry: ${trimmed}. Expected format name:key[:scope[:role]]`,
      );
    }
    const [name, key, scope, role] = segments.map((seg) => seg.trim());
    if (!name || !key) {
      throw new Error(`Invalid API_KEYS entry: ${trimmed}`);
    }
    if ([...map.values()].some((e) => e.key === key)) {
      throw new Error(`Duplicate API key: ${key}`);
    }
    const projects =
      !scope || scope === "*"
        ? ["*"]
        : scope
            .split(",")
            .map((p) => p.trim())
            .filter(Boolean);
    if (projects.length === 0) projects.push("*");
    const resolvedRole: ApiKeyEntry["role"] =
      role === "admin" || role === "read" ? role : "write";
    map.set(key, { name, key, projects, role: resolvedRole });
  }
  if (map.size === 0) {
    throw new Error("API_KEYS must contain at least one key");
  }
  return map;
}

/** Back-compat helper: old code mapped key → source name string. */
export function keyToSourceMap(
  keys: Map<string, ApiKeyEntry>,
): Map<string, Source> {
  const out = new Map<string, Source>();
  for (const [key, entry] of keys) out.set(key, entry.name);
  return out;
}

export type Config = z.infer<typeof configSchema>;

export function loadConfig(): Config {
  const parsed = configSchema.safeParse(process.env);
  if (!parsed.success) {
    const issues = parsed.error.issues
      .map((i) => `${i.path.join(".")}: ${i.message}`)
      .join("\n");
    throw new Error(`Config validation failed:\n${issues}`);
  }
  return parsed.data;
}
