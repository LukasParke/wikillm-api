//! Background maintenance loop (wave-2 improver): document staleness scans,
//! cost-capped LLM refresh proposals, and memory hygiene over the
//! most-accessed scopes.
//!
//! Design constraints baked in here:
//! - The scan NEVER touches store files: documents are paged through the
//!   existing paginated listing ([`Store::list_documents`]) and `stale_after`
//!   is filtered in memory against the current time.
//! - Every LLM call draws from a plain per-tick budget (`max_llm_calls`), so
//!   the configured cost cap holds regardless of how much work a tick finds.
//! - Refresh output is append-only: proposals land in `<wiki_root>/log.md`
//!   under today's date heading (same file the API log-append path writes),
//!   never as edits to the pages themselves.
//! - Metrics are log-based (`tracing`) — there is no metrics registry wiring
//!   required for this service.
//!
//! Not registered in `services/mod.rs` here; INTEGRATION-B owns registration
//! and spawning (`interval_seconds == 0` makes [`ImproverService::run_forever`]
//! exit immediately, which is how the `improver_interval_seconds: 0 = off`
//! setting disables the loop).

use crate::domain::{DocumentRecord, ListOptions};
use crate::error::Result;
use crate::llm::provider::DynLlmProvider;
use crate::services::memory::{
    memory_type_to_str, normalize_content, sha256_hex, AgentMemory, MemoryService,
};
use crate::services::settings::SettingsService;
use crate::store::{MemoryMutation, Store};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Page size for the paginated staleness scan.
const SCAN_PAGE_SIZE: i64 = 200;
/// Safety cap on pages per scan so a pathological store cannot spin forever.
const MAX_SCAN_PAGES: u32 = 1000;
/// A scope qualifies for hygiene once it holds MORE than this many memories.
const HYGIENE_SCOPE_MIN: i64 = 30;
/// Top-accessed memories considered per hygiene pass.
const HYGIENE_TOP_N: i64 = 20;
/// Max characters of document body fed to the refresh prompt.
const REFRESH_BODY_SNIPPET_CHARS: usize = 2000;

const REFRESH_SYSTEM: &str = "You refresh stale wiki knowledge-base pages. Given a page title, its summary, and a body excerpt, output ONLY the markdown body of a short `## Refreshed summary` appendix: restate the facts that still appear current and flag claims that may have changed since the page was last updated. No preamble, no code fences.";

const HYGIENE_SYSTEM: &str = "You are a memory hygiene engine. You receive the TOP-ACCESSED memories of one scope, numbered from 0. Find (a) near-duplicate memories worth merging into one canonical phrasing and (b) direct contradictions where one statement supersedes another. Respond ONLY with JSON: {\"actions\":[{\"action\":\"merge\",\"keep\":0,\"drop\":[2],\"content\":\"canonical merged text\"},{\"action\":\"delete\",\"id\":3},{\"action\":\"noop\"}]}. For merges: keep = index of the surviving memory, drop = indices whose content is folded into it, content = the canonical merged wording preserving every durable fact. Use delete only for hard contradictions; prefer merge over delete; noop when nothing needs changing.";

/// One hygiene decision resolved against real memory ids.
#[derive(Debug, Clone, PartialEq)]
enum HygieneAction {
    /// Fold `drop` memories into `keep`, replacing its content with `content`.
    Merge {
        keep: String,
        drop: Vec<String>,
        content: String,
    },
    Delete(String),
    Noop,
}

/// Per-tick LLM cost cap. Every LLM call must draw from the budget first;
/// once exhausted, remaining work degrades gracefully (scan-only).
struct LlmBudget {
    remaining: u32,
}

impl LlmBudget {
    fn new(cap: u32) -> Self {
        Self { remaining: cap }
    }

    /// Spend one call; `false` means the budget is exhausted.
    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn remaining(&self) -> u32 {
        self.remaining
    }
}

/// Log-metrics snapshot of one maintenance tick.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TickStats {
    pub stale_documents: usize,
    pub refresh_proposals: usize,
    pub hygiene_mutations: usize,
}

