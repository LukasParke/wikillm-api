//! Link-graph traversal over the edges table + document outgoing links.

use crate::error::Result;
use crate::store::Store;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct GraphNode {
    pub rel_path: String,
    pub title: Option<String>,
    pub exists: bool,
}

#[derive(Debug, Serialize)]
pub struct GraphEdge {
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Serialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

pub struct GraphService {
    store: Arc<dyn Store>,
}

impl GraphService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    pub async fn neighbors(&self, rel_path: &str, depth: i64) -> Result<GraphView> {
        let mut edges: BTreeMap<String, GraphEdge> = BTreeMap::new();
        let mut frontier: BTreeSet<String> = BTreeSet::from([rel_path.to_string()]);
        let mut visited: BTreeSet<String> = frontier.clone();

        for _ in 0..depth.max(1) {
            let mut next = BTreeSet::new();
            for current in &frontier {
                let outgoing = match self.store.get_document(current).await? {
                    Some(doc) => doc
                        .outgoing_links
                        .iter()
                        .map(|link| link.trim_start_matches('/').to_string())
                        .filter(|t| !t.is_empty() && t != current.as_str())
                        .collect::<Vec<_>>(),
                    None => Vec::new(),
                };
                for target in outgoing {
                    edges.insert(format!("{current}->{target}"), GraphEdge {
                        src: current.clone(),
                        dst: target.clone(),
                    });
                    if !visited.contains(&target) {
                        next.insert(target);
                    }
                }
                for source in self.store.backlinks(current, 500).await? {
                    edges.insert(format!("{source}->{current}"), GraphEdge {
                        src: source.clone(),
                        dst: current.clone(),
                    });
                    if !visited.contains(&source) {
                        next.insert(source);
                    }
                }
            }
            for n in &next {
                visited.insert(n.clone());
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        let mut node_ids: BTreeSet<String> = BTreeSet::from([rel_path.to_string()]);
        for (_, edge) in &edges {
            node_ids.insert(edge.src.clone());
            node_ids.insert(edge.dst.clone());
        }
        let mut nodes = Vec::new();
        for id in node_ids {
            let doc = self.store.get_document(&id).await?;
            nodes.push(GraphNode {
                rel_path: id.clone(),
                title: doc.as_ref().map(|d| d.title.clone()).flatten(),
                exists: doc.is_some(),
            });
        }
        Ok(GraphView { nodes, edges: edges.into_values().collect() })
    }
}
