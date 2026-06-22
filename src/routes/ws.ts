import { Hono } from "hono";
import type { AppVariables } from "../app.js";
import type { WSContext } from "hono/ws";

let upgradeWebSocket: typeof import("hono/bun").upgradeWebSocket | undefined;
try {
  ({ upgradeWebSocket } = await import("hono/bun"));
} catch {
  // Bun adapter not available on Node test runs
}

const app = new Hono<{ Variables: AppVariables }>();

if (upgradeWebSocket) {
  app.get(
    "/",
    upgradeWebSocket((c) => {
      const broadcaster = c.get("broadcaster");
      return {
        onOpen(_event, ws) {
          if (broadcaster) {
            broadcaster.addWS(ws);
          }
        },
        onClose(_event, ws) {
          if (broadcaster) {
            broadcaster.removeWS(ws);
          }
        },
        onMessage(event, ws) {
          // Minimal protocol support: echo pings, ignore rest
          try {
            const msg = JSON.parse(event.data as string);
            if (msg.type === "ping") {
              ws.send(
                JSON.stringify({
                  type: "pong",
                  time: new Date().toISOString(),
                }),
              );
            }
          } catch {
            // ignore non-JSON messages
          }
        },
      };
    }),
  );
} else {
  app.get("/", (c) =>
    c.json(
      {
        error: "websocket_unavailable",
        message: "WebSocket requires Bun runtime",
      },
      503,
    ),
  );
}

export default app;
