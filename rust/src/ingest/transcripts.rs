//! Coding-agent transcript ingestion: incremental JSONL sync from Claude Code
//! and Codex rollout directories into the session/memory subsystem.
//!
//! Each transcript carries a persisted watermark
//! ([`crate::store::TranscriptWatermark`]) so a sync only parses lines past
//! the stored high-water mark. The watermark also stores a prefix hash (sha256
//! of the first non-empty line): if that hash changes while a watermark
//! exists, the file was truncated/rewritten and the sync restarts from line 0
//! (counted as a rescan). Parsed turns are batched into
//! [`SessionService::ingest_messages`] for fact extraction.

use crate::error::Result;
use crate::llm::provider::DynLlmProvider;
use crate::services::memory::sha256_hex;
use crate::services::sessions::{Session, SessionService};
use crate::store::{Store, TranscriptWatermark};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Conversation turns per [`SessionService::ingest_messages`] extraction call.
const BATCH_SIZE: usize = 20;

/// user_id owning memories imported from coding-agent transcripts.
const TRANSCRIPT_USER: &str = "transcript";

/// One parsed conversation turn ready for extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: String,
}

/// Counters for one sync run.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncStats {
    /// Transcript files examined.
    pub scanned: usize,
    /// Sessions created during this run.
    pub discovered_sessions: usize,
    /// Parsed conversation turns handed to fact extraction.
    pub messages_ingested: usize,
    /// Blank/unparsable lines inside the scanned ranges.
    pub skipped_lines: usize,
    /// Full rescans triggered by prefix-hash mismatches (rewritten files).
    pub rescans: usize,
}

pub struct TranscriptScanner {
    store: Arc<dyn Store>,
    base_dirs: Vec<PathBuf>,
    session: Arc<SessionService>,
    /// Optional LLM for extraction; `None` uses the heuristic classifier.
    llm: Option<DynLlmProvider>,
    /// filename stem → session id cache; the mapping itself is derived
    /// deterministically from the stem (see [`derive_session_id`]) so it
    /// survives restarts without extra state.
    sessions: Mutex<HashMap<String, String>>,
}

