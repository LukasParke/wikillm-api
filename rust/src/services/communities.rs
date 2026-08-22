//! Community detection over the entity graph.
//!
//! Detection itself is label propagation in [`crate::services::kg::
//! detect_communities`]; this service memoizes results via
//! [`KnowledgeGraphService::communities`] (60s TTL) and exposes the read
//! surface for the future `/v1/communities` routes (wired by INTEGRATION-A).

use crate::domain::DocumentRecord;
use crate::error::Result;
use crate::services::kg::KnowledgeGraphService;
use crate::store::Store;
use serde::Serialize;
use std::sync::Arc;

/// Summary row for a community listing.
#[derive(Debug, Clone, Serialize)]
pub struct CommunitySummary {
    pub id: String,
    pub label: String,
    pub size: usize,
}

pub struct CommunitiesService {
    kg: Arc<KnowledgeGraphService>,
    store: Arc<dyn Store>,
}

impl CommunitiesService {
    pub fn new(kg: Arc<KnowledgeGraphService>, store: Arc<dyn Store>) -> Self {
        Self { kg, store }
    }

    /// All detected communities with member counts.
    pub async fn list(&self) -> Result<Vec<CommunitySummary>> {
        Ok(self
            .kg
            .communities()
            .await?
            .into_iter()
            .map(|c| CommunitySummary {
                id: c.id,
                label: c.label,
                size: c.member_paths.len(),
            })
            .collect())
    }

    /// Documents belonging to a community, by community id. Members that do
    /// not resolve to a stored document (e.g. frontmatter-only entities like
    /// `/person/...`) are skipped.
    pub async fn docs(&self, id: &str) -> Result<Option<Vec<DocumentRecord>>> {
        let community = match self.kg.communities().await?.iter().find(|c| c.id == id) {
            Some(c) => c.clone(),
            None => return Ok(None),
        };
        let mut docs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for path in &community.member_paths {
            for candidate in [
                format!("{path}.md"),
                format!("/{path}.md"),
                path.clone(),
                format!("/{path}"),
            ] {
                if let Some(doc) = self.store.get_document(&candidate).await? {
                    if seen.insert(doc.rel_path.clone()) {
                        docs.push(doc);
                    }
                    break;
                }
            }
        }
        Ok(Some(docs))
    }
}
