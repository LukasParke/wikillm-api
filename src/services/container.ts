import type { Config } from "../config.js";
import { createStore } from "../store/index.js";
import type { Store } from "../store/types.js";
import { createLlmProviderFromEnv, type LlmProvider } from "../llm/provider.js";
import {
  resolveEmbedder,
  type Embedder,
  type EmbedderProviderKind,
} from "../llm/embedder.js";
import { IndexPipeline } from "./pipeline.js";
import { SearchService } from "./searchService.js";
import { QueryService } from "./queryService.js";
import { GraphService } from "./graphService.js";
import { OkfService } from "./okfService.js";
import { createProjectService } from "./projectService.js";
import { ConnectorManager } from "../connectors/manager.js";
import { SettingsService } from "./settingsService.js";
import { KeyRegistry } from "./keyRegistry.js";

/**
 * Shared mutable LLM handle: settings changes swap `current` in place so
 * search/query/pipeline pick up new providers without restart.
 */
export class LlmHolder {
  constructor(public current: LlmProvider | null) {}
}

/** Live settings getters consumed at request time (never stale). */
export interface RuntimeFlags {
  llm: () => LlmProvider | null;
  embedder: () => Embedder | null;
  distillEnabled: () => Promise<boolean>;
}

/**
 * Process-wide service container. Built once at boot; routes construct the
 * cheap per-request service facades (pages/sources/index/log/ingest) from it.
 */
export interface Services {
  config: Config;
  store: Store;
  settings: SettingsService;
  keys: KeyRegistry;
  llmHolder: LlmHolder;
  pipeline: IndexPipeline;
  search: SearchService;
  query: QueryService;
  graph: GraphService;
  okf: OkfService;
  projects: ReturnType<typeof createProjectService>;
  connectors: ConnectorManager;
}

export async function createServices(
  config: Config,
  injectedStore?: Store,
): Promise<Services> {
  const store = injectedStore ?? (await createStore(config));
  const settings = new SettingsService(store, config);
  const keys = new KeyRegistry(store, envKeyEntries(config));

  // Bootstrap: an instance with no configured keys mints one admin key and
  // prints it once, so deployment -> configuration happens entirely via API.
  if (!keys.hasEnvKeys() && (await store.countApiKeys()) === 0) {
    const secret = process.env.BOOTSTRAP_ADMIN_KEY || undefined;
    const created = await keys.createKey({
      name: "bootstrap-admin",
      secret,
      role: "admin",
      scope: ["*"],
      createdBy: "bootstrap",
    });
    console.log(
      `\n=== WikiLLM bootstrap admin key (shown once; store it now) ===\n  ${created.secret}\n=== Configure the instance via PUT /v1/settings, POST /v1/keys ===\n`,
    );
  }

  await settings.warm();
  const llmHolder = new LlmHolder(buildLlm(config, settings));
  const embedderHolder: { current: Embedder | null } = {
    current: buildEmbedder(settings),
  };
  const flags: RuntimeFlags = {
    llm: () => llmHolder.current,
    embedder: () => embedderHolder.current,
    distillEnabled: async () =>
      (await settings.get<boolean>("llm_distill")) === true,
  };
  const pipeline = new IndexPipeline(
    config.WIKI_ROOT,
    store,
    flags,
    (msg, err) => {
      if (err) console.error(msg, err);
      else if (config.LOG_LEVEL === "debug") console.log(msg);
    },
  );
  const search = new SearchService(store, flags);
  const query = new QueryService(store, flags, search);
  const graph = new GraphService(store);
  const okf = new OkfService(config, settings);
  const projects = createProjectService(store);
  const connectors = new ConnectorManager(store, pipeline);

  settings.onChange((key, value) => {
    if (
      [
        "embedding_provider",
        "onnx_model",
        "onnx_dtype",
        "onnx_device",
      ].includes(key)
    ) {
      embedderHolder.current = buildEmbedder(settings);
      console.log(`Embedder rebuilt after settings change: ${key}`);
    }
    if (
      key === "llm_base_url" ||
      key === "llm_api_key" ||
      key === "llm_model" ||
      key === "llm_embed_model"
    ) {
      llmHolder.current = buildLlm(config, settings);
      console.log(`LLM provider rebuilt after settings change: ${key}`);
    }
    if (key === "connector_poll_seconds") {
      connectors.stop();
      connectors.start(Number(value) || config.CONNECTOR_POLL_SECONDS);
    }
  });

  return {
    config,
    store,
    settings,
    keys,
    llmHolder,
    pipeline,
    search,
    query,
    graph,
    okf,
    projects,
    connectors,
  };
}

