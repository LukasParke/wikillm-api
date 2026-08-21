import { createHash, randomBytes } from "node:crypto";
import type { Store } from "../store/types.js";
import type { AuthInfo } from "./projectService.js";

export interface EnvKeyEntry {
  name: string;
  secret: string;
  role: "admin" | "write" | "read";
  scope: string[];
}

export function sha256Hex(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

export function generateKeySecret(): string {
  return `wk_${randomBytes(24).toString("hex")}`;
}

/**
 * Resolves bearer secrets to identities from two sources: env-configured keys
 * (bootstrap/immutable) and DB-managed keys (created via API/MCP, stored as
 * SHA-256 hashes). DB keys take effect immediately; env keys always win on
 * duplicate secret.
 */
export class KeyRegistry {
  constructor(
    private readonly store: Store,
    private readonly envKeys: Map<string, EnvKeyEntry>,
  ) {}

  async verify(secret: string): Promise<AuthInfo | null> {
    const env = this.envKeys.get(secret);
    if (env) {
      return { name: env.name, role: env.role, projects: env.scope };
    }
    const record = await this.store.findApiKeyByHash(sha256Hex(secret));
    if (!record) return null;
    return { name: record.name, role: record.role, projects: record.scope };
  }

  hasEnvKeys(): boolean {
    return this.envKeys.size > 0;
  }

  async isEmpty(): Promise<boolean> {
    return !this.hasEnvKeys() && (await this.store.countApiKeys()) === 0;
  }

  async createKey(input: {
    name?: string;
    secret?: string;
    role?: "admin" | "write" | "read";
    scope?: string[];
    createdBy: string;
  }): Promise<{
    name: string;
    secret: string;
    prefix: string;
    role: string;
    scope: string[];
  }> {
    const secret = input.secret ?? generateKeySecret();
    const name =
      input.name?.trim() || `agent-${randomBytes(3).toString("hex")}`;
    const existing = await this.store.getApiKey(name);
    if (existing) throw new Error(`Key name already exists: ${name}`);
    await this.store.upsertApiKey({
      name,
      key_hash: sha256Hex(secret),
      key_prefix: secret.slice(0, 6),
      scope: input.scope ?? ["*"],
      role: input.role ?? "write",
      created_by: input.createdBy,
    });
    return {
      name,
      secret,
      prefix: secret.slice(0, 6),
      role: input.role ?? "write",
      scope: input.scope ?? ["*"],
    };
  }

  async deleteKey(name: string): Promise<boolean> {
    return this.store.deleteApiKey(name);
  }
}
