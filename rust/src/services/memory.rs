//! Agent memory ledger: two-phase consolidation (dedup → LLM classification)
//! with scoped identity, typed memory, and mutation history.

use crate::store::Store;
use crate::error::Result;
use crate::llm::provider::DynLlmProvider;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    Semantic,
    Episodic,
    Procedural,
    Preference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryScope {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl MemoryScope {
    /// Canonical scope string for DB storage and query filtering.
    pub fn scope_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.user_id,
            self.agent_name.as_deref().unwrap_or(""),
            self.session_id.as_deref().unwrap_or("")
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMemory {
    pub id: String,
    pub scope_key: String,
    pub memory_type: MemoryType,
    pub content: String,
    pub created_at: String,
    pub accessed_at: String,
    pub access_count: i64,
    #[serde(default)]
    pub source_session_id: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct MutationRecord {
    pub memory_id: String,
    pub old_content: Option<String>,
    pub new_content: Option<String>,
    pub action: String,
    pub timestamp: String,
}

pub struct MemoryService {
    store: Arc<dyn Store>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConsolidationAction {
    Add,
    Update(String), // existing memory ID
    Delete(String), // existing memory ID to remove
    Noop,
}

const CONSOLIDATION_SYSTEM: &str = "You are a memory consolidation engine. Given a NEW memory and EXISTING memories (numbered), decide for each existing memory whether the new memory: ADD (new information), UPDATE <id> (enriches/supersedes), DELETE <id> (directly contradicts), or NOOP (equivalent). Respond ONLY with JSON: {\"actions\":[{\"action\":\"add\"},{\"action\":\"update\",\"id\":1},{\"action\":\"delete\",\"id\":2},{\"action\":\"noop\",\"id\":3}]}";

impl MemoryService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Two-phase store: dedup gate → LLM consolidation → apply decisions.
    /// Returns the list of mutation records for audit.
    /// `source_session_id`/`source_ref` capture provenance when the memory
    /// originates from a conversation session or an external reference.
    pub async fn store_memory(
        &self,
        scope: &MemoryScope,
        memory_type: MemoryType,
        content: &str,
        llm: Option<&DynLlmProvider>,
        source_session_id: Option<&str>,
        source_ref: Option<&str>,
    ) -> Result<Vec<MutationRecord>> {
        let scope_key = scope.scope_key();
        let normalized = normalize_content(content);
        let hash = sha256_hex(&normalized);

        // Phase 1: hash dedup fast path
        // Scope-wide top-K candidates: LIKE-matching the new content would
        // miss paraphrases, which is exactly what consolidation must catch.
        let existing = self.store.search_memories(&scope_key, "", 10).await?;
        let mut mutations: Vec<MutationRecord> = Vec::new();

        // Check exact duplicates — noop outcomes are persisted too.
        for mem in &existing {
            if sha256_hex(&normalize_content(&mem.content)) == hash {
                let rec = MutationRecord {
                    memory_id: mem.id.clone(),
                    old_content: None,
                    new_content: None,
                    action: "noop".into(),
                    timestamp: now_rfc3339(),
                };
                self.persist_mutation(&rec).await?;
                mutations.push(rec);
                return Ok(mutations);
            }
        }

        // Phase 2: LLM consolidation (if LLM available)
        if let Some(llm) = llm {
            if !existing.is_empty() {
                let existing_list: Vec<String> = existing
                    .iter()
                    .enumerate()
                    .map(|(i, m)| format!("[{}] {}", i, m.content))
                    .collect();
                let user_prompt = format!(
                    "New memory: {}\n\nExisting memories:\n{}",
                    content,
                    existing_list.join("\n")
                );
                let messages: Vec<(&str, &str)> = vec![
                    ("system", CONSOLIDATION_SYSTEM),
                    ("user", &user_prompt),
                ];
                let raw = llm.chat(&messages, 0.0, 500).await?;
                if let Some(actions) = parse_consolidation_response(&raw, &existing) {
                    for action in &actions {
                        match action {
                            ConsolidationAction::Delete(id) => {
                                if let Some(old) = existing.iter().find(|m| &m.id == id) {
                                    let rec = MutationRecord {
                                        memory_id: id.clone(),
                                        old_content: Some(old.content.clone()),
                                        new_content: None,
                                        action: "delete".into(),
                                        timestamp: now_rfc3339(),
                                    };
                                    self.store.delete_memory(id).await?;
                                    self.persist_mutation(&rec).await?;
                                    mutations.push(rec);
                                }
                            }
                            ConsolidationAction::Update(id) => {
                                if let Some(old) = existing.iter().find(|m| &m.id == id) {
                                    let rec = MutationRecord {
                                        memory_id: id.clone(),
                                        old_content: Some(old.content.clone()),
                                        new_content: Some(content.to_string()),
                                        action: "update".into(),
                                        timestamp: now_rfc3339(),
                                    };
                                    // Hash param must be sha256 of the (normalized)
                                    // content — never the raw text itself.
                                    self.store.update_memory(id, content, &hash).await?;
                                    self.persist_mutation(&rec).await?;
                                    mutations.push(rec);
                                }
                            }
                            ConsolidationAction::Noop => {
                                let rec = MutationRecord {
                                    memory_id: String::new(),
                                    old_content: None,
                                    new_content: None,
                                    action: "noop".into(),
                                    timestamp: now_rfc3339(),
                                };
                                self.persist_mutation(&rec).await?;
                                mutations.push(rec);
                            }
                            ConsolidationAction::Add => {}
                        }
                    }
                    // Double-write fix: an Update already merged the new content
                    // into an existing row — never also insert a fresh copy.
                    if should_fall_through_to_add(&actions) {
                        let new_id = self
                            .insert_memory(
                                &scope_key,
                                &memory_type,
                                content,
                                &normalized,
                                source_session_id,
                                source_ref,
                                None,
                            )
                            .await?;
                        let rec = MutationRecord {
                            memory_id: new_id,
                            old_content: None,
                            new_content: Some(content.to_string()),
                            action: "add".into(),
                            timestamp: now_rfc3339(),
                        };
                        self.persist_mutation(&rec).await?;
                        mutations.push(rec);
                    }
                    return Ok(mutations);
                }
            }
        }

        // No LLM or no existing memories: just add
        let new_id = self
            .insert_memory(
                &scope_key,
                &memory_type,
                content,
                &normalized,
                source_session_id,
                source_ref,
                None,
            )
            .await?;
        let rec = MutationRecord {
            memory_id: new_id,
            old_content: None,
            new_content: Some(content.to_string()),
            action: "add".into(),
            timestamp: now_rfc3339(),
        };
        self.persist_mutation(&rec).await?;
        mutations.push(rec);
        Ok(mutations)
    }

    /// Append-only audit trail: every outcome (add/update/delete/noop) is
    /// recorded via `store.record_memory_mutation` right after its store op.
    async fn persist_mutation(&self, rec: &MutationRecord) -> Result<()> {
        let m = crate::store::MemoryMutation {
            id: new_mutation_id(),
            memory_id: rec.memory_id.clone(),
            action: rec.action.clone(),
            old_content: rec.old_content.clone(),
            new_content: rec.new_content.clone(),
            timestamp: rec.timestamp.clone(),
        };
        self.store.record_memory_mutation(&m).await
    }

    async fn insert_memory(
        &self,
        scope_key: &str,
        memory_type: &MemoryType,
        content: &str,
        normalized: &str,
        source_session_id: Option<&str>,
        source_ref: Option<&str>,
        promote_candidate: Option<bool>,
    ) -> Result<String> {
        let hash = sha256_hex(normalized);
        self.store
            .insert_memory(
                scope_key,
                memory_type_to_str(memory_type),
                content,
                &hash,
                source_session_id,
                source_ref,
                promote_candidate,
            )
            .await
    }

    pub async fn search_memories(
        &self,
        scope: &MemoryScope,
        query: &str,
        limit: i64,
    ) -> Result<Vec<AgentMemory>> {
        let scope_key = scope.scope_key();
        self.store.search_memories(&scope_key, query, limit).await
    }
}

pub(crate) fn memory_type_to_str(t: &MemoryType) -> &'static str {
    match t {
        MemoryType::Semantic => "semantic",
        MemoryType::Episodic => "episodic",
        MemoryType::Procedural => "procedural",
        MemoryType::Preference => "preference",
    }
}

pub(crate) fn normalize_content(content: &str) -> String {
    content
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `mm-` + 12-char ulid-style suffix (matches the `wh-`/`sess-` id pattern).
fn new_mutation_id() -> String {
    format!("mm-{}", &ulid::Ulid::new().to_string()[..12].to_lowercase())
}

/// Double-write gate: fall through to an Add only when the consolidation
/// decided no Update (which already merged the content) and the model either
/// said Add or produced no usable actions.
fn should_fall_through_to_add(actions: &[ConsolidationAction]) -> bool {
    !actions
        .iter()
        .any(|a| matches!(a, ConsolidationAction::Update(_)))
        && (actions.iter().any(|a| matches!(a, ConsolidationAction::Add)) || actions.is_empty())
}

fn parse_consolidation_response(
    raw: &str,
    existing: &[AgentMemory],
) -> Option<Vec<ConsolidationAction>> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    let parsed: Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let actions = parsed.get("actions")?.as_array()?;

    let mut out = Vec::new();
    for action in actions {
        let action_str = action.get("action")?.as_str()?;
        match action_str {
            "add" => out.push(ConsolidationAction::Add),
            "noop" => out.push(ConsolidationAction::Noop),
            "update" | "delete" => {
                let id_num = action.get("id")?.as_u64()? as usize;
                if let Some(mem) = existing.get(id_num) {
                    if action_str == "update" {
                        out.push(ConsolidationAction::Update(mem.id.clone()));
                    } else {
                        out.push(ConsolidationAction::Delete(mem.id.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::provider::{ChatMessage, LlmProvider};
    use crate::store::sqlite::SqliteStore;

    #[test]
    fn preference_variant_serializes_lowercase() {
        assert_eq!(memory_type_to_str(&MemoryType::Preference), "preference");
        let json = serde_json::to_string(&MemoryType::Preference).unwrap();
        assert_eq!(json, r#""preference""#);
        let back: MemoryType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemoryType::Preference);
    }

    #[test]
    fn normalize_and_sha256_are_dedup_friendly() {
        assert_eq!(normalize_content("Hello,  WORLD!!"), "hello world");
        let a = sha256_hex(&normalize_content("Deployment uses Docker."));
        let b = sha256_hex(&normalize_content("deployment uses docker"));
        assert_eq!(a, b);
        assert_ne!(a, sha256_hex("different"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn update_blocks_add_fallthrough() {
        assert!(!should_fall_through_to_add(&[
            ConsolidationAction::Update("m1".into())
        ]));
        assert!(!should_fall_through_to_add(&[
            ConsolidationAction::Update("m1".into()),
            ConsolidationAction::Add,
        ]));
        // Delete alone never triggered an Add (no Add action, non-empty).
        assert!(!should_fall_through_to_add(&[
            ConsolidationAction::Delete("m1".into())
        ]));
        assert!(should_fall_through_to_add(&[ConsolidationAction::Add]));
        assert!(should_fall_through_to_add(&[]));
        assert!(!should_fall_through_to_add(&[ConsolidationAction::Noop]));
    }

    #[test]
    fn parse_consolidation_maps_indices_to_ids() {
        let existing = vec![
            AgentMemory {
                id: "mem-aaa".into(),
                scope_key: "u|a|".into(),
                memory_type: MemoryType::Semantic,
                content: "one".into(),
                created_at: String::new(),
                accessed_at: String::new(),
                access_count: 0,
                source_session_id: None,
                source_ref: None,
            },
            AgentMemory {
                id: "mem-bbb".into(),
                scope_key: "u|a|".into(),
                memory_type: MemoryType::Semantic,
                content: "two".into(),
                created_at: String::new(),
                accessed_at: String::new(),
                access_count: 0,
                source_session_id: None,
                source_ref: None,
            },
        ];
        let actions = parse_consolidation_response(
            r#"sure! {"actions":[{"action":"update","id":1},{"action":"delete","id":0},{"action":"add"}]}"#,
            &existing,
        )
        .unwrap();
        assert_eq!(
            actions,
            vec![
                ConsolidationAction::Update("mem-bbb".into()),
                ConsolidationAction::Delete("mem-aaa".into()),
                ConsolidationAction::Add,
            ]
        );
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
        async fn chat(&self, _messages: &[ChatMessage<'_>], _t: f32, _mt: i64) -> crate::error::Result<String> {
            Ok(self.response.to_string())
        }
        async fn embed(&self, _texts: &[String]) -> crate::error::Result<Vec<Vec<f32>>> {
            Ok(Vec::new())
        }
    }

    async fn make_store() -> Arc<dyn Store> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-test.db");
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.migrate().await.unwrap();
        std::mem::forget(dir);
        Arc::new(store)
    }

    fn mem_id_hash(content: &str) -> String {
        sha256_hex(&normalize_content(content))
    }

    /// Bug fix regression: an Update decision must merge into the existing row
    /// and NOT additionally insert the new content as a second row.
    #[tokio::test]
    async fn update_decision_does_not_double_write() {
        let store = make_store().await;
        let old = "deployment pipeline uses docker";
        store
            .insert_memory("u|a|", "semantic", old, &mem_id_hash(old), None, None, None)
            .await
            .unwrap();

        let svc = MemoryService::new(store.clone());
        let llm: DynLlmProvider = Arc::new(MockLlm {
            response: r#"{"actions":[{"action":"update","id":0}]}"#,
        });
        let scope = MemoryScope {
            user_id: "u".into(),
            agent_name: Some("a".into()),
            session_id: None,
        };
        let recs = svc
            .store_memory(
                &scope,
                MemoryType::Semantic,
                "deployment pipeline uses docker compose",
                Some(&llm),
                Some("sess-abc123"),
                None,
            )
            .await
            .unwrap();

        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].action, "update");

        let all = store.search_memories("u|a|", "docker", 10).await.unwrap();
        assert_eq!(all.len(), 1, "update must not create a second row");
        assert!(all[0].content.contains("compose"));

        // The applied outcome is persisted as a MemoryMutation.
        let muts = store.list_memory_mutations(&all[0].id, 10).await.unwrap();
        assert_eq!(muts.len(), 1);
        assert_eq!(muts[0].action, "update");
        assert_eq!(muts[0].old_content.as_deref(), Some(old));
    }

    /// Exact-duplicate fast path persists a noop mutation.
    #[tokio::test]
    async fn exact_duplicate_persists_noop_mutation() {
        let store = make_store().await;
        let content = "ci runs on github actions";
        store
            .insert_memory("u|a|", "semantic", content, &mem_id_hash(content), None, None, None)
            .await
            .unwrap();

        let svc = MemoryService::new(store.clone());
        let scope = MemoryScope {
            user_id: "u".into(),
            agent_name: Some("a".into()),
            session_id: None,
        };
        let recs = svc
            .store_memory(&scope, MemoryType::Semantic, content, None, None, None)
            .await
            .unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].action, "noop");
        assert!(!recs[0].memory_id.is_empty());

        let muts = store.list_memory_mutations(&recs[0].memory_id, 10).await.unwrap();
        assert_eq!(muts.len(), 1);
        assert_eq!(muts[0].action, "noop");
    }
}
