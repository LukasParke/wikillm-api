//! Query service (plan → execute tools → synthesize), ported from TypeScript
//! `src/services/queryService.ts`.

use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use serde::Serialize;

use crate::domain::{QueryRecord, SearchFilters};
use crate::error::{Error, Result};
use crate::llm::provider::DynLlmProvider;
use crate::services::search::{SearchOptions, SearchService};
use crate::store::Store;

const PLANNER_SYSTEM: &str = "You plan retrieval for a knowledge-base service. \
Available tools: search_pages (wiki + ingested docs), search_sources (raw source files), recent_changes (latest edits). \
Given a question, respond with ONLY JSON: {\"tools\":[{\"name\":\"search_pages\",\"query\":\"...\"}]}. \
Pick 1-3 tool calls with precise search queries. Prefer search_pages by default.";

const SYNTHESIS_SYSTEM: &str = "You answer questions strictly from the provided evidence. \
Cite sources inline using their exact path in parentheses like (wiki/example.md). \
If evidence is insufficient, say so plainly. Never invent facts.";

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
}

impl QueryService {
    pub fn new(store: Arc<dyn Store>, llm: LlmGetter, search: Arc<SearchService>) -> Self {
        QueryService {
            store,
            llm,
            search,
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

        let evidence = self.execute_tools(&tools, filters).await;
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
                self.record(question, source, &started, 0, false, Some(format!("{err}")))
                    .await;
                return Err(err);
            }
        };

        self.record(
            question,
            source,
            &started,
            evidence.len() as i64,
            evidence.is_empty(),
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
                .map(|tool| ToolUse {
                    name: tool.name,
                    query: tool.query,
                })
                .collect(),
            mode: "query".to_string(),
        })
    }

    /// Promise.allSettled equivalent: run every planned tool concurrently and
    /// keep only the fulfilled branches.
    async fn execute_tools(
        &self,
        tools: &[ToolPlanEntry],
        filters: Option<&SearchFilters>,
    ) -> Vec<EvidenceItem> {
        let runs = tools.iter().map(|tool| self.run_tool(tool, filters));
        let settled = join_all(runs).await;
        let merged: Vec<EvidenceItem> = settled
            .into_iter()
            .flatten()
            .flat_map(|items| items.into_iter())
            .collect();

        // Dedupe by rel_path + heading_path, keeping the max score.
        let mut best: index_map::IndexMap = index_map::IndexMap::new();
        for item in merged {
            best.keep_max(item);
        }
        let mut items: Vec<EvidenceItem> = best.into_items();
        items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        items
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
            top_paths: Vec::new(),
            source: source.map(str::to_string),
            error,
        };
        let _ = self.store.record_query(&rec).await;
    }
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