pub struct ImproverService {
    store: Arc<dyn Store>,
    memory: Arc<MemoryService>,
    settings: Arc<SettingsService>,
    wiki_root: PathBuf,
    scopes: std::sync::Mutex<BTreeSet<String>>,
}

impl ImproverService {
    /// `wiki_root` locates the `log.md` proposal ledger (same file the API
    /// log-append handler writes). `watched_scopes` seeds the memory-hygiene
    /// target list; more can be added at runtime via [`Self::watch_scope`].
    pub fn new(
        store: Arc<dyn Store>,
        memory: Arc<MemoryService>,
        settings: Arc<SettingsService>,
        wiki_root: impl Into<PathBuf>,
        watched_scopes: Vec<String>,
    ) -> Self {
        Self {
            store,
            memory,
            settings,
            wiki_root: wiki_root.into(),
            scopes: std::sync::Mutex::new(watched_scopes.into_iter().collect()),
        }
    }

    /// Register a scope key (the canonical `"user|agent|session"` string) for
    /// future hygiene passes. INTEGRATION-B can call this wherever scope
    /// activity becomes known.
    pub fn watch_scope(&self, scope_key: &str) {
        self.scopes
            .lock()
            .expect("improver scopes lock")
            .insert(scope_key.to_string());
    }

    /// Shared handle to the underlying [`MemoryService`] — the hygiene tick
    /// applies resolutions through the store with MemoryService's exact hash
    /// and mutation conventions; callers (e.g. INTEGRATION-B wiring) can also
    /// drive two-phase consolidation between ticks.
    pub fn memory(&self) -> &MemoryService {
        &self.memory
    }

    /// Maintenance loop. `max_llm_calls` caps LLM usage per tick (the
    /// `improver_max_llm_calls` setting, passed in plain so this module stays
    /// decoupled from the settings registry). `interval_seconds == 0` exits
    /// immediately — the documented "off" switch.
    ///
    /// The first tick fires right away (tokio interval semantics), then the
    /// loop sleeps `interval` between ticks.
    pub async fn run_forever(
        &self,
        llm: Option<DynLlmProvider>,
        max_llm_calls: u32,
        interval_seconds: u64,
    ) {
        if interval_seconds == 0 {
            tracing::info!("improver disabled (interval 0); exiting");
            return;
        }
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
        loop {
            ticker.tick().await;
            let mut budget = LlmBudget::new(max_llm_calls);
            match self.tick(llm.as_ref(), &mut budget).await {
                Ok(stats) => tracing::info!(
                    stale_documents = stats.stale_documents,
                    refresh_proposals = stats.refresh_proposals,
                    hygiene_mutations = stats.hygiene_mutations,
                    llm_calls_left = budget.remaining(),
                    "improver tick complete"
                ),
                Err(err) => tracing::warn!(error = %err, "improver tick failed"),
            }
        }
    }

    /// One maintenance pass: staleness scan (+ optional autorewrite
    /// proposals), then memory hygiene. Never fails the loop: sub-step errors
    /// are logged and swallowed so one bad batch cannot kill the task.
    async fn tick(&self, llm: Option<&DynLlmProvider>, budget: &mut LlmBudget) -> Result<TickStats> {
        let autorewrite = self.settings.get_bool("improver_autorewrite").await?;
        let stale = self.scan_stale().await?;
        let mut stats = TickStats {
            stale_documents: stale.len(),
            ..TickStats::default()
        };

        if autorewrite {
            match llm {
                Some(llm) => {
                    for doc in &stale {
                        if !budget.take() {
                            break;
                        }
                        match self.propose_refresh(llm, doc).await {
                            Ok(Some(entry)) => match self.append_log_entry(&entry) {
                                Ok(()) => stats.refresh_proposals += 1,
                                Err(err) => {
                                    tracing::warn!(error = %err, rel_path = %doc.rel_path, "failed to append refresh proposal to log.md")
                                }
                            },
                            Ok(None) => {}
                            Err(err) => {
                                tracing::warn!(error = %err, rel_path = %doc.rel_path, "refresh proposal LLM call failed")
                            }
                        }
                    }
                }
                None => {
                    tracing::info!("improver_autorewrite is on but no LLM provider is configured; skipping refresh proposals")
                }
            }
        }

        stats.hygiene_mutations = self.hygiene_tick(llm, budget).await.unwrap_or_else(|err| {
            tracing::warn!(error = %err, "memory hygiene tick failed");
            0
        });
        Ok(stats)
    }

