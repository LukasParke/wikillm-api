//! Temporal knowledge graph: entity extraction, bi-temporal relations,
//! community detection over the entity graph.

use crate::store::Store;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub summary: Option<String>,
    pub first_seen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEdge {
    pub id: String,
    pub src_entity: String,
    pub dst_entity: String,
    pub relation_type: String,
    pub fact: String,
    pub source_doc: String,
    pub valid_at: Option<String>,
    pub invalid_at: Option<String>,
    pub expired_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Community {
    pub id: String,
    pub label: String,
    pub entity_count: usize,
}

/// Extract candidate entities from document headings and wikilinks.
pub fn extract_entities(title: &str, heading_paths: &[String], wikilinks: &[String]) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    if !title.is_empty() && title != "index" && title != "log" {
        if seen.insert(title.to_lowercase()) {
            out.push((title.to_string(), "Document".into()));
        }
    }
    for hp in heading_paths {
        for part in hp.split(" > ") {
            let trimmed = part.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
                out.push((trimmed.to_string(), "Section".into()));
            }
        }
    }
    for link in wikilinks {
        let target = link.trim_start_matches('/').trim_end_matches(".md");
        if !target.is_empty() && seen.insert(target.to_lowercase()) {
            out.push((target.to_string(), "Referenced".into()));
        }
    }
    out
}

pub struct KgService {
    store: Arc<dyn Store>,
}

impl KgService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Extract entities from a document and upsert them.
    pub async fn extract_and_upsert_entities(
        &self,
        title: &str,
        heading_paths: &[String],
        wikilinks: &[String],
        rel_path: &str,
    ) -> Result<Vec<Entity>> {
        let candidates = extract_entities(title, heading_paths, wikilinks);
        let mut entities = Vec::new();
        for (name, entity_type) in candidates {
            let id = format!("ent-{}", sha_short(&name));
            self.store.upsert_entity(&id, &name, &entity_type, rel_path).await?;
            entities.push(Entity {
                id,
                name,
                entity_type,
                summary: None,
                first_seen: chrono::Utc::now().to_rfc3339(),
            });
        }
        Ok(entities)
    }

    /// Run label propagation community detection on the entity graph.
    pub async fn detect_communities(&self) -> Result<Vec<Community>> {
        // Simple label propagation on the co-occurrence graph
        let entities = self.store.list_entities().await?;
        if entities.is_empty() {
            return Ok(Vec::new());
        }
        // Build adjacency from shared documents
        let mut adj: std::collections::HashMap<String, Vec<String>> = Default::default();
        let mut labels: std::collections::HashMap<String, String> = Default::default();
        for e in &entities {
            labels.insert(e.id.clone(), e.id.clone());
        }
        // Group by source doc to find co-occurring entities
        let mut by_doc: std::collections::HashMap<String, Vec<String>> = Default::default();
        for e in &entities {
            by_doc.entry(e.name.clone()).or_default().push(e.id.clone());
        }
        // For simplicity, connect entities that appear in the same named group
        for group in by_doc.values() {
            for i in 0..group.len() {
                for j in i + 1..group.len() {
                    adj.entry(group[i].clone()).or_default().push(group[j].clone());
                    adj.entry(group[j].clone()).or_default().push(group[i].clone());
                }
            }
        }

        // Label propagation iterations
        let ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();
        for _ in 0..10 {
            let mut changed = false;
            for id in &ids {
                let neighbors = adj.get(id).cloned().unwrap_or_default();
                if neighbors.is_empty() {
                    continue;
                }
                let mut counts: std::collections::HashMap<String, usize> = Default::default();
                for n in &neighbors {
                    *counts.entry(labels.get(n).cloned().unwrap_or_default()).or_default() += 1;
                }
                let best = counts.into_iter().max_by_key(|(_, c)| *c).map(|(l, _)| l).unwrap_or_default();
                if labels.get(id).map(|l| l.clone()) != Some(best.clone()) {
                    labels.insert(id.clone(), best);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Aggregate communities
        let mut communities: std::collections::HashMap<String, Vec<&Entity>> = Default::default();
        for e in &entities {
            let label = labels.get(&e.id).cloned().unwrap_or_else(|| e.id.clone());
            communities.entry(label).or_default().push(e);
        }
        Ok(communities
            .into_iter()
            .filter(|(_, members)| members.len() >= 2)
            .map(|(label, members)| Community {
                id: format!("comm-{}", sha_short(&label)),
                label: members.first().map(|m| m.name.clone()).unwrap_or(label),
                entity_count: members.len(),
            })
            .collect())
    }
}

fn sha_short(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))[..12].to_string()
}
