#!/usr/bin/env python3
"""
WikiLLM — Memory Abilities Eval Harness (OWNER-EVAL).
Evaluates five agent-memory abilities against a running WikiLLM API:
  fact_recall, preference_following, procedural_recall, latest_state, abstention.
Protocol per ability: seed k memories via POST /v1/memory tagged with a fresh run
uuid, probe with GET /v1/search + POST /v1/query, LLM-judge answer nuggets on a
0 / 0.5 / 1 scale, report per-ability mean score + p50 latency.
Markdown report prints to stdout and appends a dated section to
scripts/memory-eval-results.md. Stdlib only (urllib); the optional judge LLM is
any OpenAI-compatible POST /chat/completions endpoint — without one, a lexical
fallback scorer keeps the harness runnable.

Usage:
    python3 scripts/memory_eval.py [--base http://127.0.0.1:3860] [--key KEY]
        [--judge-url URL] [--judge-model M] [--judge-key K] [--label NAME]
        [--no-pages]
Env fallbacks: MEMORY_EVAL_BASE, MEMORY_EVAL_API_KEY, MEMORY_EVAL_JUDGE_URL,
    MEMORY_EVAL_JUDGE_KEY, MEMORY_EVAL_JUDGE_MODEL.
"""

import argparse, json, os, re, sys, time, uuid
import urllib.request, urllib.error, urllib.parse

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
        resp = urllib.request.urlopen(req, data=data, timeout=60)
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

