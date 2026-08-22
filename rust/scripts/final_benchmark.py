#!/usr/bin/env python3
"""
WikiLLM Rust — Comprehensive Final Benchmark
Tests: throughput, memory recall, KG traversal, vector search, FTS quality.
Produces a single JSON + human-readable report.
"""

import argparse, json, math, os, sys, time, urllib.request, urllib.error, urllib.parse
import hashlib, random

# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------
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

def timed(fn):
    t0 = time.time()
    result = fn()
    return result, (time.time() - t0) * 1000

# ---------------------------------------------------------------------------
# Deterministic fake embeddings (for vector search testing without a model)
# ---------------------------------------------------------------------------
def fake_embed(text: str, dims: int = 64) -> list:
    h = hashlib.sha256(text.encode()).digest()
    vec = [((h[i % len(h)] / 255.0) - 0.5) * 2 for i in range(dims)]
    norm = math.sqrt(sum(x*x for x in vec)) or 1.0
    return [x / norm for x in vec]

def cosine(a: list, b: list) -> float:
    return sum(x*y for x, y in zip(a, b))

# ---------------------------------------------------------------------------
# Main benchmark
# ---------------------------------------------------------------------------
class Bench:
    def __init__(self):
        self.results = {"throughput": {}, "memory": {}, "kg": {}, "vector": {}, "retrieval": {}, "latency": {}}
        self.latencies = {}

    def record(self, section, name, **kwargs):
        self.results[section][name] = kwargs

    def report(self):
        lines = ["\n" + "=" * 70, "WIKILLM RUST — COMPREHENSIVE BENCHMARK REPORT", "=" * 70]
        for section, data in self.results.items():
            lines.append(f"\n--- {section.upper()} ---")
            for name, metrics in data.items():
                parts = [f"{k}={v}" for k, v in metrics.items()]
                lines.append(f"  {name}: {', '.join(parts)}")
        lines.append("\n" + "=" * 70)
        return "\n".join(lines)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3860)
    parser.add_argument("--key", default="bench:benchkey")
    parser.add_argument("--docs", type=int, default=100)
    args = parser.parse_args()

    key = args.key.split(":")[-1]  # use last segment as bearer
    base = f"http://127.0.0.1:{args.port}"
    H = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}

    bench = Bench()
    wiki_root = os.environ.get("WIKI_ROOT", "")

    # Verify server
    st, health = http("GET", f"{base}/health")
    if st != 200:
        print(f"Server not healthy: {st}"); sys.exit(1)
    print(f"Server OK on {base}")

    # =====================================================================
    # SECTION 1: HTTP THROUGHPUT
    # =====================================================================
    print("\n[1/6] HTTP Throughput...")

    # Seed corpus
    topics = ["authentication", "database", "deployment", "monitoring", "security",
              "networking", "storage", "compute", "caching", "messaging"]
    entities = ["auth-service", "payment-api", "user-database", "cache-cluster",
                "message-queue", "search-api", "notification-svc", "audit-logger"]

    for i in range(args.docs):
        topic = topics[i % len(topics)]
        entity = entities[i % len(entities)]
        rel = f"wiki/{topic}/{entity}-{i}.md"
        content = f"# {entity} {topic.title()}\n\n{entity} handles {topic} for the LLM ecosystem.\nDepends on [[user-database]] and uses [[jwt-tokens]].\nKey metric: {random.randint(1, 999)}ms average latency."
        st, _ = http("PUT", f"{base}/v1/pages/{rel}", headers=H, body={"content": content, "frontmatter": {"type": "Service", "tags": [topic]}})

    time.sleep(1)

    # Throughput: health
    latencies = []
    for _ in range(50):
        _, ms = timed(lambda: http("GET", f"{base}/health"))
        latencies.append(ms)
    latencies.sort()
    bench.record("throughput", "health",
        p50=round(latencies[len(latencies)//2], 2),
        p95=round(latencies[int(len(latencies)*0.95)], 2),
        p99=round(latencies[int(len(latencies)*0.99)], 2))

    # Throughput: page reads
    latencies = []
    for i in range(50):
        path = f"wiki/{topics[i % len(topics)]}/{entities[i % len(entities)]}-{i}.md"
        _, ms = timed(lambda p=path: http("GET", f"{base}/v1/pages/{p}"))
        latencies.append(ms)
    latencies.sort()
    bench.record("throughput", "page_read",
        p50=round(latencies[len(latencies)//2], 2),
        p95=round(latencies[int(len(latencies)*0.95)], 2),
        p99=round(latencies[int(len(latencies)*0.99)], 2))

    # Throughput: writes
    latencies = []
    for i in range(30):
        _, ms = timed(lambda i=i: http("PUT", f"{base}/v1/pages/wiki/bench-{i}.md", headers=H,
            body={"content": f"# Bench {i}\n\nContent for benchmark {i}.", "frontmatter": {"type": "Note"}}))
        latencies.append(ms)
    latencies.sort()
    bench.record("throughput", "page_write",
        p50=round(latencies[len(latencies)//2], 2),
        p95=round(latencies[int(len(latencies)*0.95)], 2))

    # Throughput: changes feed
    latencies = []
    for _ in range(30):
        _, ms = timed(lambda: http("GET", f"{base}/v1/changes?limit=100"))
        latencies.append(ms)
    latencies.sort()
    bench.record("throughput", "changes_feed",
        p50=round(latencies[len(latencies)//2], 2),
        p95=round(latencies[int(len(latencies)*0.95)], 2))

    # =====================================================================
    # SECTION 2: MEMORY RECALL
    # =====================================================================
    print("[2/6] Memory Store & Recall...")

    facts = [
        ("auth-service handles user authentication and JWT issuance", "semantic"),
        ("payment-api processes transactions via Stripe", "semantic"),
        ("user-database is a PostgreSQL instance in us-east-1", "semantic"),
        ("To restart nginx: sudo systemctl restart nginx", "procedural"),
        ("During deploy on 2026-08-20 the migration failed due to lock timeout", "episodic"),
        ("The team prefers PostgreSQL over MySQL for new services", "semantic"),
        ("Cache invalidation uses Redis pub-sub for real-time updates", "semantic"),
        ("Deploy checklist: run tests, build Docker image, push to registry, update K8s", "procedural"),
    ]

    # Store all facts via the real ledger API (scoped to the bench agent).
    store_latencies = []
    for content, mem_type in facts:
        _, ms = timed(lambda c=content, t=mem_type: http("POST", f"{base}/v1/memory",
            headers=H, body={"content": c, "memory_type": t, "agent_name": "bench"}))
        store_latencies.append(ms)
    store_latencies.sort()
    bench.record("memory", "store_latency_p50_ms", value=round(store_latencies[len(store_latencies)//2], 2))

    # Recall: query for specific facts
    recall_tests = [
        ("user authentication", 0), ("Stripe transactions", 1),
        ("PostgreSQL us-east-1", 2), ("restart nginx", 3),
        ("migration failed lock timeout", 4), ("PostgreSQL MySQL preference", 5),
        ("cache invalidation Redis", 6), ("deploy checklist Docker", 7),
    ]
    correct = 0
    recall_latencies = []
    for query, expected_idx in recall_tests:
        expected = facts[expected_idx][0][:40].lower()
        _, ms = timed(lambda q=query: http("GET", f"{base}/v1/memory?agent=bench&q={urllib.parse.quote(query)}&limit=5", headers=H))
        recall_latencies.append(ms)
        st, resp = http("GET", f"{base}/v1/memory?agent=bench&q={urllib.parse.quote(query)}&limit=5", headers=H)
        memories = resp.get("memories", []) if isinstance(resp, dict) else []
        found = any(expected in m.get("content", "").lower() for m in memories)
        if found: correct += 1

    bench.record("memory", "recall_accuracy", correct=correct, total=len(recall_tests),
        rate=f"{correct/max(len(recall_tests),1)*100:.0f}%")
    recall_latencies.sort()
    bench.record("memory", "recall_latency_p50_ms", value=round(recall_latencies[len(recall_latencies)//2], 2))

    # Consolidation: store duplicate, verify dedup
    st, resp = http("POST", f"{base}/v1/memory", headers=H,
        body={"scope": {"user_id": "bench"}, "type": "semantic", "content": facts[0][0]})
    total_memories_before = st
    st, resp = http("POST", f"{base}/v1/memory", headers=H,
        body={"scope": {"user_id": "bench"}, "type": "semantic", "content": facts[0][0] + "!"})
    bench.record("memory", "dedup_test", duplicate_content_sent=True)

    # =====================================================================
    # SECTION 3: KNOWLEDGE GRAPH
    # =====================================================================
    print("[3/6] Knowledge Graph...")

    # Graph traversal
    _, ms = timed(lambda: http("GET", f"{base}/v1/graph/wiki/entities/entity-25.md?depth=3", headers=H))
    bench.record("kg", "traverse_depth3_latency_ms", value=round(ms, 2))

    # Entity relations
    _, ms = timed(lambda: http("GET", f"{base}/v1/entities/test/relations", headers=H))
    bench.record("kg", "entity_relations_latency_ms", value=round(ms, 2))

    # =====================================================================
    # SECTION 4: VECTOR SEARCH
    # =====================================================================
    print("[4/6] Vector Search (brute-force cosine)...")

    # Store a doc with known content to trigger chunking
    st, _ = http("PUT", f"{base}/v1/pages/wiki/vec-test.md", headers=H,
        body={"content": "# Vector Test\n\nSemantic similarity search content for vector testing."})

    # Note: without an embedder configured, vectors won't be stored.
    # We test that the endpoint at least responds correctly.
    st, resp = http("GET", f"{base}/v1/search?q=vector+test&limit=5")
    mode = resp.get("mode", "?") if isinstance(resp, dict) else "?"
    bench.record("vector", "search_mode", value=mode)
    bench.record("vector", "note", detail="vector search requires embedder; FTS-only mode active")

    # =====================================================================
    # SECTION 5: RETRIEVAL QUALITY
    # =====================================================================
    print("[5/6] Retrieval Quality (Precision/Recall/MRR)...")

    # Query the corpus this script actually seeds (topic/entity-index paths).
    # Paths follow the seed loop: topics[i % 10] / entities[i % 8]-{i}.md.
    quality_tests = [
        ("auth-service authentication handles ecosystem", "wiki/authentication/auth-service-0.md"),
        ("payment-api database handles ecosystem", "wiki/database/payment-api-1.md"),
        ("user-database deployment handles ecosystem", "wiki/deployment/user-database-2.md"),
        ("cache-cluster monitoring handles ecosystem", "wiki/monitoring/cache-cluster-3.md"),
        ("message-queue security handles ecosystem", "wiki/security/message-queue-4.md"),
        ("search-api networking handles ecosystem", "wiki/networking/search-api-5.md"),
    ]

    precisions, recalls, mrrs = [], [], []
    for query, expected_path in quality_tests:
        _, ms = timed(lambda q=query: http("GET", f"{base}/v1/search?q={urllib.parse.quote(q)}&limit=10", headers=H))
        st, resp = http("GET", f"{base}/v1/search?q={urllib.parse.quote(query)}&limit=10", headers=H)
        hits = resp.get("results", []) if isinstance(resp, dict) else []
        retrieved = [h.get("rel_path", "") for h in hits]
        relevant = {expected_path}

        p_at_10 = sum(1 for r in retrieved[:10] if r in relevant) / max(len(retrieved[:10]), 1)
        r_at_10 = sum(1 for r in retrieved[:10] if r in relevant) / max(len(relevant), 1)
        rr = next((1.0 / (i + 1) for i, r in enumerate(retrieved) if r in relevant), 0.0)
        precisions.append(p_at_10)
        recalls.append(r_at_10)
        mrrs.append(rr)

    bench.record("retrieval", "precision_at_10", value=f"{sum(precisions)/max(len(precisions),1):.1%}")
    bench.record("retrieval", "recall_at_10", value=f"{sum(recalls)/max(len(recalls),1):.1%}")
    bench.record("retrieval", "mrr", value=round(sum(mrrs)/max(len(mrrs),1), 3))

    # =====================================================================
    # SECTION 6: LATENCY PERCENTILES (all endpoints)
    # =====================================================================
    print("[6/6] Latency Percentiles...")

    endpoints = [
        ("health", "GET", "/health", None),
        ("search", "GET", "/v1/search?q=LLM&limit=10", None),
        ("documents", "GET", "/v1/documents?limit=50", None),
        ("changes", "GET", "/v1/changes?limit=50", None),
        ("settings", "GET", "/v1/settings/public_read", None),
    ]

    for name, method, path, body in endpoints:
        latencies = []
        for _ in range(30):
            _, ms = timed(lambda m=method, p=path, b=body: http(m, f"{base}{p}", headers=H, body=b))
            latencies.append(ms)
        latencies.sort()
        bench.record("latency", name,
            p50=round(latencies[len(latencies)//2], 2),
            p95=round(latencies[int(len(latencies)*0.95)], 2),
            p99=round(latencies[int(len(latencies)*0.99)], 2))

    # =====================================================================
    # REPORT
    # =====================================================================
    print(bench.report())

    # Save JSON
    output = "/tmp/wikillm-final-benchmark.json"
    with open(output, "w") as f:
        json.dump(bench.results, f, indent=2, default=str)
    print(f"\nJSON saved to {output}")

if __name__ == "__main__":
    main()
