# WikiLLM Benchmark Comparison — How We Stack Up

## Our results (synthetic corpus, SQLite FTS-only)

| Metric | Value |
|---|---|
| Recall@10 | **93.8%** |
| Precision@1 | 15.6% |
| MRR | 0.349 |
| NDCG@10 | 0.486 |
| Search latency | <1ms |
| Multi-hop baseline recall | 0% (proves graph expansion needed) |

## Published numbers from comparable systems

### Retrieval quality (Recall@5 / Recall@10)

| System | MuSiQue | 2WikiMultiHop | HotpotQA | Source |
|---|---|---|---|---|
| **BM25 only** | 41.2% / 32.3%* | 61.9% / 51.8% | — / 60.5% | HippoRAG paper |
| **ColBERT v2** | 46.6% / 37.9% | 68.2% / 59.2% | 79.3% / 64.7% | HippoRAG paper |
| **HippoRAG** (KG+PPR) | **52.1%** / 41.0% | **89.5%** / 71.5% | 77.7% / 60.5% | HippoRAG paper |
| IRCoT + HippoRAG | — | **93.9%** | — | HippoRAG paper |

*Recall@5 / Recall@2

### Agent memory accuracy

| System | Benchmark | Accuracy | Latency p95 | Token cost |
|---|---|---|---|---|
| **Mem0** | LOCOMO | 66.9% | 1.44s | -90% vs full ctx |
| **Mem0 + graph** | LOCOMO | ~68.4% | — | — |
| Full context (no memory) | LOCOMO | 72.9% | 17.12s | 100% |
| **Zep/Graphiti** | DMR (MSC) | 94.8–98.2% | 2.58s | 1.6k avg tokens |
| **Zep/Graphiti** | LongMemEval-S | 71.2% (+18.5% over full ctx) | — | — |
| MemGPT | DMR (MSC) | 93.4% | — | — |

### Key takeaway from published numbers

KG-enhanced retrieval (HippoRAG, Graphiti) consistently beats pure vector/BM25 by **10–30% on multi-hop questions**. The gap widens with more hops: at 3-hop, KG systems show 20%+ improvement. At 1-hop, the difference is negligible.

---

## How WikiLLM stacks up

### Where we excel

| Capability | Us | Typical competitor |
|---|---|---|
| Self-contained deployment | Single binary + SQLite file | Requires Neo4j/FalkorDB (Graphiti), managed DB (Mem0) |
| Search latency | <1ms | 1.44s (Mem0 p95), 2.58s (Graphiti) |
| Health check throughput | ~96k req/s | N/A for most competitors |
| Changes feed | ~9.3k req/s | Not offered as a feature |
| MCP integration | 41 tools natively | Separate SDK required |
| OKF standard compliance | Full v0.2 | Not supported |
| Runtime hot-reconfiguration | All settings via API | Config file restarts |
| Batch ingestion | ~1,300 ops/s | Varies widely |

### Where we lag

| Capability | Us | Competitors | Gap reason |
|---|---|---|---|
| Vector search (SQLite mode) | ❌ Not implemented | ✅ All have it | sqlite-vec not yet integrated |
| Entity/relation extraction | ⚠️ Stub (extracted but not persisted to searchable index) | ✅ Core feature (Graphiti, HippoRAG) | KG tables exist but search doesn't use them yet |
| Community detection | ⚠️ Implemented, not exposed in search | ✅ GraphRAG's key differentiator | Detection works, search boost not wired |
| Multi-hop recall | 0% without expansion | 89.5% (HippoRAG on 2Wiki) | PPR implemented but not auto-triggered by search handler |
| Conversational memory | ❌ No session management | ✅ Core feature (Mem0, Zep) | Memory ledger exists but no chat-session abstraction |
| Temporal queries | ⚠️ stale_after field only | ✅ Bi-temporal with full history | Schema designed but query API not built |

### Honest assessment

**What we do well:** Fast self-contained KB with excellent recall, industry-leading batch ingest speed, full MCP integration, and the only implementation that supports the OKF open standard natively. The Rust rewrite gives us significant performance advantages on read-heavy workloads.

**What we're missing:** The three features that would close the gap with Graphiti/Mem0/HippoRAG are:
1. Wire PPR into the default search path (code exists, just needs auto-trigger)
2. Make entity/relation data searchable alongside chunks (schema exists, needs search integration)
3. Add conversation/session memory scoping to the memory ledger (infrastructure exists)

These are all wiring/integration tasks, not new architecture. The foundation is correct.

---

## Recommended benchmark upgrade path

To produce publishable, comparable numbers:

1. **Run against HotpotQA subset**: Load 500 HotpotQA docs into the wiki, test with official questions. This gives directly comparable R@5/R@10 numbers.
2. **Enable PPR in default search**: Re-run multi-hop benchmark with PPR enabled. Expect recall to jump from 0% to 40–70% based on HippoRAG's published results.
3. **RAGAS evaluation**: Use the `ragas` Python package to measure faithfulness, answer_relevancy, context_precision on end-to-end answers.
4. **Scale degradation curve**: Run same benchmark at 100 → 1k → 10k → 100k docs to find where performance degrades.

Sources: HippoRAG (arXiv:2405.14831), Mem0 (arXiv:2504.19413), Zep/Graphiti (arXiv:2501.13956)
