import type { Store } from "../store/types.js";
import type { ConnectorConfig } from "../store/types.js";

export interface IncomingDoc {
  path: string;
  content: string;
  title?: string;
  contentType?: string;
  mtime?: number;
}

export interface ConnectorContext {
  store: Store;
  log(msg: string, err?: unknown): void;
}

export interface ConnectorPollResult {
  docs: IncomingDoc[];
  state: unknown;
}

export interface ConnectorImpl {
  kind: string;
  poll(
    config: Record<string, unknown>,
    state: unknown,
    ctx: ConnectorContext,
  ): Promise<ConnectorPollResult>;
}

export function strConfig(
  config: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = config[key];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

export function numConfig(
  config: Record<string, unknown>,
  key: string,
): number | undefined {
  const value = config[key];
  if (typeof value === "number" && Number.isFinite(value)) return value;
  return undefined;
}

export function boolConfig(
  config: Record<string, unknown>,
  key: string,
  fallback: boolean,
): boolean {
  const value = config[key];
  return typeof value === "boolean" ? value : fallback;
}

export function strListConfig(
  config: Record<string, unknown>,
  key: string,
  fallback: string[],
): string[] {
  const value = config[key];
  return Array.isArray(value)
    ? value.filter((v): v is string => typeof v === "string")
    : fallback;
}

export function connectorDisplayName(conn: ConnectorConfig): string {
  return conn.id;
}