/** Build the embedder from live settings (provider selection). */
function buildEmbedder(settings: SettingsService): Embedder | null {
  const cache = settings.cacheSnapshot();
  const get = (k: string): unknown => cache.get(k)?.value;
  const providerRaw =
    (get("embedding_provider") as string | undefined) ?? "auto";
  let provider = providerRaw as EmbedderProviderKind | "auto";
  if (provider === "auto") {
    const baseUrl = (get("llm_base_url") as string | undefined) ?? "";
    const embedModel = (get("llm_embed_model") as string | undefined) ?? "";
    provider = baseUrl && embedModel ? "api" : "none";
  }
  return resolveEmbedder({
    getProvider: () => provider,
    getApiBaseUrl: () => (get("llm_base_url") as string | undefined) ?? "",
    getApiKey: () => (get("llm_api_key") as string | undefined) ?? "",
    getApiModel: () => (get("llm_embed_model") as string | undefined) ?? "",
    getOnnxModel: () =>
      (get("onnx_model") as string | undefined) ?? "Xenova/bge-small-en-v1.5",
    getOnnxDtype: () => (get("onnx_dtype") as string | undefined) ?? "q8",
    getOnnxDevice: () => (get("onnx_device") as string | undefined) ?? "cpu",
    getDimsFallback: () => Number(get("embedding_dims") ?? 1536),
  });
}

/** Build a provider from live settings, falling back to env config. */
function buildLlm(
  config: Config,
  settings: SettingsService,
): LlmProvider | null {
  const cache = settings.cacheSnapshot();
  const baseUrl =
    (cache.get("llm_base_url")?.value as string | undefined) ?? "";
  const effectiveEnv: NodeJS.ProcessEnv = {
    ...process.env,
    LLM_BASE_URL:
      baseUrl.length > 0 ? baseUrl : (process.env.LLM_BASE_URL ?? ""),
    LLM_API_KEY:
      (cache.get("llm_api_key")?.value as string | undefined) ||
      process.env.LLM_API_KEY ||
      "",
    LLM_MODEL:
      (cache.get("llm_model")?.value as string | undefined) || config.LLM_MODEL,
    LLM_EMBED_MODEL:
      (cache.get("llm_embed_model")?.value as string | undefined) ||
      process.env.LLM_EMBED_MODEL ||
      "",
    EMBEDDING_DIMS: String(
      cache.get("embedding_dims")?.value ?? config.EMBEDDING_DIMS,
    ),
  };
  return createLlmProviderFromEnv(effectiveEnv);
}

function envKeyEntries(config: Config): Map<
  string,
  {
    name: string;
    secret: string;
    role: "admin" | "write" | "read";
    scope: string[];
  }
> {
  const out = new Map<
    string,
    {
      name: string;
      secret: string;
      role: "admin" | "write" | "read";
      scope: string[];
    }
  >();
  for (const entry of config.API_KEYS.values()) {
    out.set(entry.key, {
      name: entry.name,
      secret: entry.key,
      role: entry.role,
      scope: entry.projects,
    });
  }
  return out;
}

export async function startConnectors(services: Services): Promise<void> {
  const pollSeconds = (await services.settings.get(
    "connector_poll_seconds",
  )) as number;
  services.connectors.start(
    Number(pollSeconds) || services.config.CONNECTOR_POLL_SECONDS,
  );
}

export function stopConnectors(services: Services): void {
  services.connectors.stop();
}