impl TranscriptScanner {
    /// Poison-recovering lock (mirrors `fs::watcher`); the cache is benign
    /// under poisoning.
    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn new(store: Arc<dyn Store>, base_dirs: Vec<PathBuf>, session: Arc<SessionService>) -> Self {
        Self {
            store,
            base_dirs,
            session,
            llm: None,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Attach an LLM provider so extraction uses the LLM path instead of the
    /// heuristic classifier. Optional; builder-style.
    pub fn with_llm(mut self, llm: Option<DynLlmProvider>) -> Self {
        self.llm = llm;
        self
    }

    /// Walk every root (env-expanded) collecting `*.jsonl` transcript files,
    /// sorted for deterministic ordering.
    pub fn discover(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in &self.base_dirs {
            let root = expand_root(root);
            if !root.is_dir() {
                continue;
            }
            walk_jsonl(&root, 0, &mut out);
        }
        out.sort();
        out.dedup();
        out
    }

    /// Discover transcripts and sync them all, inferring the tool label from
    /// each path (`.claude` → `claude`, `.codex` → `codex`, else `generic`).
    /// Entry point for the background sync loop.
    pub async fn sync_all(&self) -> Result<SyncStats> {
        let mut by_tool: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for path in self.discover() {
            by_tool.entry(infer_tool(&path)).or_default().push(path);
        }
        let mut total = SyncStats::default();
        for (tool, paths) in by_tool {
            let stats = self.sync_tool(&tool, &paths).await?;
            total.scanned += stats.scanned;
            total.discovered_sessions += stats.discovered_sessions;
            total.messages_ingested += stats.messages_ingested;
            total.skipped_lines += stats.skipped_lines;
            total.rescans += stats.rescans;
        }
        Ok(total)
    }

    /// Sync the given transcripts as one tool's format. Per-file failures are
    /// logged and skipped so one bad file cannot stall the loop.
    pub async fn sync_tool(&self, tool: &str, paths: &[PathBuf]) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        for path in paths {
            stats.scanned += 1;
            if let Err(err) = self.sync_file(tool, path, &mut stats).await {
                tracing::warn!(
                    tool,
                    path = %path.display(),
                    error = %err,
                    "transcript sync failed for file"
                );
            }
        }
        Ok(stats)
    }

    /// Watermark-aware incremental sync of one transcript file.
    ///
    /// Failure semantics are at-least-once: the watermark is only advanced
    /// after every batch ingested successfully, so an error re-parses the
    /// same range next time (extraction dedupes on normalized content).
    async fn sync_file(&self, tool: &str, path: &Path, stats: &mut SyncStats) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len() as i64;
        let path_str = path.to_string_lossy().to_string();
        let prefix_hash = first_nonempty_line(&lines).map(sha256_hex);

        let wm = self
            .store
            .get_watermark(tool, &path_str)
            .await?
            .unwrap_or(TranscriptWatermark {
                tool: tool.to_string(),
                transcript_path: path_str.clone(),
                last_line: 0,
                prefix_hash: None,
                last_synced_at: None,
            });

        // Decide where to resume. `last_line` counts consumed lines, i.e. the
        // next unparsed index.
        let mut start = wm.last_line.max(0);
        let prefix_matches = wm.prefix_hash.is_some() && wm.prefix_hash == prefix_hash;
        if prefix_matches {
            if total < wm.last_line {
                // Tail-truncated with intact history: nothing new to ingest.
                return Ok(());
            }
        } else if wm.last_line > 0 {
            // First line changed or is unverifiable while a watermark exists:
            // the file was rewritten — restart from zero and note the rescan.
            start = 0;
            stats.rescans += 1;
        }
        if start >= total {
            return Ok(()); // empty file or nothing new
        }

        let session_id = self.ensure_session(tool, path, stats).await?;

        // Parse new lines into bounded extraction batches. Unparsable shapes
        // count as skipped rather than failing the whole file.
        let mut batch: Vec<(String, String)> = Vec::with_capacity(BATCH_SIZE);
        for line in &lines[start as usize..] {
            match Self::parse_line(line) {
                Some(msg) => batch.push((msg.role, msg.content)),
                None => stats.skipped_lines += 1,
            }
            if batch.len() >= BATCH_SIZE {
                self.flush_batch(&session_id, tool, &mut batch, stats).await?;
            }
        }
        if !batch.is_empty() {
            self.flush_batch(&session_id, tool, &mut batch, stats).await?;
        }

        // Everything through EOF is consumed at this point (errors above
        // propagate before we get here).
        self.store
            .upsert_watermark(&TranscriptWatermark {
                tool: tool.to_string(),
                transcript_path: path_str,
                last_line: total,
                prefix_hash,
                last_synced_at: Some(chrono::Utc::now().to_rfc3339()),
            })
            .await
    }

    /// Ingest one bounded batch of turns and record the count.
    async fn flush_batch(
        &self,
        session_id: &str,
        tool: &str,
        batch: &mut Vec<(String, String)>,
        stats: &mut SyncStats,
    ) -> Result<()> {
        self.session
            .ingest_messages(session_id, tool, TRANSCRIPT_USER, batch, self.llm.as_ref())
            .await?;
        stats.messages_ingested += batch.len();
        batch.clear();
        Ok(())
    }

    /// Resolve (creating once) the session a transcript ingests into. The id
    /// derives deterministically from the filename stem, so the same
    /// transcript always maps to the same session across restarts.
    async fn ensure_session(
        &self,
        tool: &str,
        path: &Path,
        stats: &mut SyncStats,
    ) -> Result<String> {
        let key = file_stem(path);
        if let Some(id) = self.lock_sessions().get(&key) {
            return Ok(id.clone());
        }

        let derived = derive_session_id(path);
        let id = if self.store.get_session(&derived).await?.is_some() {
            derived
        } else {
            let session = Session {
                id: derived.clone(),
                agent_name: tool.to_string(),
                user_id: TRANSCRIPT_USER.to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                context_summary: None,
            };
            match self.store.insert_session(&session).await {
                Ok(()) => {
                    stats.discovered_sessions += 1;
                    derived
                }
                Err(err) => {
                    tracing::warn!(
                        session_id = %derived,
                        error = %err,
                        "derived session insert failed; falling back to generated id"
                    );
                    let (created, _) = self.session.create(tool, TRANSCRIPT_USER).await?;
                    created.id
                }
            }
        };

        self.lock_sessions().insert(key, id.clone());
        Ok(id)
    }

