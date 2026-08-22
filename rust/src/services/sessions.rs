//! Conversation session layer: scoped memory for chat agents with automatic
//! fact extraction and context loading.

use crate::store::Store;
use crate::error::Result;
use crate::services::memory::MemoryScope;
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
    pub async fn extract_and_store(
        &self,
        session_id: &str,
        agent_name: &str,
        user_id: &str,
        message: &str,
    ) -> Result<usize> {
        // Simple heuristic extraction: sentences with "is", "uses", "prefers",
        // "depends on" are likely factual statements worth remembering
        let mut stored = 0;
        for sentence in message.split(|c: char| c == '.' || c == '\n') {
            let trimmed = sentence.trim();
            if trimmed.len() < 10 || trimmed.len() > 500 {
                continue;
            }
            let lower = trimmed.to_lowercase();
            if ["is", "uses", "depends", "prefers", "runs on", "deployed"]
                .iter()
                .any(|k| lower.contains(k))
            {
                let scope = MemoryScope {
                    user_id: user_id.to_string(),
                    agent_name: Some(agent_name.to_string()),
                    session_id: Some(session_id.to_string()),
                };
                let _ = self
                    .store
                    .insert_memory(
                        &scope.scope_key(),
                        "semantic",
                        trimmed,
                        &md5(trimmed),
                    )
                    .await;
                stored += 1;
            }
        }
        Ok(stored)
    }
}

fn md5(s: &str) -> String {
    // simple hash for dedup (md5 not needed for security)
    format!("{:x}", {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    })
}