    /// Page every document through the existing paginated listing and filter
    /// stale ones in memory — no store changes, no filesystem access.
    async fn scan_stale(&self) -> Result<Vec<DocumentRecord>> {
        let now = chrono::Utc::now();
        let opts = ListOptions::default();
        let mut cursor: Option<String> = None;
        let mut stale = Vec::new();
        for _ in 0..MAX_SCAN_PAGES {
            let page = self
                .store
                .list_documents(&opts, SCAN_PAGE_SIZE, cursor.as_deref())
                .await?;
            stale.extend(
                page.items
                    .iter()
                    .filter(|d| {
                        is_stale(d.stale_after.as_deref(), d.status.as_deref(), now)
                    })
                    .cloned(),
            );
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(stale),
            }
        }
        tracing::warn!("improver staleness scan hit the page cap; results may be partial");
        Ok(stale)
    }

    /// Generate one append-only log.md proposal for a stale document.
    /// Returns `None` when the model produced nothing usable.
    async fn propose_refresh(
        &self,
        llm: &DynLlmProvider,
        doc: &DocumentRecord,
    ) -> Result<Option<String>> {
        let title = doc.title.clone().unwrap_or_else(|| doc.rel_path.clone());
        let summary = doc.summary.as_deref().unwrap_or("(no summary)");
        let snippet: String = doc.body.chars().take(REFRESH_BODY_SNIPPET_CHARS).collect();
        let user = format!(
            "Page: {}\nTitle: {}\nSummary: {}\n\nBody excerpt:\n{snippet}",
            doc.rel_path, title, summary
        );
        let messages: Vec<(&str, &str)> =
            vec![("system", REFRESH_SYSTEM), ("user", &user)];
        let raw = llm.chat(&messages, 0.2, 400).await?;
        let appendix = raw.trim();
        if appendix.is_empty() {
            return Ok(None);
        }
        Ok(Some(format_refresh_proposal(
            &doc.rel_path,
            &title,
            doc.stale_after.as_deref(),
            appendix,
        )))
    }

    /// Append an entry to `<wiki_root>/log.md` — the same ledger the API's
    /// log-append path writes (http/mod.rs). Strictly append-only: existing
    /// content is never truncated or rewritten. Today's date heading is
    /// reused when already present instead of duplicated.
    fn append_log_entry(&self, entry: &str) -> std::io::Result<()> {
        use std::io::Write;
        let path = self.wiki_root.join("log.md");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();

        let mut body = entry.to_string();
        if let Some(first_line) = entry.lines().next() {
            if first_line.starts_with("## ") && existing.contains(first_line) {
                body = body[first_line.len()..].trim_start_matches('\n').to_string();
            }
        }

        let mut payload = String::new();
        if !existing.is_empty() && !existing.ends_with('\n') {
            payload.push('\n');
        }
        payload.push_str(&body);
        if !payload.ends_with('\n') {
            payload.push('\n');
        }

        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        f.write_all(payload.as_bytes())
    }

    /// Hygiene over watched scopes: only scopes holding more than
    /// [`HYGIENE_SCOPE_MIN`] memories qualify; per qualifying scope one
    /// cost-capped LLM call proposes merges/contradiction resolutions over
    /// the top-accessed memories, applied through the store with a persisted
    /// [`MemoryMutation`] per outcome (add/update/delete/noop convention).
    async fn hygiene_tick(
        &self,
        llm: Option<&DynLlmProvider>,
        budget: &mut LlmBudget,
    ) -> Result<usize> {
        let Some(llm) = llm else {
            return Ok(0);
        };
        let scopes: Vec<String> = self
            .scopes
            .lock()
            .expect("improver scopes lock")
            .iter()
            .cloned()
            .collect();
        let mut applied = 0usize;
        for scope_key in scopes {
            if budget.remaining() == 0 {
                break;
            }
            // Probe one past the threshold; search_memories orders by
            // access_count DESC so this doubles as the top-accessed fetch.
            // Side effect: returned rows get their access_count bumped —
            // acceptable for a periodic pass (see handoff note re: a neutral
            // list_memories trait method).
            let probe = self
                .store
                .search_memories(&scope_key, "", HYGIENE_SCOPE_MIN + 1)
                .await?;
            if probe.len() as i64 <= HYGIENE_SCOPE_MIN {
                continue;
            }
            let top: Vec<AgentMemory> = probe.into_iter().take(HYGIENE_TOP_N as usize).collect();
            if !budget.take() {
                break;
            }
            match self.hygiene_pass(llm, &scope_key, &top).await {
                Ok(n) => applied += n,
                Err(err) => {
                    tracing::warn!(error = %err, scope = %scope_key, "memory hygiene pass failed")
                }
            }
        }
        Ok(applied)
    }

    /// One scope pass: ask the model for merge/delete/noop resolutions over
    /// `memories` (numbered), then apply them. Returns the number of applied
    /// mutations. An unparseable model response applies nothing.
    async fn hygiene_pass(
        &self,
        llm: &DynLlmProvider,
        scope_key: &str,
        memories: &[AgentMemory],
    ) -> Result<usize> {
        let listing: Vec<String> = memories
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!("[{i}] ({}) {}", memory_type_to_str(&m.memory_type), m.content)
            })
            .collect();
        let user = format!(
            "Scope: {scope_key}\n\nMemories:\n{}",
            listing.join("\n")
        );
        let messages: Vec<(&str, &str)> =
            vec![("system", HYGIENE_SYSTEM), ("user", &user)];
        let raw = llm.chat(&messages, 0.0, 600).await?;
        let Some(actions) = parse_hygiene_response(&raw, memories) else {
            tracing::warn!(scope = %scope_key, "hygiene LLM output was not usable JSON; applying nothing");
            return Ok(0);
        };
        self.apply_hygiene_actions(actions, memories).await
    }

    async fn apply_hygiene_actions(
        &self,
        actions: Vec<HygieneAction>,
        memories: &[AgentMemory],
    ) -> Result<usize> {
        let mut applied = 0usize;
        for action in actions {
            match action {
                HygieneAction::Merge { keep, drop, content } => {
                    // Hash convention mirrors MemoryService: sha256 of the
                    // normalized content, raw content stored.
                    let normalized = normalize_content(&content);
                    let hash = sha256_hex(&normalized);
                    let old = memories.iter().find(|m| m.id == keep);
                    self.store.update_memory(&keep, &content, &hash).await?;
                    self.record_mutation(
                        &keep,
                        "update",
                        old.map(|m| m.content.as_str()),
                        Some(&content),
                    )
                    .await?;
                    applied += 1;
                    for id in &drop {
                        let old = memories.iter().find(|m| m.id == *id);
                        self.store.delete_memory(id).await?;
                        self.record_mutation(id, "delete", old.map(|m| m.content.as_str()), None)
                            .await?;
                        applied += 1;
                    }
                }
                HygieneAction::Delete(id) => {
                    let old = memories.iter().find(|m| m.id == *id);
                    self.store.delete_memory(&id).await?;
                    self.record_mutation(&id, "delete", old.map(|m| m.content.as_str()), None)
                        .await?;
                    applied += 1;
                }
                HygieneAction::Noop => {
                    // Persist the outcome too, matching the memory-ledger
                    // convention that every decision leaves an audit trail.
                    self.record_mutation("", "noop", None, None).await?;
                }
            }
        }
        Ok(applied)
    }

    /// Append-only audit trail entry (mirrors MemoryService::persist_mutation;
    /// local copy until `new_mutation_id` is shared).
    async fn record_mutation(
        &self,
        memory_id: &str,
        action: &str,
        old_content: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<()> {
        let m = MemoryMutation {
            id: new_mutation_id(),
            memory_id: memory_id.to_string(),
            action: action.to_string(),
            old_content: old_content.map(str::to_string),
            new_content: new_content.map(str::to_string),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.store.record_memory_mutation(&m).await
    }
}

/// `mm-` + 12-char ulid-style suffix (matches the `wh-`/`sess-` id pattern).
fn new_mutation_id() -> String {
    format!("mm-{}", &ulid::Ulid::new().to_string()[..12].to_lowercase())
}

/// Staleness predicate: a document is due for refresh when it carries a
/// parseable `stale_after` timestamp in the past and is not a draft.
/// Unparseable timestamps never fire (fail closed).
fn is_stale(
    stale_after: Option<&str>,
    status: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if status.map(|s| s.eq_ignore_ascii_case("draft")).unwrap_or(false) {
        return false;
    }
    let Some(raw) = stale_after.filter(|s| !s.trim().is_empty()) else {
        return false;
    };
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(t) => t.with_timezone(&chrono::Utc) < now,
        Err(_) => false,
    }
}

