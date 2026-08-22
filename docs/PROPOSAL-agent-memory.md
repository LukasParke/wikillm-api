# Proposal: 10 Ways to Make WikiLLM the Best Agent Memory System

Research synthesis from Graphiti, Mem0, Letta/MemGPT, HippoRAG-2, GraphRAG,
RAPTOR, CRAG/Self-RAG, Voyager, CoALA, and analysis of the current Rust
codebase gaps. Ordered by impact-per-effort; each proposal notes which
inspiration it draws from and which others it composes with.

---

## 1. Temporal Knowledge Graph with Entity Extraction

**From:** Graphiti (Zep) | **Effort:** Medium-High | **Composes with:** #3, #7, #10

Replace the current `edges` table (simple src→dst pairs) with a bi-temporal
knowledge graph that captures *what* the wiki knows about *entities* and *when*
that knowledge was true.

### Current limitation

The system stores document-level links but discards the semantic content of
those links. An agent asking "who depends on service X?" gets no answer because
there are no typed relationships between entities — only "page A links to page B."

### Design

```rust
// New table: entities (extracted from headings, [[links]], frontmatter.type)
pub struct Entity {
    id: Uuid,
    name: String,           // canonical name, deduplicated
    entity_type: String,    // "Company", "Service", "Person", "Concept"
    embedding: Vec<f32>,    // for fuzzy name resolution
    first_seen: DateTime<Utc>,
    summary: Option<String>, // LLM-generated when entity is hub-like
}

// Replaces `edges`: bi-temporal relationship edges
pub struct RelationEdge {
    id: Uuid,
    src_entity: Uuid,
    dst_entity: Uuid,
    relation_type: String,     // "DEPENDS_ON", "MENTIONS", "PART_OF", etc.
    fact: String,              // searchable natural language: "X depends on Y"
    fact_embedding: Vec<f32>,  // embed the fact text for semantic search
    source_doc: String,        // provenance: which document asserted this
    valid_at: Option<DateTime<Utc>>,   // when the fact became true
    invalid_at: Option<DateTime<Utc>>, // when it stopped being true (NULL = current)
    expired_at: Option<DateTime<Utc>>, // when we learned it was superseded
}
```

### Pipeline

On every document write (already hooked into `IndexPipeline`):
1. Extract entities: heading text, `[[wikilinks]]`, frontmatter `type`,
   code identifiers (regex), proper nouns via cheap NER (no LLM needed for wiki)
2. Resolve entities against existing set (fuzzy match on embedding similarity)
3. Extract relations: co-occurrence within same section + wikilink targets +
   optional LLM call for richer typing when configured
4. Write edges with bi-temporal stamps; supersede conflicting edges by setting
   `expired_at` (never delete)

### Why agents care

An agent can ask "what did we know about service X last month?" and get exact
temporal answers. Contradictions between documents don't break the graph — they
coexist as temporal facts. This is the single highest-leverage change because
it transforms the system from a document store into a *knowledge base*.

---

## 2. Agent Memory Ledger (Two-Phase Consolidation)

**From:** Mem0 | **Effort:** Medium | **Composes with:** #1, #9

Add a parallel `memories` table alongside documents for conversation-derived
facts, preferences, and procedural knowledge — things an agent learns that
don't belong in the curated wiki.

### Design

```rust
pub struct AgentMemory {
    id: Uuid,
    scope: MemoryScope,          // { user_id, agent_name, session_id? }
    memory_type: MemoryType,     // Semantic | Episodic | Procedural
    content: String,             // extracted fact/preference/procedure
    embedding: Vec<f32>,
    content_hash: String,        // normalized MD5 for fast dedup
    source_episode: String,      // raw input that produced this memory
    created_at: DateTime<Utc>,
    accessed_at: DateTime<Utc>,  // last retrieval (reinforcement signal)
    access_count: u32,           // retrieval frequency
}

pub enum MemoryType {
    Semantic,   // "User prefers PostgreSQL over MySQL"
    Episodic,   // "During deploy on 2026-08-20, the migration failed"
    Procedural, // "To restart nginx: sudo systemctl restart nginx"
}
```

### Two-phase ingestion