    /// Tolerant single-line probe across known transcript shapes:
    ///
    /// - Claude Code: `{"type":"user"|"assistant","message":{"role":..,
    ///   "content":".."|[{"type":"text","text":".."}]}}`
    /// - Codex rollout: `{"type":"message"|"response_item","role":..,
    ///   "content":[..]}`
    ///
    /// Unknown shapes (summaries, tool results, garbage) yield `None`; the
    /// caller counts them as skipped lines. Text-only turns are returned;
    /// turns whose content carries no text (e.g. pure tool calls) are skipped.
    pub fn parse_line(line: &str) -> Option<TranscriptMessage> {
        let v: Value = serde_json::from_str(line).ok()?;
        // Claude Code wraps the turn inside a "message" object; Codex items
        // may nest under "payload". Fall back to probing the top level.
        for candidate in [
            v.get("message"),
            v.get("payload"),
            Some(&v),
        ] {
            let Some(obj) = candidate.and_then(Value::as_object) else {
                continue;
            };
            if let Some(msg) = probe_turn(obj) {
                return Some(msg);
            }
        }
        None
    }
}

/// Probe one JSON object for a `{role, content}` conversation turn.
fn probe_turn(obj: &serde_json::Map<String, Value>) -> Option<TranscriptMessage> {
    let role = obj.get("role")?.as_str()?.trim();
    if role.is_empty() {
        return None;
    }
    let content = extract_text(obj.get("content"))?;
    Some(TranscriptMessage {
        role: role.to_string(),
        content,
    })
}

/// Flatten a `content` field into plain text: accepts a string or an array of
/// blocks carrying `"text"` fields (Claude Code / Codex block style). Returns
/// `None` for missing, non-text, or whitespace-only content.
fn extract_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// First non-empty (trimmed) line, used for the watermark prefix hash.
fn first_nonempty_line<'a>(lines: &[&'a str]) -> Option<&'a str> {
    lines.iter().map(|l| l.trim()).find(|l| !l.is_empty())
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Deterministic session id from a transcript filename stem: sanitized to
/// ascii alphanumerics/dashes, lowercased, capped at 40 chars, `sess-` prefixed.
fn derive_session_id(path: &Path) -> String {
    let clean: String = file_stem(path)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_lowercase();
    if clean.len() >= 4 {
        format!("sess-{}", &clean[..clean.len().min(40)])
    } else {
        format!("sess-{}", &ulid::Ulid::new().to_string()[..12].to_lowercase())
    }
}

/// Infer the tool label from a transcript path for [`Self::sync_all`].
fn infer_tool(path: &Path) -> String {
    for component in path.components() {
        let c = component.as_os_str().to_string_lossy();
        if c.contains(".claude") {
            return "claude".to_string();
        }
        if c.contains(".codex") {
            return "codex".to_string();
        }
    }
    "generic".to_string()
}

/// Expand a settings-style root (`~/.claude/projects`) against `$HOME`.
fn expand_root(root: &Path) -> PathBuf {
    let s = root.to_string_lossy();
    let expanded = if s == "~" {
        std::env::var("HOME").ok().map(PathBuf::from)
    } else if let Some(rest) = s.strip_prefix("~/") {
        std::env::var("HOME").ok().map(|home| PathBuf::from(home).join(rest))
    } else {
        None
    };
    expanded.unwrap_or_else(|| root.to_path_buf())
}

