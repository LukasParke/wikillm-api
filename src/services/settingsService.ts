import { z } from "zod";
import type { Config } from "../config.js";
import type { Store } from "../store/types.js";

export type SettingType = "bool" | "int" | "string" | "secret" | "enum";

export interface SettingMeta {
  key: string;
  type: SettingType;
  mutable: boolean;
  /** changing this value requires a full re-embed of the corpus */
  requiresReindex?: boolean;
  description: string;
  validate: z.ZodTypeAny;
}

interface ResolvedSetting {
  value: unknown;
  overridden: boolean;
}

const SETTING_META: SettingMeta[] = [
  {
    key: "public_read",
    type: "bool",
    mutable: true,
    description: "Allow unauthenticated read access",
    validate: z.boolean(),
  },
  {
    key: "rate_limit_rpm",
    type: "int",
    mutable: true,
    description: "Requests per minute per identity (0 disables)",
    validate: z.number().int().min(0).max(1_000_000),
  },
  {
    key: "connector_poll_seconds",
    type: "int",
    mutable: true,
    description: "Connector polling interval in seconds",
    validate: z.number().int().min(5).max(86_400),
  },
  {
    key: "llm_base_url",
    type: "string",
    mutable: true,
    description:
      "OpenAI-compatible base URL for chat/embeddings (empty = LLM features off)",
    validate: z.union([z.string(), z.null()]),
  },
  {
    key: "llm_api_key",
    type: "secret",
    mutable: true,
    description:
      "Bearer token for the LLM endpoint (write-only; never returned)",
    validate: z.union([z.string(), z.null()]),
  },
  {
    key: "llm_model",
    type: "string",
    mutable: true,
    description: "Chat model used for rerank/distill/query synthesis",
    validate: z.string().min(1),
  },
  {
    key: "llm_embed_model",
    type: "string",
    mutable: true,
    description: "Embedding model (empty = FTS-only retrieval)",
    validate: z.union([z.string(), z.null()]),
  },
  {
    key: "embedding_dims",
    type: "int",
    mutable: true,
    requiresReindex: true,
    description:
      "Vector dimensions; changing wipes embeddings and requires reindex",
    validate: z.number().int().min(64).max(4096),
  },
  {
    key: "llm_distill",
    type: "bool",
    mutable: true,
    description:
      "Extract question/summary per chunk with the LLM before embedding",
    validate: z.boolean(),
  },
  {
    key: "okf_strict",
    type: "bool",
    mutable: true,
    description:
      "Reject writes without a type when the bundle declares okf_version",
    validate: z.boolean(),
  },
  {
    key: "human_actors",
    type: "string",
    mutable: true,
    description: "Comma-separated API-key names attributed as human:<name>",
    validate: z.string(),
  },
  {
    key: "layout",
    type: "enum",
    mutable: true,
    description: "Bundle layout profile",
    validate: z.enum(["auto", "okf", "wikillm"]),
  },
];

/** Immutable settings are reported for discoverability but reject PUT. */
const IMMUTABLE_META: Array<{
  key: string;
  envKey: keyof Config & string;
  secret?: boolean;
  description: string;
}> = [
  {
    key: "wiki_root",
    envKey: "WIKI_ROOT",
    description: "Wiki folder root (set at deployment)",
  },
  { key: "port", envKey: "PORT", description: "HTTP port (set at deployment)" },
  {
    key: "host",
    envKey: "HOST",
    description: "Bind address (set at deployment)",
  },
  {
    key: "db_backend",
    envKey: "DB_BACKEND",
    description: "Index backend: sqlite | postgres (set at deployment)",
  },
  {
    key: "database_url",
    envKey: "DATABASE_URL",
    secret: true,
    description: "Postgres connection string (set at deployment)",
  },
];

const SECRET_KEYS = new Set(["llm_api_key"]);

export class OkfStrictError extends Error {
  constructor(
    message = "Write rejected by OKF strict mode: missing frontmatter 'type'",
  ) {
    super(message);
    this.name = "OkfStrictError";
  }
}

export class UnknownSettingError extends Error {
  constructor(key: string) {
    super(`Unknown setting: ${key}`);
    this.name = "UnknownSettingError";
  }
}

export class ImmutableSettingError extends Error {
  constructor(key: string) {
    super(`Setting is immutable (deployment-level): ${key}`);
    this.name = "ImmutableSettingError";
  }
}

/**
 * Runtime configuration: every hot-appliable knob lives here. Values resolve
 * DB override -> env -> schema default, cached briefly so per-request reads
 * stay cheap; writes invalidate immediately and fire change hooks.
 */
export class SettingsService {
  private cache: { at: number; values: Map<string, ResolvedSetting> } | null =
    null;
  private hooks: Array<(key: string, value: unknown) => void> = [];

  constructor(
    private readonly store: Store,
    private readonly config: Config,
    private readonly ttlMs = 1000,
  ) {}

  onChange(hook: (key: string, value: unknown) => void): void {
    this.hooks.push(hook);
  }