When an agent stores a new memory:
1. **Dedup gate**: normalize (lowercase, strip punctuation) → hash → check
   existing hashes in scope. Exact duplicates skip LLM entirely.
2. **Consolidation**: embed new memory → retrieve top-K scoped candidates by
   cosine similarity → single LLM call classifies each pair as
   `ADD` (new fact), `UPDATE` (enrichment, keep ID), `DELETE` (contradiction),
   or `NOOP` (equivalent). Apply decisions atomically.
3. **History**: append `(memory_id, old_content, action, ts)` to an audit
   ledger for recoverability.

### Why agents care

Agents accumulate knowledge across sessions instead of starting cold. The
system remembers "this user prefers concise answers" without being told again.
Contradictions resolve automatically rather than accumulating stale beliefs.

---

## 3. Personalized PageRank Retrieval (Multi-Hop)

**From:** HippoRAG-2 | **Effort:** Very Low (~50 lines) | **Composes with:** #1

The link graph already exists (`edges` table). Use it for multi-hop retrieval:
seed a Personalized PageRank walk from top hybrid-search hits, then return
graph-connected documents that pure similarity misses.

```rust
pub async fn ppr_expand(
    &self,
    seed_hits: &[ChunkHit],
    store: &Arc<dyn Store>,
    damping: f64,       // default 0.85
    iterations: usize,  // default 10
) -> Vec<(String, f64)> {
    // Build adjacency from edges table
    // Initialize scores from seed hit scores
    // Iterate PPR until convergence
    // Return top non-seed documents
}
```

This costs ~50 lines and immediately enables multi-hop questions like
"what services depend on the database that failed during the last deploy?"
— queries that zero-shot cosine search cannot answer.

---

## 4. Corrective Retrieval (CRAG)

**From:** CRAG / Self-RAG | **Effort:** Low | **Composes with:** #5

Before returning search results, grade their relevance to the query using a
cheap LLM call. If graded below threshold, trigger corrective actions:
query rewriting, broader filters, or fallback to full-text-only.

```rust
enum RetrievalGrade { Correct, Ambiguous, Incorrect }

async fn correct_and_retry(&self, query: &str, results: &mut SearchResult) {
    let grade = self.grade_retrieval(query, results).await;
    match grade {
        Correct => {},  // proceed
        Ambiguous => { results.expand_context(); }  // add neighbors
        Incorrect => {
            let rewritten = self.rewrite_query(query).await;
            *results = self.search(rewritten, ...).await;
        }
    }
}
```

Prevents agents from building on irrelevant evidence — the #1 cause of
hallucination in RAG systems.

---

## 5. Multi-Slot LLM Configuration

**From:** Production practice | **Effort:** Low | **Unblocks:** #4, all LLM features

Currently one `SharedLlm` serves chat AND embeddings AND rerank AND distill
AND synthesis. Split into four independently configurable slots:

```
EMBEDDING_PROVIDER=onnx          EMBEDDING_MODEL=Xenova/bge-small-en-v1.5
RERANK_PROVIDER=openai           RERANK_MODEL=gpt-4o-mini    # cheap + fast
SYNTHESIS_PROVIDER=cerebras      SYNTHESIS_MODEL=llama3.1-70b # quality
DISTILL_PROVIDER=ollama          DISTILL_MODEL=phi3           # local + free
```

Each slot hot-swappable independently via settings API. A cheap model handles
rerank while an expensive model handles synthesis — matching production RAG
patterns where rerank latency dominates.

---

## 6. RAPTOR Recursive Summarization Tree

**From:** RAPTOR paper | **Effort:** Medium | **Composes with:** #1

Build a hierarchical summarization tree over chunks: leaf level = original
chunks, level 1 = cluster summaries, level 2 = section summaries, root =
document summary. Store each level as additional searchable chunks.

At retrieval time, the tree enables answering both specific questions (leaf
level) and broad questions ("what is this project about?" → root summary)
without needing the entire document in context.

Implementation: after indexing, run bottom-up clustering (by embedding
similarity within sections) → summarize each cluster → recurse until < 3 items.
Store as `chunks` rows with `tree_level` column; search searches all levels.

