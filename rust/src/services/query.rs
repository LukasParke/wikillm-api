//! Query service (plan → execute tools → synthesize), ported from TypeScript
//! `src/services/queryService.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use serde::Serialize;

use crate::domain::{QueryRecord, SearchFilters};
use crate::error::{Error, Result};
use crate::llm::provider::DynLlmProvider;
use crate::services::crag::{self, RetrievalGrade};
use crate::services::search::{
    collapse_by_content, rrf_fuse, SearchOptions, SearchService, NEAR_DUP_JACCARD,
};
use crate::services::settings::SettingsService;
use crate::store::Store;

const PLANNER_SYSTEM: &str = "You plan retrieval for a knowledge-base service. \
Available tools: search_pages (wiki + ingested docs), search_sources (raw source files), recent_changes (latest edits). \
Given a question, respond with ONLY JSON: {\"tools\":[{\"name\":\"search_pages\",\"query\":\"...\"}]}. \
Pick 1-3 tool calls with precise search queries. Prefer search_pages by default. \
For questions about current or latest state, plan queries that surface the most recently confirmed information.";

const SYNTHESIS_SYSTEM: &str = "You answer questions strictly from the provided evidence. \
Cite sources inline using their exact path in parentheses like (wiki/example.md). \
If evidence is insufficient, say so plainly. Never invent facts. \
Prefer the most recently confirmed fact when answering about current or latest state, \
and compare older versus newer mentions across the evidence rather than stopping at the first hit.";

const ABSTENTION_ANSWER: &str = "Not answerable from this knowledge base.";

const KNOWN_TOOLS: [&str; 3] = ["search_pages", "search_sources", "recent_changes"];

/// Owned handle that snapshots the current provider out of the container's
/// hot-swap lock. Returns `None` when no LLM is configured.
pub type LlmGetter = Box<dyn Fn() -> Option<DynLlmProvider> + Send + Sync>;

