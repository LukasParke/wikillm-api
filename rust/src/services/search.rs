//! Retrieval service, ported from TypeScript `src/services/searchService.ts`.
//!
//! FTS + optional vector search fused with Reciprocal Rank Fusion, decay and
//! title boosts, optional LLM rerank, and neighbor-chunk context expansion.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde::Serialize;

use crate::domain::{trust_tier, ChunkHit, SearchFilters};
use crate::error::{Error, Result};
use crate::llm::provider::{ChatMessage, DynLlmProvider};
use crate::store::Store;

/// Hot-swappable LLM holder owned by the container.
pub type SharedLlm = Arc<RwLock<Option<DynLlmProvider>>>;

const DAY_MS: f64 = 86_400_000.0;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHitContext {
    pub ordinal: i64,
    pub heading_path: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub rel_path: String,
    pub title: Option<String>,
    pub kind: String,
    pub origin: String,
    pub okf_type: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub stale_after: Option<String>,
    pub trust: &'static str,
    pub hash: String,
    pub mtime: i64,
    pub heading_path: Option<String>,
    pub snippet: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<SearchHitContext>>,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub q: String,
    pub limit: usize,
    pub filters: Option<SearchFilters>,
    /// disable LLM rerank for this call
    pub rerank: bool,
    /// attach neighboring chunks for winners
    pub expand_context: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            q: String::new(),
            limit: 20,
            filters: None,
            rerank: true,
            expand_context: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub results: Vec<SearchHit>,
    /// "hybrid" | "fts"
    pub mode: String,
    pub latency_ms: f64,
}

/// Reciprocal Rank Fusion over ranked key lists (Cerebras/SIGIR K=60 recipe).
/// Contribution of rank `r` (0-based) is `1 / (k + r + 1)`; ties keep
/// first-seen order.
pub fn rrf_fuse(lists: &[Vec<String>], k: i64) -> Vec<(String, f64)> {
    let mut order: Vec<(String, f64)> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for list in lists {
        for (rank, key) in list.iter().enumerate() {
            let contribution = 1.0 / (k as f64 + rank as f64 + 1.0);
            match positions.get(key) {
                Some(&index) => order[index].1 += contribution,
                None => {
                    positions.insert(key.clone(), order.len());
                    order.push((key.clone(), contribution));
                }
            }
        }
    }
    order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    order
}

/// Age-decay factor: 1.0 today, e^-1 at 30 days; future-dated clamps to 1.0.
pub fn recency_boost(mtime_ms: i64, now_ms: i64) -> f64 {
    let age_days = ((now_ms - mtime_ms) as f64 / DAY_MS).max(0.0);
    (-age_days / 30.0).exp()
}

const RERANK_PROMPT: &str = "You are a search result reranker. \
Rate each document's relevance to the query on a scale of 0-10. \
Respond with ONLY a JSON array of numbers, one per document, in order.";

struct Scored {
    hit: ChunkHit,
    score: f64,
}

pub struct SearchService {
    store: Arc<dyn Store>,
    llm: SharedLlm,
}

impl SearchService {
    pub fn new(store: Arc<dyn Store>, llm: SharedLlm) -> Self {
        SearchService { store, llm }
    }

    pub async fn search(&self, opts: SearchOptions) -> Result<SearchResult> {
        let started = Instant::now();
        let candidate_depth = (opts.limit * 4).max(40) as i64;

        // Read the provider handle exactly once per call; the lock is never
        // held across an await point (we clone the Arc out).
        let llm: Option<DynLlmProvider> = match self.llm.read() {
            Ok(guard) => guard.clone(),
            Err(_) => return Err(Error::Other("llm provider lock poisoned".into())),
        };

        let fts_hits = self
            .store
            .search_fts(&opts.q, candidate_depth, opts.filters.as_ref())
            .await?;

        let mut vector_hits: Vec<ChunkHit> = Vec::new();
        if let Some(provider) = &llm {
            if provider.embed_model().is_some() && self.store.supports_vector() {
                // Embedding failure degrades silently to FTS-only.
                if let Ok(vectors) = provider.embed(std::slice::from_ref(&opts.q)).await {
                    if let Some(vector) = vectors.first() {
                        vector_hits = self
                            .store
                            .search_vector(vector, candidate_depth, opts.filters.as_ref())
                            .await
                            .unwrap_or_default();
                    }
                }
            }
        }

        let mut by_id: HashMap<String, ChunkHit> = HashMap::new();
        for hit in fts_hits.iter().chain(vector_hits.iter()) {
            by_id.insert(hit.chunk_id.clone(), hit.clone());
        }

        let fused = rrf_fuse(
            &[
                fts_hits.iter().map(|h| h.chunk_id.clone()).collect(),
                vector_hits.iter().map(|h| h.chunk_id.clone()).collect(),
            ],
            60,
        );

        let lower_q = opts.q.to_lowercase();
        let now_ms = now_millis();
        let scored: Vec<Scored> = fused
            .into_iter()
            .filter_map(|(key, score)| {
                by_id.remove(&key).map(|hit| {
                    let decay = 1.0 + 0.15 * recency_boost(hit.mtime, now_ms);
                    let title_bonus = match &hit.title {
                        Some(title) if title.to_lowercase().contains(&lower_q) => 0.05,
                        _ => 0.0,
                    };
                    Scored {
                        hit,
                        score: score * decay + title_bonus,
                    }
                })
            })
            .take(60)
            .collect();

        let final_order =
            maybe_rerank(&opts.q, llm.as_ref(), scored, opts.rerank).await;

        let winners: Vec<Scored> = final_order.into_iter().take(opts.limit).collect();

        let mut results = Vec::with_capacity(winners.len());
        for winner in &winners {
            let expanded = if opts.expand_context {
                self.expand_context(winner).await?
            } else {
                None
            };
            results.push(to_search_hit(winner, expanded));
        }

        Ok(SearchResult {
            results,
            mode: if !vector_hits.is_empty() {
                "hybrid".to_string()
            } else {
                "fts".to_string()
            },
            latency_ms: started.elapsed().as_millis() as f64,
        })
    }