---

## 7. Adaptive Retrieval Gate

**From:** Self-RAG, adaptive retrieval papers | **Effort:** Low

Before running retrieval, a lightweight LLM classification decides:
1. Does this query need retrieval at all? (greeting/calculation → skip)
2. What type of query is it? (lookup → FTS-first, conceptual → vector-first,
   multi-hop → PPR + graph, temporal → filter by date)
3. How many rounds? (simple → 1, complex → iterative refinement)

Saves latency on trivial queries and improves quality on complex ones by
routing to the right retrieval strategy.

---

## 8. SQLite Vector Search (sqlite-vec)

**From:** sqlite-vec project | **Effort:** Medium | **Unblocks:** hybrid search everywhere

The SQLite backend currently has no vector search (`supports_vector() → false`),
meaning the majority of self-hosted deployments run FTS-only. Add `sqlite-vec`
(a zero-dependency SQLite extension) for brute-force KNN vector search.

```rust
// After loading the extension:
conn.execute_batch("SELECT vec0_load_extension('sqlite-vec')")?;
// Create virtual table
conn.execute("CREATE VIRTUAL TABLE embeddings USING vec0(chunk_id TEXT, embedding float[384])", [])?;
// Search
let results = conn.prepare("SELECT chunk_id, distance FROM embeddings WHERE embedding MATCH ? AND k = ?")?;
```

Combined with existing FTS5, this gives full hybrid search in embedded mode —
no Postgres required.

---

## 9. Forgetting Curve + Reinforcement Learning

**From:** Mem0, cognitive science | **Effort:** Low | **Composes with:** #2

Implement a scoring function that decays unretrieved memories and reinforces
frequently-accessed ones:

```rust
fn relevance_score(memory: &AgentMemory, now: DateTime<Utc>) -> f64 {
    let age_days = (now - memory.created_at).num_days() as f64;
    let recency_decay = (-age_days / 30.0).exp();  // e^-1 at 30 days
    let reinforcement = (memory.access_count as f64).ln().max(0.0); // log boost
    recency_decay * (1.0 + reinforcement)
}

// At retrieval time: multiply hybrid score by relevance_score
// Periodic cleanup: archive memories with score < threshold after 90 days
```

This mirrors human forgetting curves: frequently-used knowledge stays sharp;
stale knowledge fades unless reinforced. Prevents the memory from becoming
polluted with outdated facts that degrade retrieval precision over time.

---

## 10. Community Detection + Topic Browsing

**From:** Graphiti, GraphRAG | **Effort:** Medium | **Composes with:** #1, #3

Run community detection (Louvain or label propagation) on the entity/relation
graph to discover natural topic clusters. Expose:

```
GET /v1/communities           → list detected communities with labels
GET /v1/communities/:id/docs  → documents in this community
GET /v1/communities/:id/summary → LLM-generated community description
```

At search time, boost results from the same community as top hits (topical
coherence). At browse time, agents can explore the knowledge base by topic
rather than by file path. Communities auto-update as new documents arrive.

---

## Implementation Priority

| Priority | Proposal | Effort | Impact on agent performance |
|---|---|---|---|
| 1 | #5 Multi-slot LLM | Low | Unblocks all LLM features |
| 2 | #3 PageRank retrieval | ~50 lines | Multi-hop questions work |
| 3 | #4 Corrective retrieval | Low | Eliminates hallucination from bad retrieval |
| 4 | #2 Agent memory ledger | Medium | Agents learn across sessions |
| 5 | #1 Temporal knowledge graph | Med-High | Transforms doc store into knowledge base |
| 6 | #9 Forgetting curve | Low | Keeps memory relevant over time |
| 7 | #8 SQLite vector search | Medium | Full hybrid search everywhere |
| 8 | #7 Adaptive retrieval gate | Low | Saves latency on easy queries |
| 9 | #10 Community detection | Medium | Topic-aware browsing and boosting |
| 10 | #6 RAPTOR summarization | Medium | Better broad-question answering |

Proposals 1-3 can ship independently. Proposals 4-5 benefit from having 3
already. Proposals 6-10 compose naturally once the foundation exists.
