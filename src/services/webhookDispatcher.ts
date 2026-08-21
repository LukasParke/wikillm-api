import { createHmac } from "node:crypto";
import { ulid } from "ulidx";
import type { Store, WebhookRecord } from "../store/types.js";
import type { ChangeEvent } from "../types/index.js";
import type { SettingsService } from "./settingsService.js";

const RETRY_DELAYS_MS = [250, 1000, 4000];

/**
 * Outbound change webhooks: POST the change event to each matching subscriber
 * with an HMAC-SHA256 signature header, retrying with backoff. Delivery state
 * (last_status) is recorded on the webhook row.
 */
export class WebhookDispatcher {
  constructor(
    private readonly store: Store,
    private readonly settings: SettingsService,
  ) {}

  async dispatch(event: ChangeEvent["data"]): Promise<void> {
    const hooks = await this.store.listWebhooks();
    const matching = hooks.filter(
      (h) =>
        h.enabled &&
        h.events.includes("change") &&
        matchPrefixes(h.prefixes, event.rel_path),
    );
    await Promise.allSettled(matching.map((hook) => this.deliver(hook, event)));
  }

  private async deliver(
    hook: WebhookRecord,
    event: ChangeEvent["data"],
  ): Promise<void> {
    const body = JSON.stringify({ type: "change", data: event });
    const secret = await this.settings.get<string>("webhook_secret");
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      "X-WikiLLM-Event": "change",
    };
    if (secret) {
      headers["X-WikiLLM-Signature"] = `sha256=${createHmac("sha256", secret)
        .update(body)
        .digest("hex")}`;
    }

    let lastStatus = "unknown";
    for (let attempt = 0; attempt <= RETRY_DELAYS_MS.length; attempt += 1) {
      if (attempt > 0) {
        await new Promise((r) => setTimeout(r, RETRY_DELAYS_MS[attempt - 1]));
      }
      try {
        const response = await fetch(hook.url, {
          method: "POST",
          headers,
          body,
          signal: AbortSignal.timeout(10_000),
        });
        lastStatus = String(response.status);
        if (response.ok) break;
      } catch (err) {
        lastStatus = `error: ${(err as Error).message.slice(0, 120)}`;
      }
    }
    await this.store.recordWebhookAttempt(hook.id, lastStatus);
  }
}

/** Prefix match tolerant of trailing slashes; "*" matches everything. */
export function matchPrefixes(prefixes: string[], relPath: string): boolean {
  for (const raw of prefixes) {
    const prefix = raw.endsWith("/") ? raw.slice(0, -1) : raw;
    if (prefix === "*") return true;
    if (relPath === prefix || relPath.startsWith(`${prefix}/`)) return true;
  }
  return false;
}

export function newWebhookId(): string {
  return `wh-${ulid().slice(-8).toLowerCase()}`;
}
