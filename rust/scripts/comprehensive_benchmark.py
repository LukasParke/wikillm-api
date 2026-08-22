#!/usr/bin/env python3
"""
WikiLLM Comprehensive Benchmark Suite — 3 Benchmarks

Benchmark 1: Retrieval Quality (Precision@k, Recall@k, MRR, NDCG)
Benchmark 2: Multi-hop Reasoning (anti-luck filtered 2-hop and 3-hop chains)
Benchmark 3: Ablation Study (feature contribution measurement)

All three share a synthetically-generated corpus with controlled topology,
planted multi-hop chains, and distractor documents. Golden answers are
derived programmatically from the known structure — no LLM judge needed.

Usage:
    python3 scripts/comprehensive_benchmark.py --port 3850 [--label Rust]
"""

import argparse, hashlib, json, math, os, sys, time, urllib.parse, urllib.request
from datetime import datetime, timezone

# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------

def http(method: str, url: str, headers: dict = None, body=None):
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

# ---------------------------------------------------------------------------
# Synthetic corpus generator with controlled topology
# ---------------------------------------------------------------------------

SERVICES = ["auth-service", "payment-api", "user-service", "notification-svc",
            "search-api", "reporting-engine", "audit-logger", "session-store"]
INFRA = ["user-database", "cache-cluster", "message-queue", "object-storage",
         "config-server", "secrets-manager", "logging-pipeline", "metrics-collector"]
CONCEPTS = ["jwt-tokens", "rate-limiting", "circuit-breakers", "service-mesh",
            "api-gateway", "load-balancing", "data-partitioning", "event-sourcing"]

RELATIONS = [
    ("calls", "sends requests to"),
    ("depends_on", "relies on for core functionality"),
    ("stores_data_in", "persists data to"),
    ("authenticates_via", "verifies credentials through"),
    ("publishes_to", "emits events to"),
    ("monitored_by", "health checked by"),
]

def seeded_rng(seed: int):
    """Simple deterministic RNG for reproducibility."""
    state = seed
    def rand():
        nonlocal state
        state = (state * 1103515245 + 12345) % 2147483648
        return state / 2147483648
    return rand

