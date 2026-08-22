#!/usr/bin/env python3
"""
WikiLLM New-Surface Benchmark — endpoints added by the Foundation-derived
memory loop (2026-08-22).

Measures latency percentiles + throughput for:
  - POST /v1/memory            (store with dedupe/consolidation path)
  - GET  /v1/memory            (LIKE search + access bump)
  - GET  /v1/memory/:id/history
  - POST /v1/sessions          (+ message extraction + scoped get)
  - GET  /v1/pages/:p/versions (+ :seq body fetch)
  - GET  /v1/pages/:p/diff     (LCS unified diff)
  - GET  /v1/communities       (+ docs)
  - GET  /v1/admin/gaps

Usage: python3 scripts/new_surface_benchmark.py --port <port> --key <name:key>
"""

import argparse, json, statistics, time, urllib.parse, urllib.request, urllib.error


def http(method, url, headers=None, body=None):
    req = urllib.request.Request(url, method=method)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("Content-Type", "application/json")
    try:
        resp = urllib.request.urlopen(req, data=data, timeout=30)
        raw = resp.read().decode()
        try:
            return resp.status, json.loads(raw)
        except json.JSONDecodeError:
            return resp.status, {"_raw": raw}  # text/plain endpoints (diff, revision bodies)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode())
        except Exception:
            return e.code, {}
    except Exception as e:
        return 0, {"error": str(e)}


def pct(xs, p):
    if not xs:
        return 0.0
    xs = sorted(xs)
    return xs[min(int(len(xs) * p), len(xs) - 1)]


FAILURES = []

def bench(name, n, fn):
    lat = []
    ok = 0
    t0 = time.time()
    for i in range(n):
        s = time.time()
        st, body = fn(i)
        lat.append((time.time() - s) * 1000.0)
        if 200 <= st < 300:
            ok += 1
        else:
            FAILURES.append((name, i, st, json.dumps(body)[:160]))
    wall = time.time() - t0
    print(f"  {name:<34s} p50={pct(lat,0.5):7.2f}ms p95={pct(lat,0.95):7.2f}ms "
          f"p99={pct(lat,0.99):7.2f}ms ok={ok}/{n} rps={n/wall:9.0f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=4125)
    ap.add_argument("--key", default="bench:benchkey")
    ap.add_argument("--n", type=int, default=200, help="iterations per endpoint")
    args = ap.parse_args()

    base = f"http://127.0.0.1:{args.port}"
    H = {"Authorization": f"Bearer {args.key.split(':')[1] if ':' in args.key else args.key}"}
    name = args.key.split(":")[0]

    print("=" * 78)
    print(f"WIKILLM NEW-SURFACE BENCHMARK — port {args.port}, n={args.n}/endpoint")
    print("=" * 78)

    # Seed one page twice so revisions/diff have material.
    http("PUT", f"{base}/v1/pages/wiki/bench-surface.md", headers=H,
         body={"content": "# Bench Surface v1\n\nalpha bravo charlie delta. See [[bench-link]].\n"})
    http("PUT", f"{base}/v1/pages/wiki/bench-link.md", headers=H,
         body={"content": "# Bench Link\n\nlinked from [[bench-surface]].\n"})
    st, page = http("GET", f"{base}/v1/pages/wiki/bench-surface.md", headers=H)
    h1 = page.get("hash", "")
    http("PUT", f"{base}/v1/pages/wiki/bench-surface.md", headers=H,
         body={"content": "# Bench Surface v2\n\nalpha bravo charlie delta echo foxtrot.\n", "ifMatch": h1})

    sid_holder = {}

    print("\n-- memory ledger --")
    facts = [f"Bench fact {i}: the deploy pipeline runs on github actions {i}" for i in range(args.n)]
    def store(i):
        return http("POST", f"{base}/v1/memory", headers=H,
                    body={"content": facts[i % len(facts)], "memory_type": "semantic",
                          "agent_name": "benchsurf"})
    bench("POST /v1/memory (store)", args.n, store)

    def search(i):
        return http("GET", f"{base}/v1/memory?agent=benchsurf&q=deploy+pipeline&limit=5", headers=H)
    st, r = search(0)
    mid = (r.get("memories") or [{}])[0].get("id", "")
    bench("GET  /v1/memory (search+bump)", args.n, search)

    bench("GET  /v1/memory/:id/history", args.n,
          lambda i: http("GET", f"{base}/v1/memory/{mid}/history", headers=H))

    print("\n-- sessions --")
    def sess_start(i):
        return http("POST", f"{base}/v1/sessions", headers=H, body={"agent_name": "benchsurf"})
    bench("POST /v1/sessions", min(args.n, 50), sess_start)

    st, sresp = sess_start(0)
    sid = sresp["session"]["id"]
    msgs = [f"Session turn {i}: the indexer prefers batched writes over single puts {i}" for i in range(args.n)]
    def msg(i):
        return http("POST", f"{base}/v1/sessions/{sid}/messages", headers=H,
                    body={"role": "user", "content": msgs[i % len(msgs)]})
    bench("POST /v1/sessions/:id/messages", args.n, msg)

    bench("GET  /v1/sessions/:id", min(args.n, 50),
          lambda i: http("GET", f"{base}/v1/sessions/{sid}", headers=H))

    print("\n-- versioning & diffs --")
    bench("GET  /v1/pages/:p/versions", args.n,
          lambda i: http("GET", f"{base}/v1/pages/wiki/bench-surface.md/versions", headers=H))
    bench("GET  /v1/pages/:p/versions/:seq", args.n,
          lambda i: http("GET", f"{base}/v1/pages/wiki/bench-surface.md/versions/1", headers=H))
    bench("GET  /v1/pages/:p/diff (LCS)", args.n,
          lambda i: http("GET", f"{base}/v1/pages/wiki/bench-surface.md/diff?from=1&to=2", headers=H))

    print("\n-- knowledge graph --")
    bench("GET  /v1/communities (TTL cached)", args.n,
          lambda i: http("GET", f"{base}/v1/communities", headers=H))
    st, cs = http("GET", f"{base}/v1/communities", headers=H)
    cid = (cs.get("communities") or [{}])[0].get("id", "")
    if cid:
        bench("GET  /v1/communities/:id/docs", args.n,
              lambda i: http("GET", f"{base}/v1/communities/{cid}/docs", headers=H))
    else:
        print("  GET  /v1/communities/:id/docs      skipped (no communities in corpus)")

    print("\n-- admin --")
    bench("GET  /v1/admin/gaps", min(args.n, 50),
          lambda i: http("GET", f"{base}/v1/admin/gaps", headers=H))

    if FAILURES:
        print("\nFAILURES:")
        for f in FAILURES:
            print(" ", f)
    print("\nDone.")


if __name__ == "__main__":
    main()
