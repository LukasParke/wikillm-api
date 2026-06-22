import { z } from "zod";
import type { Source } from "./types/index.js";

const configSchema = z.object({
  WIKI_ROOT: z.string().min(1, "WIKI_ROOT is required"),
  PORT: z.coerce.number().int().min(0).default(3000),
  HOST: z.string().default("0.0.0.0"),
  API_KEYS: z
    .string()
    .min(1, "API_KEYS is required")
    .transform((s) => parseApiKeys(s)),
  PUBLIC_READ: z
    .string()
    .optional()
    .transform((v) => v?.toLowerCase() !== "false"),
  DB_PATH: z.string().optional(),
  LOG_LEVEL: z
    .enum(["trace", "debug", "info", "warn", "error"])
    .default("info"),
});

function parseApiKeys(raw: string): Map<string, Source> {
  const map = new Map<string, Source>();
  for (const part of raw.split(",")) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf(":");
    if (idx === -1) {
      throw new Error(
        `Invalid API_KEYS entry: ${trimmed}. Expected format name:key`,
      );
    }
    const name = trimmed.slice(0, idx).trim();
    const key = trimmed.slice(idx + 1).trim();
    if (!name || !key) {
      throw new Error(`Invalid API_KEYS entry: ${trimmed}`);
    }
    if (map.has(key)) {
      throw new Error(`Duplicate API key: ${key}`);
    }
    map.set(key, name);
  }
  if (map.size === 0) {
    throw new Error("API_KEYS must contain at least one key");
  }
  return map;
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