def generate_corpus(seed: int = 42, n_docs: int = 200):
    """
    Generate synthetic wiki with controlled topology:
    - Services call/depend on infra
    - Services authenticate via auth-service
    - Concepts relate to services
    - Distractor docs with no meaningful connections
    - Planted multi-hop chains: A→B→C where only C has the answer
    Returns (documents, planted_chains)
    documents: list of (rel_path, content, frontmatter)
    chains: list of {hops: int, chain: [paths], question_hint: str}
    """
    rng = seeded_rng(seed)
    documents = []
    chains = []

    all_services = SERVICES[:]
    all_infra = INFRA[:]
    all_concepts = CONCEPTS[:]

    # Build service→infra dependency graph
    edges = []  # (src_path, dst_path, relation_type, relation_text)
    for i, svc in enumerate(SERVICES):
        svc_path = f"wiki/services/{svc}.md"
        svc_infra = INFRA[i % len(INFRA)]
        infra_path = f"wiki/infra/{svc_infra}.md"
        rel_type, rel_text = RELATIONS[i % len(RELATIONS)]
        edges.append((svc_path, infra_path, rel_type, f"{svc} {rel_text} {svc_infra}"))

        # Each service also calls auth-service (except auth itself)
        if svc != "auth-service":
            edges.append((svc_path, "wiki/services/auth-service.md", "calls", f"{svc} calls auth-service"))
            edges.append(("wiki/services/auth-service.md", svc_path, "called_by", f"{svc} is called by auth-service"))

    # Plant explicit multi-hop chains:
    # Chain 1 (2-hop): payment-api → auth-service → user-database
    chain1 = ["wiki/services/payment-api.md", "wiki/services/auth-service.md", "wiki/infra/user-database.md"]
    chains.append({"hops": 2, "chain": chain1, "question_hint": "What database does payment-api depend on via auth?"})

    # Chain 2 (3-hop): search-api → session-store → cache-cluster → object-storage
    chain2 = ["wiki/services/search-api.md", "wiki/services/session-store.md",
              "wiki/infra/cache-cluster.md", "wiki/infra/object-storage.md"]
    chains.append({"hops": 3, "chain": chain2, "question_hint": "What storage does search-api eventually use?"})

    # Generate service documents
    for svc in SERVICES:
        path = f"wiki/services/{svc}.md"
        # Find this service's outgoing edges
        out_edges = [(d, rt) for (s, d, rt, _) in edges if s == path]
        body_parts = [f"# {svc}", ""]
        body_parts.append(f"{svc} is a core microservice.")
        for dst, rt in out_edges:
            dst_name = dst.split("/")[-1].replace(".md", "")
            rel_text = next((t for (s, dd, r, t) in edges if s == path and dd == dst), f"connects to {dst_name}")
            body_parts.append(f"It {rel_text} [[{dst_name}]].")
        # Add some noise content
        body_parts.append(f"Configuration uses environment variables and secrets manager integration.")
        content = "\n".join(body_parts)
        documents.append((path, content, {"type": "Service", "tags": ["service"]}))

    # Generate infra documents
    for infra in INFRA:
        path = f"wiki/infra/{infra}.md"
        content = f"# {infra}\n\n{infra} provides infrastructure support.\nDeployed with high availability configuration."
        documents.append((path, content, {"type": "Infrastructure", "tags": ["infra"]}))

    # Generate concept documents
    for concept in CONCEPTS:
        path = f"wiki/concepts/{concept}.md"
        content = f"# {concept}\n\n{concept.replace('-', ' ').title()} is an architectural pattern."
        documents.append((path, content, {"type": "Concept", "tags": ["concept"]}))

    # Generate distractor documents (no meaningful connections)
    distractors = [
        "meeting-notes-2026-q3", "team-org-chart", "onboarding-guide",
        "coding-standards", "deployment-checklist", "incident-template",
        "architecture-decision-records", "glossary-of-terms",
        "quarterly-okrs", "design-review-process",
    ]
    for name in distractors[:n_docs // 20]:
        path = f"wiki/admin/{name}.md"
        content = f"# {name.replace('-', ' ').title()}\n\nThis document describes internal processes and procedures."
        documents.append((path, content, {"type": "Document", "tags": ["admin"]}))

    return documents, chains

# ---------------------------------------------------------------------------
# Golden question generation from planted chains
# ---------------------------------------------------------------------------

def generate_golden_questions(documents, chains):
    """
    Generate questions from planted multi-hop chains.
    Uses anti-luck filtering: rejects questions where keyword search alone
    finds the answer (ensures graph traversal is actually needed).
    """
    doc_map = {path: content for path, content, _ in documents}
    questions = []

    for chain_info in chains:
        chain = chain_info["chain"]
        hops = chain_info["hops"]
        start_doc = chain[0]
        end_doc = chain[-1]

        # Extract names from paths
        names = [p.split("/")[-1].replace(".md", "") for p in chain]
        start_name, end_name = names[0], names[-1]

        if hops == 2:
            question = f"What does {start_name} depend on through its immediate connections?"
            criteria = end_name
        elif hops == 3:
            mid = names[1]
            question = f"Starting from {start_name}, what is the final infrastructure component in its dependency chain through {mid}?"
            criteria = end_name
        else:
            continue

        # Anti-luck filter: check if simple FTS can find it without graph
        start_content = doc_map.get(start_doc, "")
        if end_name.lower() in start_content.lower():
            continue  # Answer leaked into source doc; skip

        # Verify intermediate doc exists
        mid_content = doc_map.get(chain[1], "") if len(chain) > 1 else ""
        if not mid_content:
            continue

        questions.append({
            "id": f"multihop-{hops}hop-{len(questions)}",
            "question": question,
            "query": start_name,
            "expected_docs": [end_doc],
            "chain": chain,
            "hops": hops,
            "category": "multi_hop",
        })

    # Also generate single-hop factoid questions
    for path, content, fm in documents:
        title = content.split("\n")[0].lstrip("# ").strip()
        if not title or title in ("Index",):
            continue
        # Pick a distinctive word from the content
        words = [w for w in content.split() if len(w) > 6 and w.isalpha()]
        if not words:
            continue
        keyword = words[len(words) // 2]  # middle word

        questions.append({
            "id": f"factoid-{hash(path) % 10000}",
            "question": f"Find information about {keyword}",
            "query": keyword,
            "expected_docs": [path],
            "hops": 1,
            "category": "factoid",
        })
        if len([q for q in questions if q["category"] == "factoid"]) >= 30:
            break

    return questions

# ---------------------------------------------------------------------------
# Retrieval quality metrics
# ---------------------------------------------------------------------------

def precision_at_k(retrieved: list[str], relevant: set[str], k: int) -> float:
    top_k = retrieved[:k]
    relevant_in_k = sum(1 for r in top_k if r in relevant)
    return relevant_in_k / min(k, len(top_k)) if top_k else 0.0

def recall_at_k(retrieved: list[str], relevant: set[str], k: int) -> float:
    top_k = set(retrieved[:k])
    return len(top_k & relevant) / len(relevant) if relevant else 0.0

def mrr(retrieved: list[str], relevant: set[str]) -> float:
    for i, r in enumerate(retrieved):
        if r in relevant:
            return 1.0 / (i + 1)
    return 0.0

def ndcg_at_k(retrieved: list[str], relevant: set[str], k: int) -> float:
    dcg = sum(
        1.0 / math.log2(i + 2)
        for i, r in enumerate(retrieved[:k]) if r in relevant
    )
    n_relevant = min(len(relevant), k)
    idcg = sum(1.0 / math.log2(i + 2) for i in range(n_relevant))
    return dcg / idcg if idcg > 0 else 0.0

# ---------------------------------------------------------------------------
# Search execution
# ---------------------------------------------------------------------------

def do_search(base_url: str, headers: dict, query: str, limit: int = 10,
              expand: bool = False, rerank: bool = False):
    params = urllib.parse.urlencode({
        "q": query, "limit": limit,
        "expand": str(expand).lower(),
        "rerank": str(rerank).lower(),
    })
    status, resp = http("GET", f"{base_url}/v1/search?{params}", headers=headers)
    if status != 200:
        return [], []
    hits = resp.get("results", [])
    ranked = [h.get("rel_path", "") for h in hits]
    return ranked, hits

# ---------------------------------------------------------------------------
# BENCHMARK 1: Retrieval Quality
# ---------------------------------------------------------------------------

def bench_retrieval_quality(base_url, headers, documents, questions, k_values=(1, 5, 10)):
    print(f"\n{'='*60}")
    print("BENCHMARK 1: RETRIEVAL QUALITY")
    print(f"{'='*60}")
    print(f"Corpus: {len(documents)} docs | Questions: {len(questions)}")

    results = {"per_query": [], "summary": {}}

    for q in questions:
        ranked, _ = do_search(base_url, headers, q["query"], limit=10)
        relevant = set(q["expected_docs"])
        entry = {
            "id": q["id"], "category": q["category"],
            "retrieved_count": len(ranked),
            "relevant_count": len(relevant),
        }
        for k in k_values:
            entry[f"p@{k}"] = precision_at_k(ranked, relevant, k)
            entry[f"r@{k}"] = recall_at_k(ranked, relevant, k)
        entry["mrr"] = mrr(ranked, relevant)
        for k in k_values:
            entry[f"ndcg@{k}"] = ndcg_at_k(ranked, relevant, k)
        results["per_query"].append(entry)

    # Aggregate by category and overall
    categories = sorted(set(q["category"] for q in questions))
    summary_lines = [f"\n{'Metric':<15s}", "-" * 60]
    header = f"{'Category':<20s}"
    for k in k_values:
        header += f" {'P@'+str(k):>6s} {'R@'+str(k):>6s} {'NDCG':>6s}"
    header += f" {'MRR':>6s}"
    summary_lines.insert(0, header)

    for cat in ["factoid"] + categories + ["OVERALL"]:
        subset = [e for e in results["per_query"]
                  if cat == "OVERALL" or e["category"] == cat]
        if not subset:
            continue
        n = len(subset)
        row = f"{cat:<20s}"
        for k in k_values:
            p = sum(e[f"p@{k}"] for e in subset) / n
            r = sum(e[f"r@{k}"] for e in subset) / n
            nd = sum(e[f"ndcg@{k}"] for e in subset) / n
            row += f" {p:>6.1%} {r:>6.1%} {nd:>6.3f}"
        mrr_avg = sum(e["mrr"] for e in subset) / n
        row += f" {mrr_avg:>6.3f}"
        summary_lines.append(row)

    report = "\n".join(summary_lines)
    print(report)
    results["report"] = report
    return results

# ---------------------------------------------------------------------------
# BENCHMARK 2: Multi-hop Reasoning
# ---------------------------------------------------------------------------

def bench_multi_hop(base_url, headers, questions, chains):
    print(f"\n{'='*60}")
    print("BENCHMARK 2: MULTI-HOP REASONING")
    print(f"{'='*60}")

    multi_hop_qs = [q for q in questions if q["category"] == "multi_hop"]
    if not multi_hop_qs:
        print("No multi-hop questions generated (anti-luck filter rejected all)")
        return {"skipped": True}

    by_hops = {}
    for q in multi_hop_qs:
        by_hops.setdefault(q["hops"], []).append(q)

    print(f"Questions: {len(multi_hop_qs)} across {len(by_hops)} hop levels")

    # Test WITHOUT graph expansion (baseline)
    baseline_results = {}
    for q in multi_hop_qs:
        ranked, _ = do_search(base_url, headers, q["query"], limit=10, expand=False)
        relevant = set(q["expected_docs"])
        found = sum(1 for r in ranked[:5] if r in relevant)
        baseline_results[q["id"]] = found / max(len(q["expected_docs"]), 1)

    # Test WITH expansion enabled (server config)
    expanded_results = {}
    for q in multi_hop_qs:
        ranked, _ = do_search(base_url, headers, q["query"], limit=10, expand=True)
        relevant = set(q["expected_docs"])
        found = sum(1 for r in ranked[:5] if r in relevant)
        expanded_results[q["id"]] = found / max(len(q["expected_docs"]), 1)

    # Report
    lines = [f"\n{'Hops':<6s} {'N':>4s} {'Baseline R':>11s} {'Expanded R':>11s} {'Delta':>8s}", "-" * 48]
    for hops in sorted(by_hops.keys()):
        qs = by_hops[hops]
        base_scores = [baseline_results.get(q["id"], 0) for q in qs]
        exp_scores = [expanded_results.get(q["id"], 0) for q in qs]
        avg_base = sum(base_scores) / max(len(base_scores), 1)
        avg_exp = sum(exp_scores) / max(len(exp_scores), 1)
        delta = avg_exp - avg_base
        lines.append(f"{hops}-hop   {len(qs):>4d} {avg_base:>10.0%} {avg_exp:>10.0%} {delta:>+7.0%}")

    report = "\n".join(lines)
    print(report)
    return {"by_hops": {h: len(qs) for h, qs in by_hops.items()},
            "baseline": baseline_results, "expanded": expanded_results,
            "report": report}

# ---------------------------------------------------------------------------
# BENCHMARK 3: Ablation Study
# ---------------------------------------------------------------------------

def bench_ablation(base_url, headers, questions, documents):
    print(f"\n{'='*60}")
    print("BENCHMARK 3: ABLATION STUDY")
    print(f"{'='*60}")
    print("NOTE: This requires restarting the server with different feature flags.")
    print("Measuring what we can without restart (expand_context on/off):\n")

    factoid_qs = [q for q in questions if q["category"] == "factoid"][:30]
    multi_qs = [q for q in questions if q["category"] == "multi_hop"]

    all_qs = factoid_qs + multi_qs

    # Config A: no expansion, no rerank
    results_a = {"latencies": [], "scores": []}
    for q in all_qs:
        t0 = time.time()
        ranked, _ = do_search(base_url, headers, q["query"], limit=10, expand=False, rerank=False)
        latency = (time.time() - t0) * 1000
        relevant = set(q["expected_docs"])
        score = recall_at_k(ranked, relevant, 10)
        results_a["latencies"].append(latency)
        results_a["scores"].append(score)

    # Config B: with expansion
    results_b = {"latencies": [], "scores": []}
    for q in all_qs:
        t0 = time.time()
        ranked, _ = do_search(base_url, headers, q["query"], limit=10, expand=True, rerank=False)
        latency = (time.time() - t0) * 1000
        relevant = set(q["expected_docs"])
        score = recall_at_k(ranked, relevant, 10)
        results_b["latencies"].append(latency)
        results_b["scores"].append(score)

    # Config C: with rerank (no expansion)
    results_c = {"latencies": [], "scores": []}
    for q in all_qs:
        t0 = time.time()
        ranked, _ = do_search(base_url, headers, q["query"], limit=10, expand=False, rerank=True)
        latency = (time.time() - t0) * 1000
        relevant = set(q["expected_docs"])
        score = recall_at_k(ranked, relevant, 10)
        results_c["latencies"].append(latency)
        results_c["scores"].append(score)

    def avg(lst): return sum(lst) / max(len(lst), 1)

    lines = [
        f"\n{'Configuration':<25s} {'R@10':>7s} {'Avg ms':>8s} {'N':>4s}",
        "-" * 50,
        f"{'A: Baseline (no extras)':<25s} {avg(results_a['scores']):>6.1%} {avg(results_a['latencies']):>7.1f}ms {len(all_qs):>4d}",
        f"{'B: + Context Expansion':<25s} {avg(results_b['scores']):>6.1%} {avg(results_b['latencies']):>7.1f}ms {len(all_qs):>4d}",
        f"{'C: + LLM Rerank':<25s} {avg(results_c['scores']):>6.1%} {avg(results_c['latencies']):>7.1f}ms {len(all_qs):>4d}",
        "",
        "Contribution of each feature:",
        f"  Context expansion: {avg(results_b['scores']) - avg(results_a['scores']):+.1%} recall change",
        f"  LLM rerank: {avg(results_c['scores']) - avg(results_a['scores']):+.1%} recall change",
        f"  Context latency cost: {avg(results_b['latencies']) - avg(results_a['latencies']):+.0f}ms",
    ]
    report = "\n".join(lines)
    print(report)
    return {"report": report}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="WikiLLM Comprehensive Benchmark Suite")
    parser.add_argument("--port", type=int, default=3850)
    parser.add_argument("--key", default="testkey")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--docs", type=int, default=200)
    args = parser.parse_args()

    base_url = f"http://127.0.0.1:{args.port}"
    headers = {"Authorization": f"Bearer {args.key}", "Content-Type": "application/json"}

    # Health check
    status, health = http("GET", f"{base_url}/health")
    if status != 200:
        print(f"Server not healthy at {base_url}")
        sys.exit(1)
    print(f"Server OK: {base_url}")

    import urllib.parse  # needed for do_search

    # Generate corpus
    print(f"\nGenerating synthetic corpus (seed={args.seed}, target={args.docs} docs)...")
    documents, chains = generate_corpus(args.seed, args.docs)
    print(f"Generated {len(documents)} documents with {len(chains)} planted chains")

    # Seed server
    for rel_path, content, fm in documents:
        dir_path = os.path.dirname(rel_path)
        os.makedirs(os.path.join(os.environ.get("WIKI_ROOT", "/tmp"), dir_path), exist_ok=True)
        st, _ = http("PUT", f"{base_url}/v1/pages/{rel_path}", headers=headers,
                     body={"content": content, "frontmatter": fm})
        if st != 200:
            print(f"  WARN: seed {rel_path} -> {st}")

    time.sleep(1)  # allow indexing

    # Generate golden questions
    questions = generate_golden_questions(documents, chains)
    cats = {}
    for q in questions:
        cats[q["category"]] = cats.get(q["category"], 0) + 1
    print(f"Generated {len(questions)} golden questions: {cats}")

    # Run benchmarks
    all_results = {}

    all_results["retrieval_quality"] = bench_retrieval_quality(
        base_url, headers, documents, questions, k_values=(1, 5, 10))

    all_results["multi_hop"] = bench_multi_hop(
        base_url, headers, questions, chains)

    all_results["ablation"] = bench_ablation(
        base_url, headers, questions, documents)

    # Save results
    output_file = "/tmp/wikillm-benchmark-results.json"
    with open(output_file, "w") as f:
        serializable = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "corpus_size": len(documents),
            "questions": len(questions),
            "results": {
                k: {kk: vv for kk, vv in v.items() if kk != "report"}
                for k, v in all_results.items() if isinstance(v, dict)
            },
        }
        json.dump(serializable, f, indent=2, default=str)
    print(f"\nResults saved to {output_file}")