    async fn expand_context(&self, scored: &Scored) -> Result<Option<Vec<SearchHitContext>>> {
        let chunks = self.store.get_chunks_for_document(&scored.hit.document_id).await?;
        let index = match chunks.iter().position(|c| c.id == scored.hit.chunk_id) {
            Some(index) => index,
            None => return Ok(None),
        };
        if chunks.len() < 2 {
            return Ok(None);
        }
        let mut context = Vec::new();
        for offset in [index.checked_sub(1), Some(index + 1)] {
            if let Some(Some(chunk)) = offset.map(|i| chunks.get(i)) {
                context.push(SearchHitContext {
                    ordinal: chunk.ordinal,
                    heading_path: chunk.heading_path.clone(),
                    content: chunk.content.clone(),
                });
            }
        }
        if context.is_empty() {
            Ok(None)
        } else {
            Ok(Some(context))
        }
    }
}

async fn maybe_rerank(
    q: &str,
    llm: Option<&DynLlmProvider>,
    candidates: Vec<Scored>,
    enabled: bool,
) -> Vec<Scored> {
    let Some(llm) = llm else {
        return candidates;
    };
    if !enabled || candidates.len() < 2 {
        return candidates;
    }
    let shortlist: Vec<Scored> = candidates.into_iter().take(20).collect();
    let docs = shortlist
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "[{i}] {}{}\n{}",
                c.hit.title.clone().unwrap_or_else(|| c.hit.rel_path.clone()),
                match &c.hit.heading_path {
                    Some(hp) => format!(" :: {hp}"),
                    None => String::new(),
                },
                truncate_chars(&c.hit.content, 500)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let messages: [ChatMessage<'_>; 2] = [
        ("system", RERANK_PROMPT),
        ("user", &format!("Query: {q}\n\n{docs}")),
    ];
    let raw = match llm.chat(&messages, 0.0, 200).await {
        Ok(raw) => raw,
        Err(_) => return shortlist, // rerank failure keeps fusion order
    };
    let scores = match parse_score_array(&raw) {
        Some(scores) => scores,
        None => return shortlist,
    };
    if scores.len() != shortlist.len() {
        return shortlist;
    }
    let mut reranked: Vec<Scored> = shortlist
        .into_iter()
        .zip(scores)
        .map(|(candidate, score)| Scored {
            score: num_or(&score, candidate.score),
            hit: candidate.hit,
        })
        .collect();
    reranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    reranked
}

fn parse_score_array(raw: &str) -> Option<Vec<serde_json::Value>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Vec<serde_json::Value>>(&raw[start..=end]).ok()
}

fn num_or(value: &serde_json::Value, fallback: f64) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
        .unwrap_or(fallback)
}

fn to_search_hit(scored: &Scored, context: Option<Vec<SearchHitContext>>) -> SearchHit {
    let hit = &scored.hit;
    SearchHit {
        rel_path: hit.rel_path.clone(),
        title: hit.title.clone(),
        kind: hit.kind.clone(),
        origin: hit.origin.clone(),
        okf_type: hit.okf_type.clone(),
        tags: hit.tags.clone(),
        status: hit.status.clone(),
        stale_after: hit.stale_after.clone(),
        trust: trust_tier(hit.verified.as_ref()),
        hash: hit.hash.clone(),
        mtime: hit.mtime,
        heading_path: hit.heading_path.clone(),
        snippet: truncate_chars(&hit.content, 280),
        score: (scored.score * 1_000_000.0).round() / 1_000_000.0,
        context,
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}\u{2026}")
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
