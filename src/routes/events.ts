import { Hono } from "hono";
import type { AppVariables } from "../app.js";

const app = new Hono<{ Variables: AppVariables }>();

app.get("/", async (c) => {
  const broadcaster = c.get("broadcaster");
  if (!broadcaster) {
    return c.json({ error: "broadcaster_unavailable" }, 503);
  }

  return new Response(
    new ReadableStream({
      start(controller) {
        const clientId = broadcaster.addSSE({
          send: (data) => controller.enqueue(new TextEncoder().encode(data)),
          close: () => controller.close(),
        });
        c.req.raw.signal.addEventListener("abort", () => {
          broadcaster.removeSSE(clientId);
          controller.close();
        });
      },
    }),
    {
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      },
    },
  );
});

export default app;
