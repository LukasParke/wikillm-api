#!/usr/bin/env python3
"""
WikiLLM Memory & Knowledge Benchmark.
Deterministic evaluation: checks if expected documents are retrieved.
No LLM dependency — measures pure retrieval quality.

Usage:
    python3 scripts/memory_benchmark.py --port <port> [--label <name>]
"""

import argparse, json, os, sys, time, urllib.request, urllib.error
from datetime import datetime

def http(method, url, headers=None, body=None):
    req = urllib.request.Request(url, method=method)
    for k, v in (headers or {}).items(): req.add_header(k, v)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("Content-Type", "application/json")
    try:
        resp = urllib.request.urlopen(req, data=data, timeout=30)
        return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        try: return e.code, json.loads(e.read().decode())
        except: return e.code, {}
    except Exception as e:
        return 0, {"error": str(e)}

# --- Test corpus: each doc has known facts ---
CORPUS = [
    ("wiki/services/auth-service.md",
     "# Auth Service\n\nThe auth-service handles user authentication and JWT tokens.\nDepends on user-database. Owner: team-identity.",
     "auth-service"),
    ("wiki/services/payment-api.md",
     "# Payment API\n\nThe payment-api processes transactions via Stripe. Calls auth-service for validation. Uses PostgreSQL.",
     "payment-api"),
    ("wiki/infra/user-database.md",
     "# User Database\n\nPostgreSQL instance hosting user credentials. Read replicas in eu-west-1.",
     "user-database"),
    ("wiki/runbooks/auth-outage.md",
     "# Auth Outage Runbook\n\nIf auth-service returns 503: check pods, verify user-database connectivity, contact @alice.",
     "auth-outage-runbook"),
    ("wiki/concepts/jwt.md",
     "# JWT Tokens\n\nJWTs used by auth-service. Expire after 24 hours. RS256 algorithm.",
     "jwt"),
    ("wiki/incidents/db-failover.md",
     "# DB Failover Incident\n\nPayment-api had errors during user-database failover. Root cause: missing retry logic.",
     "db-failover"),
]

# Test cases: (dimension, search_query, expected_rel_paths, description)
TEST_CASES = [
    ("single_hop", "auth service JWT authentication", ["wiki/services/auth-service.md", "wiki/concepts/jwt.md"], "Auth docs for auth query"),
    ("single_hop", "payment Stripe transaction processing", ["wiki/services/payment-api.md"], "Payment doc for payment query"),
    ("single_hop", "PostgreSQL database credentials user profiles", ["wiki/infra/user-database.md"], "DB doc for database query"),
    ("multi_hop", "payment-api auth-service dependency validation chain", ["wiki/services/payment-api.md", "wiki/services/auth-service.md"], "Both services in dependency chain"),
    ("temporal", "database failover incident root cause missing retry", ["wiki/incidents/db-failover.md"], "Incident doc for incident query"),
    ("cross_ref", "outage runbook escalation auth service contact", ["wiki/runbooks/auth-outage.md", "wiki/services/auth-service.md"], "Runbook + auth service cross-reference"),
]

class Results:
    def __init__(self): self.tests = []
    def add(self, dim, query, hit_count, precision, recall, latency_ms, expected_found):
        self.tests.append({"dim": dim, "query": query, "hit_count": hit_count,
                          "precision": round(precision, 3), "recall": round(recall, 3),
                          "latency_ms": round(latency_ms), "expected_found": expected_found})
    def summary(self):
        dims = {}
        for t in self.tests:
            d = dims.setdefault(t["dim"], {"p": [], "r": [], "lat": []})
            d["p"].append(t["precision"]); d["r"].append(t["recall"]); d["lat"].append(t["latency_ms"])
        lines = [f"{'Dimension':<20s} {'Precision':>9s} {'Recall':>7s} {'Avg ms':>8s}", "-" * 48]
        total_p, total_r, total_lat, n = [], [], [], 0
        for dim, d in sorted(dims.items()):
            ap = sum(d["p"])/max(len(d["p"]),1); ar = sum(d["r"])/max(len(d["r"]),1); alat = sum(d["lat"])/max(len(d["lat"]),1)
            lines.append(f"{dim:<20s} {ap:>9.1%} {ar:>7.1%} {alat:>7.0f}ms")
            total_p.extend(d["p"]); total_r.extend(d["r"]); total_lat.extend(d["lat"])
        lines.append("-"*48)
        tp = sum(total_p)/max(len(total_p),1); tr = sum(total_r)/max(len(total_r),1); tl = sum(total_lat)/max(len(total_lat),1)
        lines.append(f"{'TOTAL':<20s} {tp:>9.1%} {tr:>7.1%} {tl:>7.0f}ms")
        return "\n".join(lines)

def run(port, key, label):
    base = f"http://127.0.0.1:{port}"
    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}

    # Seed corpus
    print(f"Seeding {len(CORPUS)} documents...")
    for rel_path, content, _ in CORPUS:
        title = content.split("\n")[0].lstrip("# ").strip()
        st, _ = http("PUT", f"{base}/v1/pages/{rel_path}", headers=headers,
                     body={"content": content.replace("\n", "\n"), "frontmatter": {"type": "Note"}})
        if st != 200:
            print(f"  WARN: seed {rel_path} -> {st}")

    time.sleep(1)

    results = Results()
    print(f"\nRunning {len(TEST_CASES)} test queries [{label}]...")
    for dim, query, expected_paths, desc in TEST_CASES:
        t0 = time.time()
        status, resp = http("GET", f"{base}/v1/search?q={urllib.parse.quote(query)}&limit=10", headers=headers)
        latency = (time.time() - t0) * 1000

        hits = resp.get("results", []) if isinstance(resp, dict) else []
        retrieved = [h.get("rel_path", "") for h in hits]
        relevant_found = sum(1 for e in expected_paths if e in retrieved)

        precision = relevant_found / max(len(retrieved), 1) if retrieved else 0
        recall = relevant_found / max(len(expected_paths), 1)
        mode = resp.get("mode", "?") if isinstance(resp, dict) else "?"

        mark = "✓" if recall >= 0.5 else "✗"
        print(f"  {mark} [{dim}] {desc}: p={precision:.0%} r={recall:.0%}")
        expected_found = sum(1 for e in expected_paths if e in retrieved)
        results.add(dim, query, len(retrieved), precision, recall, latency, expected_found)

    return results

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3800)
    parser.add_argument("--key", default="testkey")
    parser.add_argument("--label", default="default")
    args = parser.parse_args()

    import urllib.parse
    print(f"\n{'='*50}")
    print(f" WikiLLM Memory Benchmark [{args.label}]")
    print(f"{'='*50}")
    results = run(args.port, args.key, args.label)
    print(results.summary())