/// Recursive `*.jsonl` walk with a depth cap; symlinked dirs are never
/// followed (loop safety).
fn walk_jsonl(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let p = entry.path();
        if ft.is_dir() {
            dirs.push(p);
        } else if ft.is_file() && p.extension() == Some(OsStr::new("jsonl")) {
            out.push(p);
        }
    }
    dirs.sort();
    for d in dirs {
        walk_jsonl(&d, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::memory::MemoryScope;
    use crate::store::sqlite::SqliteStore;

    fn claude_line(role: &str, text: &str) -> String {
        serde_json::json!({
            "type": role,
            "message": {"role": role, "content": text}
        })
        .to_string()
    }

    async fn make_store() -> Arc<dyn Store> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcripts-test.db");
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.migrate().await.unwrap();
        std::mem::forget(dir);
        Arc::new(store)
    }

    fn test_settings(store: Arc<dyn Store>) -> Arc<crate::services::settings::SettingsService> {
        let config = crate::config::Config {
            wiki_root: "/tmp/wiki".into(),
            port: 0,
            host: "127.0.0.1".into(),
            api_keys: std::collections::HashMap::new(),
            public_read: false,
            db_path: String::new(),
            log_level: "info".into(),
            db_backend: "sqlite".into(),
            database_url: None,
            layout: "okf".into(),
            okf_strict: false,
            human_actors: Vec::new(),
            llm_base_url: None,
            llm_api_key: None,
            llm_model: "test-model".into(),
            llm_embed_model: None,
            embedding_dims: 1536,
            llm_distill: false,
            connector_poll_seconds: 300,
            rate_limit_rpm: 0,
        };
        Arc::new(crate::services::settings::SettingsService::new(store, config))
    }

    async fn make_scanner(root: &Path) -> (TranscriptScanner, Arc<dyn Store>) {
        let store = make_store().await;
        let settings = test_settings(store.clone());
        let session = SessionService::new(store.clone(), settings);
        (
            TranscriptScanner::new(store.clone(), vec![root.to_path_buf()], Arc::new(session)),
            store,
        )
    }

    #[test]
    fn parses_claude_string_and_block_content() {
        let msg = TranscriptScanner::parse_line(&claude_line("user", "The wiki uses cargo."))
            .unwrap();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "The wiki uses cargo.");

        let blocks = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "first part"},
                {"type": "tool_use", "name": "x"},
                {"type": "text", "text": "second part"}
            ]}
        })
        .to_string();
        let msg = TranscriptScanner::parse_line(&blocks).unwrap();
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "first part\nsecond part");
    }

    #[test]
    fn parses_codex_rollout_items() {
        let codex = serde_json::json!({
            "type": "response_item",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello from codex"}]
        })
        .to_string();
        let msg = TranscriptScanner::parse_line(&codex).unwrap();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "hello from codex");
    }

    #[test]
    fn unknown_and_empty_shapes_are_none() {
        // Not JSON.
        assert!(TranscriptScanner::parse_line("not json at all").is_none());
        // JSON but not a conversation turn.
        assert!(TranscriptScanner::parse_line(r#"{"type":"summary","summary":"x"}"#).is_none());
        // Turn with no textual content (pure tool call).
        assert!(TranscriptScanner::parse_line(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1"}]}}"#
        )
        .is_none());
        // Whitespace-only text.
        assert!(TranscriptScanner::parse_line(
            r#"{"type":"user","message":{"role":"user","content":"   "}}"#
        )
        .is_none());
    }

    /// Full lifecycle: fresh sync ingests everything, an append is picked up
    /// incrementally into the SAME session, and watermarks advance.
    #[tokio::test]
    async fn sync_append_is_incremental_within_one_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abcdef123.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\nnot json at all\n\n{}\n",
                claude_line("user", "The wiki build uses cargo for releases."),
                claude_line("assistant", "CI depends on docker for builds.")
            ),
        )
        .unwrap();
        let (scanner, store) = make_scanner(dir.path()).await;

        let stats = scanner.sync_tool("claude", &[path.clone()]).await.unwrap();
        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.discovered_sessions, 1);
        assert_eq!(stats.messages_ingested, 2);
        assert_eq!(stats.skipped_lines, 2);
        assert_eq!(stats.rescans, 0);

        let wm = store.get_watermark("claude", &path.to_string_lossy()).await.unwrap().unwrap();
        assert_eq!(wm.last_line, 4);
        assert!(wm.prefix_hash.is_some());

        // Memories land in the deterministic session derived from the stem.
        let scope = MemoryScope {
            user_id: TRANSCRIPT_USER.into(),
            agent_name: Some("claude".into()),
            // Facts persist at durable user|agent scope (session lineage
            // rides on source_session_id), so search WITHOUT a session id.
            session_id: None,
        };
        let memories = store
            .search_memories(&scope.scope_key(), "cargo", 10)
            .await
            .unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].source_session_id.as_deref(), Some("sess-abcdef123"));

        // Append one more turn: only the new line is ingested, no rescan, no
        // new session.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", claude_line("user", "The team prefers postgres over mysql.")).unwrap();
        drop(f);

        let stats = scanner.sync_tool("claude", &[path.clone()]).await.unwrap();
        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.messages_ingested, 1);
        assert_eq!(stats.skipped_lines, 0);
        assert_eq!(stats.rescans, 0);
        assert_eq!(stats.discovered_sessions, 0);

        let wm = store.get_watermark("claude", &path.to_string_lossy()).await.unwrap().unwrap();
        assert_eq!(wm.last_line, 5);

        let memories = store
            .search_memories(&scope.scope_key(), "postgres", 10)
            .await
            .unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].source_session_id.as_deref(), Some("sess-abcdef123"));
    }

    /// Truncating and rewriting the transcript (different first line) triggers
    /// a full rescan counted in `rescans`, with the watermark reset.
    #[tokio::test]
    async fn truncation_with_new_prefix_triggers_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rescan99.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                claude_line("user", "The deploy pipeline runs on github actions."),
                claude_line("assistant", "Ok."),
                claude_line("user", "The indexer depends on sqlite fts5.")
            ),
        )
        .unwrap();
        let (scanner, store) = make_scanner(dir.path()).await;

        let stats = scanner.sync_tool("claude", &[path.clone()]).await.unwrap();
        assert_eq!(stats.messages_ingested, 3);
        assert_eq!(stats.rescans, 0);

        // Rewrite with a different first line and fewer lines.
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                claude_line("user", "The rewritten flow uses rust only."),
                claude_line("assistant", "Understood.")
            ),
        )
        .unwrap();

        let stats = scanner.sync_tool("claude", &[path.clone()]).await.unwrap();
        assert_eq!(stats.rescans, 1);
        assert_eq!(stats.messages_ingested, 2);
        assert_eq!(stats.skipped_lines, 0);

        let wm = store.get_watermark("claude", &path.to_string_lossy()).await.unwrap().unwrap();
        assert_eq!(wm.last_line, 2);
    }

    /// A tail-truncated file with an INTACT prefix is skipped, not rescanned.
    #[tokio::test]
    async fn tail_truncation_with_intact_prefix_skips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tailcut45.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                claude_line("user", "The build server runs on nixos."),
                claude_line("assistant", "Noted."),
                claude_line("user", "Storage depends on zfs mirrors.")
            ),
        )
        .unwrap();
        let (scanner, store) = make_scanner(dir.path()).await;

        let stats = scanner.sync_tool("claude", &[path.clone()]).await.unwrap();
        assert_eq!(stats.messages_ingested, 3);

        // Drop the tail but keep the first line identical.
        let kept = std::fs::read_to_string(&path).unwrap();
        let first_two: Vec<&str> = kept.lines().take(2).collect();
        std::fs::write(&path, format!("{}\n", first_two.join("\n"))).unwrap();

        let stats = scanner.sync_tool("claude", &[path.clone()]).await.unwrap();
        assert_eq!(stats.messages_ingested, 0);
        assert_eq!(stats.rescans, 0);

        let wm = store.get_watermark("claude", &path.to_string_lossy()).await.unwrap().unwrap();
        assert_eq!(wm.last_line, 3, "watermark untouched when skipping");
    }

    #[test]
    fn derive_session_id_sanitizes_stems() {
        let p = Path::new("/tmp/Some Weird_Name-42.jsonl");
        assert_eq!(derive_session_id(p), "sess-someweirdname-42");
        let junk = Path::new("/tmp/__.jsonl");
        let id = derive_session_id(junk);
        assert!(id.starts_with("sess-"));
        assert_eq!(id.len(), 5 + 12);
    }

    #[tokio::test]
    async fn discover_finds_nested_jsonl_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("proj-a/sub")).unwrap();
        std::fs::write(dir.path().join("proj-a/s1.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.path().join("proj-a/sub/s2.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.path().join("proj-a/notes.md"), "nope").unwrap();
        let (scanner, _store) = make_scanner(dir.path()).await;

        let found = scanner.discover();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p.extension() == Some(OsStr::new("jsonl"))));
    }
}
