import type { Context, Next } from "hono";
import { createMiddleware } from "hono/factory";

interface Bucket {
  windowStart: number;
  count: number;
}

/**
 * Fixed-window per-identity rate limiter. Identity = bearer key name when
 * authenticated, else client IP. RATE_LIMIT_RPM=0 disables limiting.
 */
export function rateLimitMiddleware(requestsPerMinute: number) {
  const buckets = new Map<string, Bucket>();
  return createMiddleware(async (c: Context, next: Next) => {
    if (requestsPerMinute <= 0) return next();
    const identity =
      c.get("auth")?.name ?? c.req.header("x-forwarded-for") ?? "ip unknown";
    const now = Date.now();
    const bucket = buckets.get(identity);
    if (!bucket || now - bucket.windowStart >= 60_000) {
      buckets.set(identity, { windowStart: now, count: 1 });
      return next();
    }
    bucket.count += 1;
    if (bucket.count > requestsPerMinute) {
      const retryAfter = Math.ceil((bucket.windowStart + 60_000 - now) / 1000);
      c.header("Retry-After", String(retryAfter));
      return c.json(
        { error: "rate_limited", message: "Too many requests" },
        429,
      );
    }
    // opportunistic cleanup keeps the map bounded
    if (buckets.size > 10_000) {
      for (const [key, value] of buckets) {
        if (now - value.windowStart >= 60_000) buckets.delete(key);
      }
    }
    return next();
  });
}
