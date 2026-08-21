import type { Config } from "../config.js";
import { createStore } from "../store/index.js";
import type { Store } from "../store/types.js";
import { createLlmProviderFromEnv, type LlmProvider } from "../llm/provider.js";
import { IndexPipeline } from "./pipeline.js";
import { SearchService } from "./searchService.js";
import { QueryService } from "./queryService.js";
import { GraphService } from "./graphService.js";
import { OkfService } from "./okfService.js";
import { createProjectService } from "./projectService.js";
import { ConnectorManager } from "../connectors/manager.js";

/**
 * Process-wide service container. Built once at boot; routes construct the
 * cheap per-request service facades (pages/sources/index/log/ingest) from it.
 */
export interface Services {
  config: Config;
  store: Store;
  llm: LlmProvider | null;
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
  const llm = createLlmProviderFromEnv(process.env);
  const pipeline = new IndexPipeline(
    config.WIKI_ROOT,
    store,
    llm,
    config.LLM_DISTILL,
    (msg, err) => {
      if (err) console.error(msg, err);
      else if (config.LOG_LEVEL === "debug") console.log(msg);
    },
  );
  const search = new SearchService(store, llm);
  const query = new QueryService(store, llm, search);
  const graph = new GraphService(store);
  const okf = new OkfService(config);
  const projects = createProjectService(store);
  const connectors = new ConnectorManager(store, pipeline);

  return {
    config,
    store,
    llm,
    pipeline,
    search,
    query,
    graph,
    okf,
    projects,
    connectors,
  };
}

export async function startConnectors(services: Services): Promise<void> {
  services.connectors.start(services.config.CONNECTOR_POLL_SECONDS);
}

export function stopConnectors(services: Services): void {
  services.connectors.stop();
}