#[derive(Debug, Clone, Serialize)]
pub struct Citation {
    pub rel_path: String,
    pub hash: String,
    pub quote: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceOut {
    pub rel_path: String,
    pub heading_path: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolUse {
    pub name: String,
    pub query: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryAnswer {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub evidence: Vec<EvidenceOut>,
    pub tools_used: Vec<ToolUse>,
    pub mode: String,
    /// True when the CRAG retrieval guard gave up after its corrective round
    /// and returned an abstention instead of grounded content.
    #[serde(default)]
    pub abstained: bool,
}

#[derive(Debug, Clone)]
pub struct ToolPlanEntry {
    pub name: String,
    pub query: String,
}

/// Mirrors the TS `EvidenceItem` shape; some carrier fields (`kind`,
/// `origin`, `title`, `mtime`) exist for JSON parity though unused locally.
#[allow(dead_code)]
struct EvidenceItem {
    rel_path: String,
    kind: String,
    origin: String,
    title: Option<String>,
    heading_path: Option<String>,
    content: String,
    hash: String,
    mtime: i64,
    score: f64,
}

pub struct QueryService {
    store: Arc<dyn Store>,
    llm: LlmGetter,
    search: Arc<SearchService>,
    settings: Arc<SettingsService>,
}

impl QueryService {
    pub fn new(
        store: Arc<dyn Store>,
        llm: LlmGetter,
        search: Arc<SearchService>,
        settings: Arc<SettingsService>,
    ) -> Self {
        QueryService {
            store,
            llm,
            search,
            settings,
        }
    }

    pub async fn answer(
        &self,
        question: &str,
        filters: Option<&SearchFilters>,
        source: Option<&str>,
    ) -> Result<QueryAnswer> {
        let llm = (self.llm)().ok_or_else(|| Error::Provider("No LLM provider configured".into()))?;
        let started = Instant::now();

        let mut tools: Vec<ToolPlanEntry> = match llm
            .chat(
                &[("system", PLANNER_SYSTEM), ("user", question)],
                0.0,
                300,
            )
            .await
        {
            Ok(raw) => parse_tool_plan(&raw),
            Err(_) => Vec::new(),
        };
        if tools.is_empty() {
            tools = vec![ToolPlanEntry {
                name: "search_pages".to_string(),
                query: question.to_string(),
            }];
        }

        let mut evidence = self.execute_tools(&tools, filters).await;
        let mut corrective_tools: Vec<ToolPlanEntry> = Vec::new();
        let mut abstained = false;

        // CRAG corrective retrieval guard: grade the evidence before
        // synthesis; on Incorrect run exactly one rewrite-and-re-retrieve
        // round, and abstain when the merged evidence still grades Incorrect.
        // Grading failures degrade gracefully to unguarded answering.
        if matches!(self.settings.get_bool("retrieval_guard").await, Ok(true)) {
            match self.grade_evidence(&llm, question, &evidence).await {
                RetrievalGrade::Correct => {}
                RetrievalGrade::Ambiguous => {
                    self.expand_evidence_context(&mut evidence).await;
                }
                RetrievalGrade::Incorrect => {
                    let rewritten = match crag::rewrite_query(&llm, question).await {
                        Ok(raw) => {
                            let trimmed = raw.trim().to_string();
                            if trimmed.is_empty() {
                                question.to_string()
                            } else {
                                trimmed
                            }
                        }
                        Err(_) => question.to_string(),
                    };
                    let retry_tools: Vec<ToolPlanEntry> = tools
                        .iter()
                        .map(|tool| ToolPlanEntry {
                            name: tool.name.clone(),
                            query: rewritten.clone(),
                        })
                        .collect();
                    let corrective = self.execute_tools(&retry_tools, filters).await;
                    evidence = merge_evidence_rounds(evidence, corrective);
                    corrective_tools = retry_tools;

                    match self.grade_evidence(&llm, question, &evidence).await {
                        RetrievalGrade::Incorrect => abstained = true,
                        RetrievalGrade::Ambiguous => {
                            self.expand_evidence_context(&mut evidence).await;
                        }
                        RetrievalGrade::Correct => {}
                    }
                }
            }
        }

        if abstained {
            let tools_used: Vec<ToolPlanEntry> =
                tools.into_iter().chain(corrective_tools).collect();
            self.record(question, source, &started, 0, true, Vec::new(), None)
                .await;
            return Ok(QueryAnswer {
                answer: ABSTENTION_ANSWER.to_string(),
                citations: Vec::new(),
                evidence: Vec::new(),
                tools_used: tools_used
                    .into_iter()
                    .map(|tool| ToolUse {
                        name: tool.name,
                        query: tool.query,
                    })
                    .collect(),
                mode: "query".to_string(),
                abstained: true,
            });
        }

        let citations: Vec<Citation> = evidence
            .iter()
            .take(8)
            .map(|hit| Citation {
                rel_path: hit.rel_path.clone(),
                hash: hit.hash.clone(),
                quote: truncate_chars(&hit.content, 200),
            })
            .collect();
        let evidence_block = evidence
            .iter()
            .enumerate()
            .take(8)
            .map(|(i, hit)| {
                format!(
                    "[{}] {}{} (hash {})\n{}",
                    i + 1,
                    hit.rel_path,
                    match &hit.heading_path {
                        Some(hp) => format!(" :: {hp}"),
                        None => String::new(),
                    },
                    hit.hash.chars().take(12).collect::<String>(),
                    truncate_chars(&hit.content, 800)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let answer = match llm
            .chat(
                &[(
                    "system",
                    SYNTHESIS_SYSTEM,
                ), (
                    "user",
                    &format!(
                        "Question: {question}\n\nEvidence:\n{}",
                        if evidence_block.is_empty() {
                            "(none)".to_string()
                        } else {
                            evidence_block
                        }
                    ),
                )],
                0.2,
                1200,
            )
            .await
        {
            Ok(answer) => answer,
            Err(err) => {
                let top_paths = citation_top_paths(&citations);
                self.record(
                    question,
                    source,
                    &started,
                    0,
                    false,
                    top_paths,
                    Some(format!("{err}")),
                )
                .await;
                return Err(err);
            }
        };

        let top_paths = citation_top_paths(&citations);
        self.record(
            question,
            source,
            &started,
            evidence.len() as i64,
            evidence.is_empty(),
            top_paths,
            None,
        )
        .await;

        Ok(QueryAnswer {
            answer,
            citations,
            evidence: evidence
                .iter()
                .take(8)
                .map(|hit| EvidenceOut {
                    rel_path: hit.rel_path.clone(),
                    heading_path: hit.heading_path.clone(),
                    snippet: truncate_chars(&hit.content, 240),
                })
                .collect(),
            tools_used: tools
                .into_iter()
                .chain(corrective_tools)
                .map(|tool| ToolUse {
                    name: tool.name,
                    query: tool.query,
                })
                .collect(),
            mode: "query".to_string(),
            abstained: false,
        })
    }

    /// Grade the top evidence against the question via the CRAG grader.
    /// A grading failure is treated as Correct so transient LLM errors never
    /// block or wrongly abstain an otherwise answerable query.
    async fn grade_evidence(
        &self,
        llm: &DynLlmProvider,
        question: &str,
        evidence: &[EvidenceItem],
    ) -> RetrievalGrade {
        let snippets: Vec<String> = evidence
            .iter()
            .take(8)
            .map(|hit| truncate_chars(&hit.content, 500))
            .collect();
        match crag::grade_retrieval(llm, question, &snippets).await {
            Ok(result) => result.grade,
            Err(_) => RetrievalGrade::Correct,
        }
    }

    /// CRAG corrective action for an Ambiguous grade: attach neighboring
    /// chunk content to the top evidence items so synthesis sees wider
    /// context around each partially-relevant hit.
    async fn expand_evidence_context(&self, evidence: &mut [EvidenceItem]) {
        for item in evidence.iter_mut().take(8) {
            let Ok(Some(doc)) = self.store.get_document(&item.rel_path).await else {
                continue;
            };
            let Ok(chunks) = self.store.get_chunks_for_document(&doc.id).await else {
                continue;
            };
            let position = chunks.iter().position(|chunk| {
                match &item.heading_path {
                    Some(hp) => chunk.heading_path.as_deref() == Some(hp.as_str()),
                    None => content_prefix_match(chunk.content.as_str(), &item.content),
                }
            });
            let Some(index) = position else { continue };
            let mut context: Vec<&str> = Vec::new();
            if index > 0 {
                context.push(chunks[index - 1].content.trim());
            }
            if index + 1 < chunks.len() {
                context.push(chunks[index + 1].content.trim());
            }
            if context.is_empty() {
                continue;
            }
            let joined = truncate_chars(&context.join("\n\n"), 800);
            item.content.push_str("\n\n[context] ");
            item.content.push_str(&joined);
        }
    }

    /// Run every planned tool concurrently and fuse the fulfilled branches
    /// with Reciprocal Rank Fusion (K=60) over `rel_path::heading_path` keys
    /// so consensus across tools outranks any single list's top hits, then
    /// collapse near-duplicates ahead of the caller's top-8 cut.
    async fn execute_tools(
        &self,
        tools: &[ToolPlanEntry],
        filters: Option<&SearchFilters>,
    ) -> Vec<EvidenceItem> {
        let runs = tools.iter().map(|tool| self.run_tool(tool, filters));
        let settled = join_all(runs).await;

        let mut lists: Vec<Vec<String>> = Vec::new();
        let mut pool: HashMap<String, EvidenceItem> = HashMap::new();
        for items in settled.into_iter().flatten() {
            let mut list: Vec<String> = Vec::new();
            for item in items {
                let key = format!(
                    "{}::{}",
                    item.rel_path,
                    item.heading_path.clone().unwrap_or_default()
                );
                let replace = match pool.get(&key) {
                    Some(existing) => existing.score < item.score,
                    None => true,
                };
                if replace {
                    pool.insert(key.clone(), item);
                }
                if !list.contains(&key) {
                    list.push(key);
                }
            }
            if !list.is_empty() {
                lists.push(list);
            }
        }

        let fused: Vec<EvidenceItem> = rrf_fuse(&lists, 60)
            .into_iter()
            .filter_map(|(key, score)| {
                let mut item = pool.remove(&key)?;
                item.score = score;
                Some(item)
            })
            .collect();

        // RRF output is descending, so higher-scored entries survive the
        // near-duplicate collapse.
        collapse_by_content(fused, |item| item.content.as_str(), NEAR_DUP_JACCARD)
    }

    async fn run_tool(
        &self,
        tool: &ToolPlanEntry,
        filters: Option<&SearchFilters>,
    ) -> Result<Vec<EvidenceItem>> {
        if tool.name == "recent_changes" {
            let changes = self.store.list_changes(None, None, None, 10).await?;
            return Ok(changes
                .iter()
                .map(|change| EvidenceItem {
                    rel_path: change.rel_path.clone(),
                    kind: "change".to_string(),
                    origin: change.source.clone().unwrap_or_else(|| "external".to_string()),
                    title: Some(format!("{}: {}", change.change_type, change.rel_path)),
                    heading_path: None,
                    content: format!(
                        "Change {} detected at {}",
                        change.change_type, change.detected_at
                    ),
                    hash: change.new_hash.clone().unwrap_or_default(),
                    mtime: parse_rfc3339_millis(&change.detected_at),
                    score: 0.0,
                })
                .collect());
        }
        let is_sources = tool.name == "search_sources";
        let applied: Option<SearchFilters> = if is_sources {
            let mut f: SearchFilters = filters.cloned().unwrap_or_default();
            f.kinds = Some(vec!["source".to_string()]);
            Some(f)
        } else {
            filters.cloned()
        };
        let found = self
            .search
            .search(SearchOptions {
                q: tool.query.clone(),
                limit: 8,
                filters: applied,
                rerank: false,
                expand_context: false,
            })
            .await?;
        Ok(found
            .results
            .iter()
            .map(|result| EvidenceItem {
                rel_path: result.rel_path.clone(),
                kind: result.kind.clone(),
                origin: result.origin.clone(),
                title: result.title.clone(),
                heading_path: result.heading_path.clone(),
                content: result.snippet.clone(),
                hash: result.hash.clone(),
                mtime: result.mtime,
                score: result.score,
            })
            .collect())
    }

    async fn record(
        &self,
        question: &str,
        source: Option<&str>,
        started: &Instant,
        result_count: i64,
        zero_hit: bool,
        top_paths: Vec<String>,
        error: Option<String>,
    ) {
        let rec = QueryRecord {
            id: ulid::Ulid::new().to_string(),
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            query: question.to_string(),
            mode: "query".to_string(),
            project: None,
            latency_ms: started.elapsed().as_millis() as f64,
            result_count,
            zero_hit,
            top_paths,
            source: source.map(str::to_string),
            error,
        };
        let _ = self.store.record_query(&rec).await;
    }
}

/// Deduplicated citation paths in citation order (for `queries.top_paths`).
fn citation_top_paths(citations: &[Citation]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for citation in citations {
        if seen.insert(citation.rel_path.clone()) {
            paths.push(citation.rel_path.clone());
        }
    }
    paths
}

/// Merge the original retrieval round with the corrective round, keeping the
/// highest-scoring entry per `rel_path::heading_path` key.
fn merge_evidence_rounds(primary: Vec<EvidenceItem>, corrective: Vec<EvidenceItem>) -> Vec<EvidenceItem> {
    let mut best = index_map::IndexMap::new();
    for item in primary.into_iter().chain(corrective) {
        best.keep_max(item);
    }
    let mut items = best.into_items();
    items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    items
}

/// Loose chunk-to-evidence matching for heading-less items: compare a short
/// normalized prefix of both contents (snippets are char-truncated copies).
fn content_prefix_match(chunk_content: &str, evidence_content: &str) -> bool {
    fn clean(s: &str) -> &str {
        s.trim_end_matches('\u{2026}').trim()
    }
    let prefix = |s: &str| clean(s).chars().take(80).collect::<String>();
    let chunk_prefix = prefix(chunk_content);
    let evidence_prefix = prefix(evidence_content);
    !chunk_prefix.is_empty()
        && (chunk_content.starts_with(&evidence_prefix)
            || clean(evidence_content).starts_with(&chunk_prefix))
}

fn parse_rfc3339_millis(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}\u{2026}")
    }
}

/// Parse an LLM planner reply into tool calls. Malformed JSON, a non-array
/// `tools` field, or unknown tool names yield an empty/filtered plan.
pub fn parse_tool_plan(raw: &str) -> Vec<ToolPlanEntry> {
    let Some(start) = raw.find('{') else {
        return Vec::new();
    };
    let Some(end) = raw.rfind('}') else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    let parsed: serde_json::Value = match serde_json::from_str(&raw[start..=end]) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };
    let Some(tools) = parsed.get("tools").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in tools {
        let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !KNOWN_TOOLS.contains(&name) {
            continue;
        }
        let query = entry
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        out.push(ToolPlanEntry {
            name: name.to_string(),
            query,
        });
    }
    out
}

/// Tiny insertion-ordered dedup map keyed by `rel_path::heading_path`,
/// keeping the entry with the highest score.
mod index_map {
    use super::EvidenceItem;
    use std::collections::HashMap;

    pub struct IndexMap {
        order: Vec<String>,
        items: HashMap<String, EvidenceItem>,
    }

    impl IndexMap {
        pub fn new() -> Self {
            IndexMap {
                order: Vec::new(),
                items: HashMap::new(),
            }
        }

        pub fn keep_max(&mut self, item: EvidenceItem) {
            let key = format!(
                "{}::{}",
                item.rel_path,
                item.heading_path.clone().unwrap_or_default()
            );
            match self.items.get(&key) {
                Some(existing) if existing.score >= item.score => {}
                Some(_) => {
                    self.items.insert(key, item);
                }
                None => {
                    self.order.push(key.clone());
                    self.items.insert(key, item);
                }
            }
        }
        pub fn into_items(mut self) -> Vec<EvidenceItem> {
            self.order
                .into_iter()
                .filter_map(|key| self.items.remove(&key))
                .collect()
        }
    }

    impl Default for IndexMap {
        fn default() -> Self {
            Self::new()
        }
    }
}
