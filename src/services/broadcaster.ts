import type { WSContext } from "hono/ws";
import type { ChangeEvent } from "../types/index.js";

export interface SSEClient {
  id: string;
  send: (data: string) => void;
  close: () => void;
}

export class Broadcaster {
  private sseClients = new Map<string, SSEClient>();
  private wsClients = new Set<WSContext<unknown>>();
  private idCounter = 0;

  addSSE(client: Omit<SSEClient, "id">): string {
    const id = String(++this.idCounter);
    this.sseClients.set(id, { id, ...client });
    return id;
  }

  removeSSE(id: string): void {
    this.sseClients.delete(id);
  }

  addWS(ws: WSContext<unknown>): void {
    this.wsClients.add(ws);
  }

  removeWS(ws: WSContext<unknown>): void {
    this.wsClients.delete(ws);
  }

  broadcast(event: ChangeEvent): void {
    const payload = JSON.stringify(event);

    // SSE: format as event stream
    const ssePayload = `event: change\ndata: ${payload}\n\n`;
    for (const client of this.sseClients.values()) {
      try {
        client.send(ssePayload);
      } catch {
        // ignore stale client
      }
    }

    // WebSocket: send raw JSON
    for (const ws of this.wsClients) {
      try {
        ws.send(payload);
      } catch {
        // ignore stale client
      }
    }
  }

  get sseCount(): number {
    return this.sseClients.size;
  }

  get wsCount(): number {
    return this.wsClients.size;
  }
}

export function createBroadcaster(): Broadcaster {
  return new Broadcaster();
}