/// Parse the hygiene LLM response, resolving numeric indices back to real
/// memory ids. Malformed JSON yields `None`; individual malformed actions are
/// skipped. A merge without a non-empty canonical text or without anything to
/// drop is skipped (rewriting the keeper alone is not a merge).
fn parse_hygiene_response(raw: &str, memories: &[AgentMemory]) -> Option<Vec<HygieneAction>> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    let parsed: Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let actions = parsed.get("actions")?.as_array()?;

    let resolve = |idx: u64| memories.get(idx as usize).map(|m| m.id.clone());

    let mut out = Vec::new();
    for action in actions {
        match action.get("action").and_then(Value::as_str) {
            Some("noop") => out.push(HygieneAction::Noop),
            Some("delete") => {
                if let Some(id) = action.get("id").and_then(Value::as_u64).and_then(resolve) {
                    out.push(HygieneAction::Delete(id));
                }
            }
            Some("merge") => {
                let keep = action.get("keep").and_then(Value::as_u64).and_then(resolve);
                let content = action
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let drop: Vec<String> = action
                    .get("drop")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_u64)
                            .filter_map(resolve)
                            .filter(|id| Some(id) != keep.as_ref())
                            .collect()
                    })
                    .unwrap_or_default();
                if let (Some(keep), Some(content)) = (keep, content) {
                    if !drop.is_empty() {
                        out.push(HygieneAction::Merge { keep, drop, content });
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Build one append-only log.md entry: today's date heading (validated format
/// per OKF log rules), a `* **RefreshProposal**:` bullet, and the proposed
/// appendix inside a fenced block so it is unambiguously a proposal and the
/// embedded `## Refreshed summary` heading stays out of heading validation.
fn format_refresh_proposal(
    rel_path: &str,
    title: &str,
    stale_after: Option<&str>,
    appendix_raw: &str,
) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d");
    let appendix = normalize_appendix(appendix_raw);
    let since = stale_after.unwrap_or("an unknown date");
    format!(
        "## {today}\n\n* **RefreshProposal**: [[{rel_path}]] — \"{title}\" went stale after {since}. Proposed refreshed-summary appendix below (proposal only, NOT applied):\n\n```markdown\n## Refreshed summary\n{appendix}\n```\n"
    )
}

/// Normalize model output for embedding in a proposal: unwrap a single
/// markdown fence pair and drop an echoed `## Refreshed summary` heading
/// (we add our own).
fn normalize_appendix(raw: &str) -> String {
    let mut text = raw.trim();
    if let Some(after_fence) = text.strip_prefix("```") {
        let after_lang = after_fence.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
        if let Some(body) = after_lang.strip_prefix('\n') {
            if let Some(unwrapped) = body.strip_suffix("```") {
                text = unwrapped.trim();
            }
        }
    }
    let mut lines = text.lines().peekable();
    if let Some(first) = lines.peek() {
        let t = first.trim();
        if t.starts_with('#')
            && t.trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("refreshed summary")
        {
            lines.next();
        }
    }
    lines.collect::<Vec<_>>().join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::provider::ChatMessage;
    use crate::llm::provider::LlmProvider;
    use crate::services::memory::MemoryType;
    use crate::store::sqlite::SqliteStore;
    use std::collections::HashMap;

    fn ts(secs_ago: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(secs_ago)).to_rfc3339()
    }

    #[test]
    fn is_stale_flags_past_timestamps_only() {
        let now = chrono::Utc::now();
        assert!(is_stale(Some(&ts(3600)), None, now));
        assert!(is_stale(Some(&ts(3600)), Some("published"), now));
        // Drafts never fire.
        assert!(!is_stale(Some(&ts(3600)), Some("draft"), now));
        assert!(!is_stale(Some(&ts(3600)), Some("DRAFT"), now));
        // Future / missing / blank / unparseable never fire.
        assert!(!is_stale(Some(&ts(-3600)), None, now));
        assert!(!is_stale(None, None, now));
        assert!(!is_stale(Some(""), None, now));
        assert!(!is_stale(Some("not-a-date"), None, now));
    }

    fn memory(idx: usize) -> AgentMemory {
        AgentMemory {
            id: format!("mem-{idx:03}"),
            scope_key: "u|a|".into(),
            memory_type: MemoryType::Semantic,
            content: format!("memory {idx}"),
            created_at: String::new(),
            accessed_at: String::new(),
            access_count: 0,
            source_session_id: None,
            source_ref: None,
        }
    }

    #[test]
    fn parse_hygiene_maps_indices_to_ids() {
        let memories: Vec<AgentMemory> = (0..4).map(memory).collect();
        let actions = parse_hygiene_response(
            r#"ok {"actions":[{"action":"merge","keep":0,"drop":[2],"content":"merged"},{"action":"delete","id":3},{"action":"noop"}]}"#,
            &memories,
        )
        .unwrap();
        assert_eq!(
            actions,
            vec![
                HygieneAction::Merge {
                    keep: "mem-000".into(),
                    drop: vec!["mem-002".into()],
                    content: "merged".into(),
                },
                HygieneAction::Delete("mem-003".into()),
                HygieneAction::Noop,
            ]
        );
    }

    #[test]
    fn parse_hygiene_skips_bad_actions_but_not_the_batch() {
        let memories: Vec<AgentMemory> = (0..3).map(memory).collect();
        // Out-of-range indices skipped; merge without drop skipped; merge
        // dropping the keeper filtered; empty content skipped; unknown
        // action ignored — yet a valid delete survives.
        let actions = parse_hygiene_response(
            r#"{"actions":[
                {"action":"merge","keep":99,"drop":[1],"content":"x"},
                {"action":"merge","keep":0,"drop":[7],"content":"x"},
                {"action":"merge","keep":0,"drop":[0,1],"content":"x"},
                {"action":"merge","keep":0,"drop":[1],"content":"  "},
                {"action":"explode","id":0},
                {"action":"delete","id":1}
            ]}"#,
            &memories,
        )
        .unwrap();
        assert_eq!(
            actions,
            vec![
                HygieneAction::Merge {
                    keep: "mem-000".into(),
                    drop: vec!["mem-001".into()],
                    content: "x".into(),
                },
                HygieneAction::Delete("mem-001".into()),
            ]
        );
    }

    #[test]
    fn parse_hygiene_rejects_non_json() {
        let memories: Vec<AgentMemory> = (0..2).map(memory).collect();
        assert_eq!(parse_hygiene_response("no json at all", &memories), None);
        assert_eq!(parse_hygiene_response("{broken", &memories), None);
        assert_eq!(
            parse_hygiene_response(r#"{"wrong":true}"#, &memories),
            None
        );
    }

    #[test]
    fn budget_caps_llm_calls() {
        let mut b = LlmBudget::new(2);
        assert!(b.take());
        assert!(b.take());
        assert!(!b.take());
        assert_eq!(b.remaining(), 0);
        assert!(!LlmBudget::new(0).take());
    }

    #[test]
    fn refresh_proposal_is_fenced_and_deduplicated() {
        let entry = format_refresh_proposal(
            "docs/architecture.md",
            "Architecture",
            Some(&ts(7200)),
            "## Refreshed summary\nEverything still holds.",
        );
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert!(entry.starts_with(&format!("## {today}\n\n")));
        assert!(entry.contains("* **RefreshProposal**: [[docs/architecture.md]]"));
        assert!(entry.contains("```markdown"));
        // Exactly one echoed heading, inside the fence.
        assert_eq!(entry.matches("## Refreshed summary").count(), 1);
        assert!(entry.ends_with("```\n"));

        // Model wrapping its output in a fence is unwrapped, not nested.
        let nested = format_refresh_proposal(
            "a.md",
            "A",
            None,
            "```markdown\nFresh facts.\n```",
        );
        assert!(nested.contains("```markdown\n## Refreshed summary\nFresh facts.\n```\n"));
    }

    #[test]
    fn normalize_appendix_unwraps_and_trims() {
        assert_eq!(normalize_appendix("plain"), "plain");
        assert_eq!(normalize_appendix("  spaced  "), "spaced");
        assert_eq!(
            normalize_appendix("```\nfenced\n```"),
            "fenced"
        );
        assert_eq!(
            normalize_appendix("# Refreshed Summary\ntext"),
            "text"
        );
        assert_eq!(normalize_appendix(""), "");
    }

    struct MockLlm {
        response: &'static str,
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockLlm {
        fn model(&self) -> &str {
            "mock"
        }
        fn embed_model(&self) -> Option<String> {
            None
        }
        fn embed_dims(&self) -> Option<i64> {
            None
        }
        async fn chat(&self, _messages: &[ChatMessage<'_>], _t: f32, _mt: i64) -> Result<String> {
            Ok(self.response.to_string())
        }
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(Vec::new())
        }
    }

    async fn make_service() -> (Arc<dyn Store>, ImproverService) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            SqliteStore::open(dir.path().join("improver-test.db").to_str().unwrap()).unwrap(),
        );
        store.migrate().await.unwrap();
        let config = crate::config::Config {
            wiki_root: dir.path().display().to_string(),
            port: 0,
            host: "127.0.0.1".into(),
            api_keys: HashMap::new(),
            public_read: false,
            db_path: String::new(),
            log_level: "info".into(),
            db_backend: "sqlite".into(),
            database_url: None,
            layout: "default".into(),
            okf_strict: false,
            human_actors: Vec::new(),
            llm_base_url: None,
            llm_api_key: None,
            llm_model: "mock".into(),
            llm_embed_model: None,
            embedding_dims: 0,
            llm_distill: false,
            connector_poll_seconds: 60,
            rate_limit_rpm: 600,
        };
        let settings = Arc::new(SettingsService::new(store.clone(), config));
        let memory = Arc::new(MemoryService::new(store.clone()));
        let svc = ImproverService::new(
            store.clone(),
            memory,
            settings,
            dir.path().to_path_buf(),
            vec![],
        );
        std::mem::forget(dir);
        (store, svc)
    }

    /// Contract: `interval == 0` means off — the loop must exit immediately.
    #[tokio::test]
    async fn zero_interval_exits_immediately() {
        let (_store, svc) = make_service().await;
        tokio::time::timeout(Duration::from_secs(1), svc.run_forever(None, 4, 0))
            .await
            .expect("run_forever must return immediately when interval is 0");
    }

    /// End-to-end hygiene pass against a real SQLite store: a merge decision
    /// updates the keeper, deletes the dropped row, and persists a mutation
    /// record per applied outcome.
    #[tokio::test]
    async fn hygiene_pass_applies_merge_and_records_mutations() {
        let (store, svc) = make_service().await;
        let scope = "u|a|";
        store
            .insert_memory(scope, "semantic", "deploys use docker", "h0", None, None, None)
            .await
            .unwrap();
        store
            .insert_memory(scope, "semantic", "deploys run on docker swarm", "h1", None, None, None)
            .await
            .unwrap();
        store
            .insert_memory(scope, "preference", "prefers terse answers", "h2", None, None, None)
            .await
            .unwrap();
        let top = store.search_memories(scope, "", 20).await.unwrap();
        assert_eq!(top.len(), 3);

        let llm: DynLlmProvider = Arc::new(MockLlm {
            response: r#"{"actions":[{"action":"merge","keep":2,"drop":[1],"content":"deploys use docker compose"}]}"#,
        });
        let applied = svc.hygiene_pass(&llm, scope, &top).await.unwrap();
        assert_eq!(applied, 2, "one update + one delete");

        let after = store.search_memories(scope, "", 20).await.unwrap();
        assert_eq!(after.len(), 2);
        let keeper = after.iter().find(|m| m.content.contains("compose")).unwrap();
        let dropped_gone = !after.iter().any(|m| m.content.contains("swarm"));
        assert!(dropped_gone);

        let muts = store.list_memory_mutations(&keeper.id, 10).await.unwrap();
        assert_eq!(muts.len(), 1);
        assert_eq!(muts[0].action, "update");
        assert_eq!(muts[0].old_content.as_deref(), Some("deploys use docker"));
        assert_eq!(muts[0].new_content.as_deref(), Some("deploys use docker compose"));
    }

    /// Below-threshold scopes are skipped without spending LLM budget.
    #[tokio::test]
    async fn hygiene_tick_skips_small_scopes_without_llm_call() {
        let (store, svc) = make_service().await;
        let scope = "u|small|";
        store
            .insert_memory(scope, "semantic", "tiny scope", "h0", None, None, None)
            .await
            .unwrap();
        svc.watch_scope(scope);

        // A mock that FAILS on chat proves no call is attempted.
        struct ExplodingLlm;
        #[async_trait::async_trait]
        impl LlmProvider for ExplodingLlm {
            fn model(&self) -> &str {
                "boom"
            }
            fn embed_model(&self) -> Option<String> {
                None
            }
            fn embed_dims(&self) -> Option<i64> {
                None
            }
            async fn chat(&self, _: &[ChatMessage<'_>], _: f32, _: i64) -> Result<String> {
                Err(crate::error::Error::Other("must not be called".into()))
            }
            async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
                Ok(Vec::new())
            }
        }
        let llm: DynLlmProvider = Arc::new(ExplodingLlm);
        let mut budget = LlmBudget::new(5);
        let applied = svc.hygiene_tick(Some(&llm), &mut budget).await.unwrap();
        assert_eq!(applied, 0);
        assert_eq!(budget.remaining(), 5, "no calls spent below the threshold");
    }

    /// Refresh proposals land in log.md append-only, reusing today's heading.
    #[tokio::test]
    async fn staleness_scan_and_log_append_roundtrip() {
        let (store, svc) = make_service().await;
        let past = ts(7200);
        for (path, status, stale_after) in [
            ("stale-one.md", "published", Some(past.clone())),
            ("fresh-one.md", "published", None),
            ("draft-stale.md", "draft", Some(past.clone())),
            ("future-doc.md", "published", Some(ts(-9999))),
        ] {
            let mut input = crate::domain::DocumentInput {
                rel_path: path.into(),
                ..Default::default()
            };
            input.status = Some(status.to_string());
            input.stale_after = stale_after.clone();
            store.upsert_document(&input).await.unwrap();
        }
        let stale = svc.scan_stale().await.unwrap();
        let paths: Vec<&str> = stale.iter().map(|d| d.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["stale-one.md"], "only the truly stale doc");

        let entry = format_refresh_proposal(
            "stale-one.md",
            "Stale One",
            Some(&past),
            "Still accurate overall.",
        );
        svc.append_log_entry(&entry).unwrap();
        svc.append_log_entry(&entry).unwrap(); // second proposal, same day
        let log = std::fs::read_to_string(std::path::Path::new(&svc.wiki_root).join("log.md"))
            .unwrap();
        assert_eq!(log.matches(&format!("## {}", chrono::Utc::now().format("%Y-%m-%d"))).count(), 1, "date heading reused");
        assert_eq!(log.matches("**RefreshProposal**").count(), 2);
        assert!(log.ends_with('\n'));
    }

}