  meta(): SettingMeta[] {
    return SETTING_META;
  }

  private envValue(key: string): unknown {
    switch (key) {
      case "public_read":
        return this.config.PUBLIC_READ;
      case "rate_limit_rpm":
        return this.config.RATE_LIMIT_RPM;
      case "connector_poll_seconds":
        return this.config.CONNECTOR_POLL_SECONDS;
      case "llm_base_url":
        return this.config.LLM_BASE_URL ?? "";
      case "llm_api_key":
        return this.config.LLM_API_KEY ?? "";
      case "llm_model":
        return this.config.LLM_MODEL;
      case "llm_embed_model":
        return this.config.LLM_EMBED_MODEL ?? "";
      case "embedding_dims":
        return this.config.EMBEDDING_DIMS;
      case "llm_distill":
        return this.config.LLM_DISTILL;
      case "okf_strict":
        return this.config.OKF_STRICT;
      case "human_actors":
        return this.config.HUMAN_ACTORS ?? "";
      case "layout":
        return this.config.LAYOUT;
      default:
        return undefined;
    }
  }

  private async resolved(): Promise<Map<string, ResolvedSetting>> {
    if (this.cache && Date.now() - this.cache.at < this.ttlMs) {
      return this.cache.values;
    }
    const overrides = await this.store.getSettings();
    const values = new Map<string, ResolvedSetting>();
    for (const meta of SETTING_META) {
      const has = Object.prototype.hasOwnProperty.call(overrides, meta.key);
      values.set(meta.key, {
        value: has ? overrides[meta.key] : this.envValue(meta.key),
        overridden: has,
      });
    }
    this.cache = { at: Date.now(), values };
    return values;
  }

  /** Synchronous view of the last resolved cache (call after warm()). */
  cacheSnapshot(): Map<string, ResolvedSetting> {
    return this.cache?.values ?? new Map();
  }

  async warm(): Promise<void> {
    await this.resolved();
  }

  async get<T = unknown>(key: string): Promise<T> {
    return (await this.resolved()).get(key)?.value as T;
  }

  async isOverridden(key: string): Promise<boolean> {
    return (await this.resolved()).get(key)?.overridden ?? false;
  }

  /** Masked view for API responses: secrets render as set/unset only. */
  async describe(): Promise<Array<Record<string, unknown>>> {
    const resolved = await this.resolved();
    const out: Array<Record<string, unknown>> = [];
    for (const meta of SETTING_META) {
      const entry = resolved.get(meta.key)!;
      const value = SECRET_KEYS.has(meta.key)
        ? typeof entry.value === "string" && entry.value.length > 0
          ? "<set>"
          : "<unset>"
        : entry.value;
      out.push({
        key: meta.key,
        type: meta.type,
        value,
        default_value: SECRET_KEYS.has(meta.key)
          ? undefined
          : this.envValue(meta.key),
        overridden: entry.overridden,
        mutable: true,
        requires_reindex: meta.requiresReindex === true,
        description: meta.description,
      });
    }
    for (const imm of IMMUTABLE_META) {
      const raw = this.config[imm.envKey];
      out.push({
        key: imm.key,
        type: imm.secret ? "secret" : "string",
        value: imm.secret
          ? typeof raw === "string" && raw.length > 0
            ? "<set>"
            : "<unset>"
          : raw,
        default_value: undefined,
        overridden: false,
        mutable: false,
        description: imm.description,
      });
    }
    return out;
  }

  async set(
    key: string,
    value: unknown,
    updatedBy: string,
  ): Promise<{ reindexRequired: boolean }> {
    const meta = SETTING_META.find((m) => m.key === key);
    if (!meta) {
      if (IMMUTABLE_META.some((m) => m.key === key))
        throw new ImmutableSettingError(key);
      throw new UnknownSettingError(key);
    }
    const parsed = meta.validate.safeParse(value);
    if (!parsed.success) {
      const issue = parsed.error.issues[0];
      throw new Error(
        `Invalid value for ${key}: ${issue?.message ?? "validation failed"}`,
      );
    }
    const previous = await this.get(key);
    await this.store.setSetting(key, parsed.data, updatedBy);
    this.cache = null;
    const reindexRequired =
      meta.requiresReindex === true &&
      JSON.stringify(previous) !== JSON.stringify(parsed.data);
    if (reindexRequired) {
      await this.store.resetEmbeddings();
    }
    for (const hook of this.hooks) hook(key, parsed.data);
    return { reindexRequired };
  }

  async reset(key: string, updatedBy: string): Promise<boolean> {
    const meta = SETTING_META.find((m) => m.key === key);
    if (!meta) {
      if (IMMUTABLE_META.some((m) => m.key === key))
        throw new ImmutableSettingError(key);
      throw new UnknownSettingError(key);
    }
    const removed = await this.store.deleteSetting(key);
    if (removed) {
      this.cache = null;
      for (const hook of this.hooks) hook(key, this.envValue(key));
      void updatedBy;
    }
    return removed;
  }
}
