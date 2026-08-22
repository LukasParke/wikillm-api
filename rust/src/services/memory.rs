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
    pub async fn store_memory(
        &self,
        scope: &MemoryScope,
        memory_type: MemoryType,
        content: &str,
        llm: Option<&DynLlmProvider>,
    ) -> Result<Vec<MutationRecord>> {
        let scope_key = scope.scope_key();
        let normalized = normalize_content(content);
        let hash = sha256_hex(&normalized);

        // Phase 1: hash dedup fast path
        let existing = self.store.search_memories(&scope_key, content, 10).await?;
        let mut mutations: Vec<MutationRecord> = Vec::new();

        // Check exact duplicates
        for mem in &existing {
            if sha256_hex(&normalize_content(&mem.content)) == hash {
                mutations.push(MutationRecord {
                    memory_id: mem.id.clone(),
                    old_content: None,
                    new_content: None,
                    action: "noop".into(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
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
                                    mutations.push(MutationRecord {
                                        memory_id: id.clone(),
                                        old_content: Some(old.content.clone()),
                                        new_content: None,
                                        action: "delete".into(),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    });
                                    self.store.delete_memory(id).await?;
                                }
                            }
                            ConsolidationAction::Update(id) => {
                                if let Some(old) = existing.iter().find(|m| &m.id == id) {
                                    mutations.push(MutationRecord {
                                        memory_id: id.clone(),
                                        old_content: Some(old.content.clone()),
                                        new_content: Some(content.to_string()),
                                        action: "update".into(),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    });
                                    self.store
                                        .update_memory(id, content, &normalized)
                                        .await?;
                                }
                            }
                            ConsolidationAction::Noop => {
                                mutations.push(MutationRecord {
                                    memory_id: String::new(),
                                    old_content: None,
                                    new_content: None,
                                    action: "noop".into(),
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                });
                            }
                            ConsolidationAction::Add => {}
                        }
                    }
                    // If any action was ADD or if all were NOOP, still add if no update/delete matched
                    let has_add = actions.iter().any(|a| matches!(a, ConsolidationAction::Add));
                    if has_add || actions.is_empty() {
                        self.insert_memory(&scope_key, memory_type.clone(), content, &normalized).await?;
                        mutations.push(MutationRecord {
                            memory_id: String::new(),
                            old_content: None,
                            new_content: Some(content.to_string()),
                            action: "add".into(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                    return Ok(mutations);
                }
            }
        }

        // No LLM or no existing memories: just add
        self.insert_memory(&scope_key, memory_type.clone(), content, &normalized).await?;
        mutations.push(MutationRecord {
            memory_id: String::new(),
            old_content: None,
            new_content: Some(content.to_string()),
            action: "add".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
        Ok(mutations)
    }

    async fn insert_memory(
        &self,
        scope_key: &str,
        memory_type: MemoryType,
        content: &str,
        normalized: &str,
    ) -> Result<()> {
        let hash = sha256_hex(normalized);
        self.store
            .insert_memory(
                &scope_key.to_string(),
                &memory_type_to_str(&memory_type).to_string(),
                content,
                &hash,
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

fn memory_type_to_str(t: &MemoryType) -> &'static str {
    match t {
        MemoryType::Semantic => "semantic",
        MemoryType::Episodic => "episodic",
        MemoryType::Procedural => "procedural",
    }
}

fn normalize_content(content: &str) -> String {
    content
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))
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