def p50(values):
    return round(sorted(values)[len(values) // 2], 2) if values else None

# ---------------------------------------------------------------------------
# Nugget judge — LLM when configured, lexical fallback otherwise
# ---------------------------------------------------------------------------
JUDGE_SYSTEM = ('You are a strict evaluation judge. Reply ONLY with minified '
                'JSON: {"score": 0|0.5|1, "reason": "<max 10 words>"}.')

JUDGE_PROMPT = """Question asked to a knowledge-base assistant:
{question}

Reference nugget (what a correct answer must convey):
{nugget}

Assistant answer (with retrieved passages):
---
{answer}
---

Score the answer:
- 1: conveys the nugget fully and correctly.
- 0.5: right topic but incomplete, vague, or hedged between correct and incorrect.
- 0: nugget absent, contradicted, or a wrong/outdated value presented as current.
For "current/latest" questions the NEWER statement wins; giving the older value \
as current is 0.
Refusing for lack of information scores 0 unless the question is declared \
unanswerable.
Reply ONLY: {{"score": 0|0.5|1, "reason": "..."}}"""

REFUSAL_MARKERS = ("not answerable", "cannot answer", "can't answer", "don't know",
                   "do not know", "no information", "not enough information",
                   "couldn't find", "could not find", "no relevant")

def _tokens(text):
    return [t for t in re.split(r"[^a-z0-9]+", text.lower()) if len(t) > 2]

def lexical_score(nugget, answer):
    want = _tokens(nugget)
    have = " ".join(_tokens(answer))
    ratio = sum(1 for t in want if t in have) / max(len(want), 1)
    return 1.0 if ratio >= 0.99 else (0.5 if ratio >= 0.5 else 0.0)

def judge_answer(cfg, question, nugget, answer):
    if cfg.judge_url:
        prompt = JUDGE_PROMPT.format(question=question, nugget=nugget, answer=answer[:4000])
        st, resp = http("POST", cfg.judge_url, headers={
            "Authorization": f"Bearer {cfg.judge_key}", "Content-Type": "application/json"},
            body={"model": cfg.judge_model, "temperature": 0, "max_tokens": 60,
                  "messages": [{"role": "system", "content": JUDGE_SYSTEM},
                               {"role": "user", "content": prompt}]})
        text = ""
        if st == 200 and resp.get("choices"):
            text = resp["choices"][0].get("message", {}).get("content", "")
        m = re.search(r"\{.*\}", text, re.S)
        if m:
            try:
                return max(0.0, min(1.0, float(json.loads(m.group(0))["score"])))
            except Exception:
                pass
    return lexical_score(nugget, answer)

def judge_abstention(answer):
    if not answer.strip():
        return 0.0
    low = answer.lower()
    return 1.0 if any(m in low for m in REFUSAL_MARKERS) else 0.0

# ---------------------------------------------------------------------------
# Ability fixtures: (name, [(memory_type, seed_content)], [(question, nugget)])
# ---------------------------------------------------------------------------
ABILITIES = [
    ("fact_recall", [
        ("semantic", "The payment-api service processes card transactions through Stripe."),
        ("semantic", "The wikillm auth-service signs users in by issuing JWT access tokens."),
        ("semantic", "The metrics pipeline ships service counters to a Prometheus push gateway."),
    ], [
        ("Which payment provider does the payment-api use for card transactions?",
         "payment-api processes card transactions through Stripe"),
        ("How does the auth-service sign users in?",
         "auth-service signs users in by issuing JWT access tokens"),
    ]),
    ("preference_following", [
        ("preference", "Team preference: always use PostgreSQL for new services; never MySQL."),
        ("preference", "Team preference: write runbooks in second-person imperative mood."),
    ], [
        ("We are starting a new service tomorrow. Which database engine should we pick?",
         "new services should use PostgreSQL, not MySQL"),
        ("Which database engine does the team prefer for new services?",
         "PostgreSQL instead of MySQL"),
    ]),
    ("procedural_recall", [
        ("procedural", "To rotate the JWT signing keys: generate a new RS256 keypair, "
                       "publish it to the JWKS endpoint, restart auth-service, and keep "
                       "the old key valid for verification for 24 hours."),
    ], [
        ("How do we rotate the JWT signing keys?",
         "generate a new RS256 keypair, publish it to JWKS, restart auth-service, "
         "keep the old key valid for 24 hours"),
    ]),
    ("latest_state", [
        ("semantic", "As of March, the on-call engineer for search-api is Alice Johnson."),
        ("semantic", "Rotation update: the on-call engineer for search-api is now "
                     "Bob Martinez; Alice Johnson rotated off the rotation."),
    ], [
        ("Who is the current on-call engineer for search-api?",
         "Bob Martinez is the current on-call engineer for search-api"),
    ]),
]

ABSTENTION_PROBES = [
    "What is the warranty period for the Neptune-9 coffee machine in the office kitchen?",
    "What venue did the team book for the offsite on the fictional island of Meridia?",
]

# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------
class Eval:
    def __init__(self, cfg):
        self.cfg = cfg
        self.base = cfg.base
        self.H = {"Authorization": f"Bearer {cfg.key}", "Content-Type": "application/json"}
        self.run_id = uuid.uuid4().hex[:8]

    def seed_memory(self, content, mem_type, ability):
        body = {"content": content, "memory_type": mem_type,
                "source_ref": f"eval:{self.run_id}:{ability}"}
        return http("POST", f"{self.base}/v1/memory", headers=self.H, body=body)

    def seed_page(self, slug, title, content):
        rel = f"wiki/eval/{self.run_id}/{slug}.md"
        body = {"content": f"# {title}\n\n{content}\n",
                "frontmatter": {"type": "Note", "tags": ["memory-eval"]}}
        return http("PUT", f"{self.base}/v1/pages/{rel}", headers=self.H, body=body)

    def probe(self, question):
        """GET /v1/search + POST /v1/query -> (context_text, search_ms, query_ms, body)."""
        q = urllib.parse.quote(question)
        (_, sresp), s_ms = timed(lambda: http(
            "GET", f"{self.base}/v1/search?q={q}&limit=5", headers=self.H))
        (qst, qresp), q_ms = timed(lambda: http(
            "POST", f"{self.base}/v1/query", headers=self.H, body={"question": question}))
        snippets = "\n".join(
            f"- {h.get('rel_path', '')}: {h.get('snippet', '')}"
            for h in (sresp.get("results", []) if isinstance(sresp, dict) else [])[:3])
        answer = qresp.get("answer", "") if isinstance(qresp, dict) else ""
        if isinstance(qresp, dict) and qresp.get("abstained"):
            answer = answer or "Not answerable from this knowledge base."
        ctx = f"{answer}\n\nRetrieved passages:\n{snippets}".strip()
        return ctx, s_ms, q_ms, qst, qresp

    def run_ability(self, name, seeds, probes, abstain=False):
        rows = []
        for i, (mem_type, content) in enumerate(seeds):
            st, resp = self.seed_memory(content, mem_type, name)
            if st not in (200, 201):
                print(f"  WARN: seed {name}[{i}] -> {st} {json.dumps(resp)[:120]}")
            if self.cfg.pages:
                st, resp = self.seed_page(f"{name}-{i}", f"{name} seed {i}", content)
                if st not in (200, 201):
                    print(f"  WARN: page seed {name}[{i}] -> {st}")
        time.sleep(0.5)  # indexing settle
        for question, nugget in probes:
            ctx, s_ms, q_ms, qst, qresp = self.probe(question)
            if qst == 503:  # llm_not_configured — synthesis unavailable
                rows.append({"question": question, "score": None, "note": "skipped: LLM not configured",
                             "search_ms": round(s_ms, 2), "query_ms": round(q_ms, 2)})
            score = (judge_abstention(ctx) if abstain
                     else judge_answer(self.cfg, question, nugget, ctx))
            rows.append({"question": question, "nugget": nugget, "score": score,
                         "search_ms": round(s_ms, 2), "query_ms": round(q_ms, 2)})
        scored = [r["score"] for r in rows if r["score"] is not None]
        return {"name": name, "rows": rows, "mean": round(sum(scored) / len(scored), 3) if scored else None,
                "skipped": sum(1 for r in rows if r["score"] is None),
                "p50_search_ms": p50([r["search_ms"] for r in rows]),
                "p50_query_ms": p50([r["query_ms"] for r in rows])}

    def run(self):
        results = []
        for name, seeds, probes in ABILITIES:
            print(f"[eval] {name}...")
            results.append(self.run_ability(name, seeds, probes))
        print("[eval] abstention...")
        results.append(self.run_ability(
            "abstention", [], [(q, None) for q in ABSTENTION_PROBES], abstain=True))
        return results

# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------
def render(results, cfg, run_id):
    lines = [f"# Memory Eval — {cfg.label} ({time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())})",
             "",
             f"- Base: `{cfg.base}` | run: `{run_id}`",
             f"- Judge: {'LLM ' + cfg.judge_model if cfg.judge_url else 'lexical fallback'}"
             f" | page mirror: {'on' if cfg.pages else 'off'}",
             "",
             "| ability | probes | mean score | p50 search ms | p50 query ms | skipped |",
             "|---|---|---|---|---|---|"]
    means = []
    for r in results:
        if r["mean"] is not None:
            means.append(r["mean"])
        lines.append(f"| {r['name']} | {len(r['rows'])} | {r['mean'] if r['mean'] is not None else 'n/a'} "
                     f"| {r['p50_search_ms']} | {r['p50_query_ms']} | {r['skipped']} |")
    overall = round(sum(means) / len(means), 3) if means else None
    lines.append(f"| **overall** | {sum(len(r['rows']) for r in results)} | {overall if overall is not None else 'n/a'} | | | |")
    lines += ["", "## Probe detail", ""]
    for r in results:
        lines.append(f"### {r['name']} (mean {r['mean'] if r['mean'] is not None else 'n/a'})")
        for row in r["rows"]:
            note = f" — {row['note']}" if "note" in row else ""
            nug = f"; nugget: {row['nugget']}" if row.get("nugget") else ""
            lines.append(f"- [{row['score']}] {row['question']}{nug}{note}"
                         f" (search {row['search_ms']}ms, query {row['query_ms']}ms)")
        lines.append("")
    return "\n".join(lines)

def main():
    parser = argparse.ArgumentParser(description="WikiLLM memory abilities eval")
    parser.add_argument("--port", type=int, default=None)
    parser.add_argument("--base", default=os.environ.get("MEMORY_EVAL_BASE", ""))
    parser.add_argument("--key", default=os.environ.get("MEMORY_EVAL_API_KEY", "bench:benchkey"))
    parser.add_argument("--judge-url", default=os.environ.get("MEMORY_EVAL_JUDGE_URL", ""))
    parser.add_argument("--judge-model", default=os.environ.get("MEMORY_EVAL_JUDGE_MODEL", "default"))
    parser.add_argument("--judge-key", default=os.environ.get("MEMORY_EVAL_JUDGE_KEY", ""))
    parser.add_argument("--label", default="manual")
    parser.add_argument("--no-pages", action="store_true",
                        help="skip mirroring seeds as wiki pages (search/query probes need them today)")
    args = parser.parse_args()

    base = args.base or f"http://127.0.0.1:{args.port or 3860}"
    key = args.key.split(":")[-1]  # use last segment as bearer
    args.base, args.key = base.rstrip("/"), key
    args.pages = not args.no_pages
    if args.judge_key == "":
        args.judge_key = key

    st, _ = http("GET", f"{args.base}/health")
    if st != 200:
        print(f"Server not healthy: {st}"); sys.exit(1)
    print(f"Server OK on {args.base}")

    ev = Eval(args)
    results = ev.run()
    report = render(results, args, ev.run_id)
    print(report)
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory-eval-results.md")
    with open(out, "a") as f:
        f.write("\n" + report + "\n")
    print(f"\nReport appended to {out}")

if __name__ == "__main__":
    main()
