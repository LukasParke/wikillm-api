import type { Store, ConnectorConfig } from "../store/types.js";
import type { IndexPipeline } from "../services/pipeline.js";
import type { ConnectorContext, ConnectorImpl } from "./types.js";
import { gitConnector } from "./git.js";
import { webConnector } from "./web.js";
import { githubConnector } from "./github.js";
import { ulid } from "ulidx";

/**
 * Registry + scheduler for source connectors. Connector documents land in
 * the shared index under `<connectorId>/<path>` with origin=<connectorId>.
 */
export class ConnectorManager {
  private impls = new Map<string, ConnectorImpl>();
  private timer: ReturnType<typeof setInterval> | undefined;
  private running = false;

  constructor(
    private readonly store: Store,
    private readonly pipeline: IndexPipeline,
    private readonly log: (msg: string, err?: unknown) => void = console.log,
  ) {
    this.register(gitConnector);
    this.register(webConnector);
    this.register(githubConnector);
  }

  register(impl: ConnectorImpl): void {
    this.impls.set(impl.kind, impl);
  }

  listKinds(): string[] {
    return [...this.impls.keys()];
  }

  async listConnectors(): Promise<ConnectorConfig[]> {
    return this.store.listConnectors();
  }

  async getConnector(id: string): Promise<ConnectorConfig | null> {
    return this.store.getConnector(id);
  }

  async put(input: {
    id?: string;
    kind: string;
    config: Record<string, unknown>;
    enabled?: boolean;
  }): Promise<ConnectorConfig> {
    if (!this.impls.has(input.kind)) {
      throw new Error(`Unknown connector kind: ${input.kind}`);
    }
    const now = new Date().toISOString();
    const existing = input.id ? await this.store.getConnector(input.id) : null;
    const config: ConnectorConfig = {
      id:
        existing?.id ??
        input.id ??
        `${input.kind}-${ulid().slice(-8).toLowerCase()}`,
      kind: input.kind,
      config: input.config,
      enabled: input.enabled ?? true,
      created_at: existing?.created_at ?? now,
      updated_at: now,
    };
    await this.store.putConnector(config);
    return config;
  }

  async delete(id: string): Promise<boolean> {
    const deleted = await this.store.deleteConnector(id);
    if (deleted) {
      // remove all indexed material from this connector
      await this.pipeline.removeOriginDocuments(id);
    }
    return deleted;
  }

  /** Poll one connector; returns number of docs emitted this run. */
  async runConnector(id: string): Promise<number> {
    const conn = await this.store.getConnector(id);
    if (!conn) throw new Error(`Unknown connector: ${id}`);
    const impl = this.impls.get(conn.kind);
    if (!impl) throw new Error(`No implementation for kind ${conn.kind}`);
    const ctx: ConnectorContext = { store: this.store, log: this.log };
    const state = await this.store.getConnectorState(conn.id);
    const result = await impl.poll(conn.config, state, ctx);
    for (const doc of result.docs) {
      await this.pipeline.indexExternalContent({
        relPath: `${conn.id}/${doc.path}`,
        content: doc.content,
        origin: conn.id,
        title: doc.title,
        contentType: doc.contentType,
        mtime: doc.mtime,
      });
    }
    await this.store.setConnectorState(conn.id, result.state);
    return result.docs.length;
  }

  async runAll(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      for (const conn of await this.store.listConnectors()) {
        if (!conn.enabled) continue;
        try {
          await this.runConnector(conn.id);
        } catch (err) {
          this.log(`connector ${conn.id} failed`, err);
        }
      }
    } finally {
      this.running = false;
    }
  }

  start(intervalSeconds: number): void {
    const ms = Math.max(5, intervalSeconds) * 1000;
    this.timer = setInterval(() => void this.runAll(), ms);
    this.timer.unref?.();
  }

  stop(): void {
    clearInterval(this.timer);
    this.timer = undefined;
  }
}
