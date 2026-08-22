//! Conversation session layer: scoped memory for chat agents with automatic
//! fact extraction and context loading.

use crate::store::Store;
use crate::error::Result;
use crate::llm::provider::DynLlmProvider;
use crate::services::memory::{
    memory_type_to_str, normalize_content, sha256_hex, MemoryScope, MemoryType,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent_name: String,
    pub user_id: String,
    pub created_at: String,
    pub context_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionContext {
    pub session: Session,
    /// Relevant memories loaded at session start
    pub memories: Vec<String>,
    /// Recent wiki documents related to this agent's scope
    pub recent_docs: Vec<String>,
}

/// One fact the extractor decided to apply (stored as an agent memory).
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedFact {
    pub content: String,
    pub memory_type: MemoryType,
    pub promote_candidate: bool,
}

/// STRICT JSON array contract for the LLM extraction path.
const EXTRACTION_SYSTEM: &str = "You are a memory extraction engine. Given conversation messages, extract durable facts worth remembering long-term. Respond ONLY with a STRICT JSON array: [{\"content\": \"...\", \"type\": \"semantic|episodic|procedural|preference\", \"promote_candidate\": true|false}]. Rules: store stable preferences, constraints, and reusable procedures; SKIP volatile values (counts, prices, statuses) — those stay in the transcript/wiki; mark \"promote_candidate\": true ONLY for durable team-relevant knowledge worth a standalone wiki page, otherwise false. If nothing qualifies, respond with [].";

/// LLM extraction input cap (~4k chars).
const EXTRACTION_INPUT_CAP: usize = 4000;
/// Safety bound on facts extracted from a single batch.
const MAX_FACTS_PER_EXTRACTION: usize = 10;

/// A fact after normalization: known type, non-empty content.
struct NormalizedFact {
    content: String,
    memory_type: MemoryType,
    promote_candidate: bool,
}

/// Raw item shape of the STRICT JSON array the LLM is told to emit.
#[derive(Debug, Deserialize)]
struct ExtractionItem {
    content: String,
    #[serde(rename = "type")]
    memory_type: String,
    #[serde(default)]
    promote_candidate: bool,
}

pub struct SessionService {
    store: Arc<dyn Store>,
    settings: Arc<crate::services::settings::SettingsService>,
}

impl SessionService {
    pub fn new(
        store: Arc<dyn Store>,
        settings: Arc<crate::services::settings::SettingsService>,
    ) -> Self {
        Self { store, settings }
    }

    /// Create a session and auto-load relevant memories.
    pub async fn create(
        &self,
        agent_name: &str,
        user_id: &str,
    ) -> Result<(Session, Vec<String>)> {
        let session = Session {
            id: format!("sess-{}", &ulid::Ulid::new().to_string()[..12].to_lowercase()),
            agent_name: agent_name.to_string(),
            user_id: user_id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            context_summary: None,
        };
        self.store.insert_session(&session).await?;

        // Load scoped memories for context injection
        let scope = MemoryScope {
            user_id: user_id.to_string(),
            agent_name: Some(agent_name.to_string()),
            session_id: None,
        };
        let scope_key = scope.scope_key();
        let memories = self
            .store
            .search_memories(&scope_key, "", 20)
            .await
            .unwrap_or_default();
        let memory_texts: Vec<String> = memories.iter().map(|m| m.content.clone()).collect();

        Ok((session, memory_texts))
    }

    /// Extract facts from a conversation message and store them as memories.
    ///
    /// Uses the LLM extraction path when a provider is configured, with the
    /// heuristic classifier as fallback (LLM error, unparseable output, or no
    /// provider). Returns the list of applied memories.
    pub async fn extract_and_store(
        &self,
        session_id: &str,
        agent_name: &str,
        user_id: &str,
        message: &str,
        llm: Option<&DynLlmProvider>,
    ) -> Result<Vec<ExtractedFact>> {
        let text = cap_chars(message, EXTRACTION_INPUT_CAP);
        // Durable facts outlive their session: persist at user|agent scope
        // so later sessions and memory search resurface them. Lineage back
        // to the originating conversation rides on `source_session_id`.
        let scope = MemoryScope {
            user_id: user_id.to_string(),
            agent_name: Some(agent_name.to_string()),
            session_id: None,
        };

        if let Some(llm) = llm {
            if let Some(facts) = llm_extract(llm, text).await {
                return self.persist_facts(session_id, &scope, facts).await;
            }
            // LLM unavailable or emitted garbage → heuristic fallback below.
        }

        let facts = heuristic_extract(text);
        self.persist_facts(session_id, &scope, facts).await
    }

    /// Batch variant for transcript ingestion: `messages` are (role, content)
    /// pairs combined into ONE extraction call. Callers batch at most ~20
    /// messages per call; input is capped at [`EXTRACTION_INPUT_CAP`].
    pub async fn ingest_messages(
        &self,
        session_id: &str,
        agent_name: &str,
        user_id: &str,
        messages: &[(String, String)],
        llm: Option<&DynLlmProvider>,
    ) -> Result<Vec<ExtractedFact>> {
        let mut joined = String::new();
        for (role, content) in messages {
            let line = format!("{role}: {content}\n");
            if joined.chars().count() + line.chars().count() > EXTRACTION_INPUT_CAP {
                break;
            }
            joined.push_str(&line);
        }
        if joined.is_empty() {
            return Ok(Vec::new());
        }
        self.extract_and_store(session_id, agent_name, user_id, &joined, llm)
            .await
    }

    /// Insert each extracted fact with session provenance and persist an
    /// add-mutation for every stored row.
    async fn persist_facts(
        &self,
        session_id: &str,
        scope: &MemoryScope,
        facts: Vec<NormalizedFact>,
    ) -> Result<Vec<ExtractedFact>> {
        let scope_key = scope.scope_key();
        let mut applied = Vec::with_capacity(facts.len());
        for fact in facts {
            let hash = sha256_hex(&normalize_content(&fact.content));
            let memory_id = self
                .store
                .insert_memory(
                    &scope_key,
                    memory_type_to_str(&fact.memory_type),
                    &fact.content,
                    &hash,
                    Some(session_id),
                    None,
                    Some(fact.promote_candidate),
                )
                .await?;
            // Append-only audit trail; the returned row id gives the add
            // mutation a real lineage target.
            self.record_mutation(&memory_id, "add", None, Some(&fact.content))
                .await?;
            applied.push(ExtractedFact {
                content: fact.content,
                memory_type: fact.memory_type,
                promote_candidate: fact.promote_candidate,
            });
        }
        Ok(applied)
    }

    /// Persist one append-only mutation row via the shared audit trail.
    async fn record_mutation(
        &self,
        memory_id: &str,
        action: &str,
        old_content: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<()> {
        let m = crate::store::MemoryMutation {
            id: format!("mm-{}", ulid::Ulid::new().to_string().to_lowercase()),
            memory_id: memory_id.to_string(),
            action: action.to_string(),
            old_content: old_content.map(str::to_string),
            new_content: new_content.map(str::to_string),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.store.record_memory_mutation(&m).await
    }
}

/// Run the LLM extraction path. Returns `None` when the provider call fails
/// or the output cannot be parsed as the STRICT JSON array (caller falls
/// back to heuristics); `Some(vec)` (possibly empty) on a valid parse.
async fn llm_extract(llm: &DynLlmProvider, text: &str) -> Option<Vec<NormalizedFact>> {
    let messages: Vec<(&str, &str)> = vec![
        ("system", EXTRACTION_SYSTEM),
        ("user", text),
    ];
    let raw = match llm.chat(&messages, 0.0, 1000).await {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(error = %err, "LLM extraction failed; using heuristic fallback");
            return None;
        }
    };
    match parse_extraction_items(&raw) {
        Some(items) => Some(items),
        None => {
            tracing::warn!("LLM extraction output was not a valid JSON array; using heuristic fallback");
            None
        }
    }
}

/// Parse the STRICT JSON array of extracted facts, tolerating surrounding
/// prose. Unknown types and malformed items are skipped; results are capped
/// at [`MAX_FACTS_PER_EXTRACTION`].
fn parse_extraction_items(raw: &str) -> Option<Vec<NormalizedFact>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let arr = parsed.as_array()?;

    let mut out = Vec::new();
    for item in arr {
        let Ok(item) = serde_json::from_value::<ExtractionItem>(item.clone()) else {
            continue;
        };
        let content = item.content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        let memory_type = match item.memory_type.as_str() {
            "semantic" => MemoryType::Semantic,
            "episodic" => MemoryType::Episodic,
            "procedural" => MemoryType::Procedural,
            "preference" => MemoryType::Preference,
            _ => continue,
        };
        out.push(NormalizedFact {
            content,
            memory_type,
            promote_candidate: item.promote_candidate,
        });
        if out.len() >= MAX_FACTS_PER_EXTRACTION {
            break;
        }
    }
    Some(out)
}

/// Heuristic fallback: sentences containing factual keywords are kept as
/// semantic memories (identical to the previous behavior, minus the removed
/// md5 dedup hash — dedup now uses the shared normalize+sha256 helpers).
fn heuristic_extract(text: &str) -> Vec<NormalizedFact> {
    let mut out = Vec::new();
    for sentence in text.split(|c: char| c == '.' || c == '\n') {
        let trimmed = sentence.trim();
        if trimmed.len() < 10 || trimmed.len() > 500 {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if ["is", "uses", "depends", "prefers", "runs on", "deployed"]
            .iter()
            .any(|k| lower.contains(k))
        {
            out.push(NormalizedFact {
                content: trimmed.to_string(),
                memory_type: MemoryType::Semantic,
                promote_candidate: false,
            });
            if out.len() >= MAX_FACTS_PER_EXTRACTION {
                break;
            }
        }
    }
    out
}

/// Truncate on a char boundary at `max_chars`.
fn cap_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::store::sqlite::SqliteStore;

    #[test]
    fn parses_strict_array_with_prose_wrapping() {
        let raw = r#"Here you go:
        [{"content": "Luke prefers dark mode", "type": "preference", "promote_candidate": false},
         {"content": "Deploy via make release", "type": "procedural", "promote_candidate": true}]"#;
        let facts = parse_extraction_items(raw).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].memory_type, MemoryType::Preference);
        assert!(!facts[0].promote_candidate);
        assert_eq!(facts[1].memory_type, MemoryType::Procedural);
        assert!(facts[1].promote_candidate);
    }

    #[test]
    fn skips_unknown_types_and_bad_items_but_stays_some() {
        let raw = r#"[{"content": "", "type": "semantic"},
                      {"content": "x", "type": "vibes"},
                      {"content": "Team uses Rust", "type": "semantic"}]"#;
        let facts = parse_extraction_items(raw).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "Team uses Rust");
    }

    #[test]
    fn non_array_output_is_none() {
        assert!(parse_extraction_items("no json here").is_none());
        assert!(parse_extraction_items("{\"content\": \"obj not array\"}").is_none());
    }

    #[test]
    fn heuristic_keeps_factual_sentences_only() {
        let facts = heuristic_extract(
            "The build uses cargo. Hi. The team prefers postgres over mysql.",
        );
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.memory_type == MemoryType::Semantic));
        assert!(facts.iter().all(|f| !f.promote_candidate));
    }

    #[test]
    fn cap_chars_respects_char_boundaries() {
        assert_eq!(cap_chars("hello", 3), "hel");
        assert_eq!(cap_chars("héllo", 3), "hél");
        assert_eq!(cap_chars("ab", 10), "ab");
    }

    async fn make_store() -> Arc<dyn Store> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions-test.db");
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

    async fn make_session_service() -> (SessionService, Arc<dyn Store>) {
        let store = make_store().await;
        let settings = test_settings(store.clone());
        (SessionService::new(store.clone(), settings), store)
    }

    /// Heuristic extraction stores memories carrying session provenance and
    /// persists an add-mutation per outcome.
    #[tokio::test]
    async fn extraction_stores_provenance_and_mutations() {
        let (svc, store) = make_session_service().await;

        let applied = svc
            .extract_and_store(
                "sess-abc123",
                "build-bot",
                "luke",
                "The deploy pipeline runs on github actions. Small talk.",
                None,
            )
            .await
            .unwrap();

        assert_eq!(applied.len(), 1);
        assert!(applied[0].content.contains("github actions"));

        let scope = MemoryScope {
            user_id: "luke".into(),
            agent_name: Some("build-bot".into()),
            session_id: None,
        };
        let memories = store
            .search_memories(&scope.scope_key(), "deploy", 10)
            .await
            .unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].source_session_id.as_deref(),
            Some("sess-abc123"),
            "stored memories must carry source_session_id provenance"
        );
    }

    /// ingest_messages combines a batch into one extraction run.
    #[tokio::test]
    async fn ingest_messages_batches_and_extracts() {
        let (svc, _store) = make_session_service().await;

        let applied = svc
            .ingest_messages(
                "sess-batch01",
                "build-bot",
                "luke",
                &[
                    ("user".into(), "CI depends on docker for builds.".into()),
                    ("assistant".into(), "Noted.".into()),
                ],
                None,
            )
            .await
            .unwrap();
        assert!(!applied.is_empty());
    }
}
