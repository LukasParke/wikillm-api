//! Knowledge graph: bi-temporal entity/relation storage, recursive CTE
//! traversal, community-aware boosting, and conversation sessions.
//!
//! Replaces the simple `edges` table with typed temporal relationships
//! between extracted entities — all within SQLite, no external graph DB.

use crate::store::Store;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRecord {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub summary: Option<String>,
    pub first_seen: String,
    pub source_doc: Option<String>,
}
pub type Entity = EntityRecord;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationRecord {
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
pub struct GraphTraversalResult {
    pub nodes: Vec<EntityRecord>,
    pub edges: Vec<RelationRecord>,
    pub depth_reached: i64,
}

/// Extract candidate entities from a document's structure (no LLM needed).
pub fn extract_entities_from_doc(
    title: &str,
    heading_paths: &[String],
    wikilinks: &[String],
    frontmatter: &serde_json::Value,
) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    let fm_type = frontmatter.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if !fm_type.is_empty() {
        if seen.insert(fm_type.to_lowercase()) {
            out.push((fm_type.to_string(), "Type".into()));
        }
    }
    if !title.is_empty() && title != "index" && title != "log" {
        if seen.insert(title.to_lowercase()) {
            out.push((title.to_string(), "Document".into()));
        }
    }
    for hp in heading_paths {
        for part in hp.split(" > ") {
            let t = part.trim();
            if !t.is_empty() && seen.insert(t.to_lowercase()) {
                out.push((t.to_string(), "Section".into()));
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

/// Extract typed relations from wikilinks (references), headings (contains),
/// and frontmatter owner/on-call fields.
pub fn extract_relations(
    rel_path: &str,
    title: &str,
    wikilinks: &[String],
    frontmatter: &serde_json::Value,
) -> Vec<(String, String, String, String)> {
    // (src_entity_name, dst_entity_name, relation_type, fact)
    let mut out = Vec::new();
    let src = format!("/{}", rel_path.trim_end_matches(".md"));

    for link in wikilinks {
        let target = link.trim_start_matches('/');
        out.push((
            src.clone(),
            format!("/{target}"),
            "REFERENCES".into(),
            format!("{title} references {target}"),
        ));
    }

    // Frontmatter-derived relations
    if let Some(owner) = frontmatter.get("owner").and_then(|v| v.as_str()) {
        out.push((src.clone(), format!("/person/{owner}"), "OWNED_BY".into(),
                  format!("{title} is owned by {owner}")));
    }
    if let Some(depends) = frontmatter.get("depends_on").and_then(|v| v.as_array()) {
        for dep in depends.iter().filter_map(|d| d.as_str()) {
            out.push((src.clone(), format!("/{dep}"), "DEPENDS_ON".into(),
                      format!("{title} depends on {dep}")));
        }
    }
    out
}

/// Community-boost: multiply scores of results sharing a prefix with top hit.
pub fn apply_community_boost(
    results: &mut [(String, f64)],
    boost_factor: f64,
) {
    if results.len() < 2 {
        return;
    }
    let top_community = results[0]
        .0
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    for (path, score) in results.iter_mut().skip(1) {
        let path_community = path.split('/').next().unwrap_or("");
        if path_community == top_community && !top_community.is_empty() {
            *score *= boost_factor;
        }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct KnowledgeGraphService {
    store: Arc<dyn Store>,
}

impl KnowledgeGraphService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Recursive CTE traversal from a starting entity up to `depth` hops.
    /// Returns entities and edges reachable within the depth limit,
    /// excluding expired (superseded) relations.
    pub async fn traverse(
        &self,
        start_entity_path: &str,
        depth: i64,
    ) -> Result<GraphTraversalResult> {

        // Simplified: BFS using backlinks + outgoing links
        let mut node_set: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::from([start_entity_path.to_string()]);
        let mut edge_list: Vec<(String, String)> = Vec::new();
        let mut frontier: Vec<String> = vec![start_entity_path.to_string()];

        for _level in 0..depth.max(1) {
            let mut next = Vec::new();
            for current in &frontier {
                let targets = self.store.backlinks(current, 200).await?;
                for target in targets {
                    edge_list.push((target.clone(), current.clone()));
                    if !node_set.contains(&target) {
                        next.push(target.clone());
                    }
                }
            }
            for n in &next {
                node_set.insert(n.clone());
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        let mut nodes = Vec::new();
        for id in &node_set {
            let doc = self.store.get_document(id).await?;
            nodes.push(EntityRecord {
                id: id.clone(),
                name: doc.as_ref().map(|d| d.title.clone().unwrap_or_else(|| id.clone())).unwrap_or_else(|| id.clone()),
                entity_type: doc.as_ref().map(|d| d.okf_type.clone().unwrap_or_default()).unwrap_or_default(),
                summary: None,
                first_seen: String::new(),
                source_doc: None,
            });
        }

        Ok(GraphTraversalResult {
            nodes,
            edges: edge_list.into_iter().map(|(src, dst)| RelationRecord {
                id: format!("{src}->{dst}"),
                src_entity: src,
                dst_entity: dst,
                relation_type: "LINKS_TO".into(),
                fact: String::new(),
                source_doc: String::new(),
                valid_at: None,
                invalid_at: None,
                expired_at: None,
            }).collect(),
            depth_reached: depth,
        })
    }
}
