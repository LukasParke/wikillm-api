//! Knowledge graph: bi-temporal entity/relation storage, typed traversal
//! with wikilink fallback, label-propagation community detection.
//!
//! Replaces the simple `edges` table with typed temporal relationships
//! between extracted entities — all within SQLite, no external graph DB.
use crate::store::Store;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// A detected community over the union of wikilink edges and active
/// (non-expired) typed relations. Members are normalized paths: no leading
/// `/`, no `.md` suffix.
#[derive(Debug, Clone, Serialize)]
pub struct Community {
    pub id: String,
    /// Title of the highest-degree member.
    pub label: String,
    pub member_paths: Vec<String>,
}

/// How long [`KnowledgeGraphService::communities`] memoizes detection output.
const COMMUNITY_CACHE_TTL: Duration = Duration::from_secs(60);

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

/// Normalize an entity/doc path into the shared node namespace: no leading
/// `/`, no `.md` suffix.
fn norm_path(p: &str) -> String {
    let p = p.trim().trim_start_matches('/');
    p.strip_suffix(".md").unwrap_or(p).to_string()
}

fn add_edge(adj: &mut HashMap<String, BTreeSet<String>>, a: &str, b: &str) {
    if a == b {
        return;
    }
    adj.entry(a.to_string()).or_default().insert(b.to_string());
    adj.entry(b.to_string()).or_default().insert(a.to_string());
}

/// Label-propagation community detection over UNION(wikilink edges, active
/// relation_edges). Runs up to ~10 iterations or until labels stabilize.
/// Deterministic: nodes are visited in sorted order and label ties break to
/// the smallest label. Single-node groups are dropped; output is sorted by
/// size (desc), then label.
pub async fn detect_communities(store: &Arc<dyn Store>) -> Result<Vec<Community>> {
    let mut adjacency: HashMap<String, BTreeSet<String>> = HashMap::new();
    // Node key -> display title, for labeling communities.
    let mut titles: HashMap<String, String> = HashMap::new();

    // Wikilink edges: every edge src -> dst appears as a backlink of dst, so
    // scanning backlinks per document covers the full edge set. Document
    // keys are kept for the relation probe below.
    let mut doc_keys: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = store
            .list_documents(&crate::domain::ListOptions::default(), 500, cursor.as_deref())
            .await?;
        for doc in &page.items {
            let key = norm_path(&doc.rel_path);
            titles.insert(key.clone(), doc.title.clone().unwrap_or_else(|| key.clone()));
            doc_keys.push(key.clone());
            for src in store.backlinks(&doc.rel_path, 500).await? {
                let s = norm_path(&src);
                titles.entry(s.clone()).or_insert_with(|| {
                    s.rsplit('/').next().unwrap_or(&s).to_string()
                });
                add_edge(&mut adjacency, &s, &key);
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    // Active typed relations. Probe by entity id AND name, plus every
    // document key (bare and `/`-prefixed), because the pipeline keys
    // relations by normalized document paths even when no entity row exists.
    // The store filters expired_at IS NOT NULL; one global seen-set dedupes
    // since each edge is returned from both of its endpoints.
    let mut seen: HashSet<String> = HashSet::new();
    let mut probe_keys: Vec<String> = Vec::new();
    for entity in store.list_entities().await? {
        let ekey = norm_path(&entity.name);
        titles.entry(ekey.clone()).or_insert_with(|| entity.name.clone());
        probe_keys.push(entity.id.clone());
        probe_keys.push(entity.name.clone());
    }
    for key in &doc_keys {
        probe_keys.push(key.clone());
        probe_keys.push(format!("/{}", key));
    }
    for probe in &probe_keys {
        for rel in store.get_relations_for_entity(probe, 500).await? {
            if !seen.insert(rel.id.clone()) {
                continue;
            }
            let s = norm_path(&rel.src_entity);
            let d = norm_path(&rel.dst_entity);
            titles.entry(s.clone()).or_insert_with(|| {
                s.rsplit('/').next().unwrap_or(&s).to_string()
            });
            titles.entry(d.clone()).or_insert_with(|| {
                d.rsplit('/').next().unwrap_or(&d).to_string()
            });
            add_edge(&mut adjacency, &s, &d);
        }
    }
    if adjacency.len() < 2 {
        return Ok(Vec::new());
    }

    // Initial label = own node; propagate asynchronously up to 10 rounds.
    let mut nodes: Vec<String> = adjacency.keys().cloned().collect();
    nodes.sort();
    let empty = BTreeSet::new();
    let degree = |n: &str| adjacency.get(n).map_or(0, |s| s.len());
    let mut labels: HashMap<String, String> =
        nodes.iter().map(|n| (n.clone(), n.clone())).collect();
    for _ in 0..10 {
        let mut changed = false;
        for node in &nodes {
            let best = {
                let neighbors = adjacency.get(node).unwrap_or(&empty);
                let mut tally: HashMap<&str, usize> = HashMap::new();
                for n in neighbors {
                    *tally.entry(labels[n].as_str()).or_insert(0) += 1;
                }
                tally.into_iter()
                    .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                    .map(|(label, _)| label.to_string())
            };
            if let Some(best) = best {
                if best != labels[node] {
                    labels.insert(node.clone(), best);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for node in &nodes {
        groups.entry(labels[node].clone()).or_default().push(node.clone());
    }

    let mut communities: Vec<Community> = groups
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|mut members| {
            members.sort();
            // Label = highest-degree member's title; ties -> first alphabetically.
            let head = members
                .iter()
                .max_by_key(|m| (degree(m), std::cmp::Reverse((*m).clone())))
                .cloned()
                .unwrap_or_else(|| members[0].clone());
            Community {
                id: format!("comm-{}", &ulid::Ulid::new().to_string()[..8].to_lowercase()),
                label: titles.get(&head).cloned().unwrap_or(head),
                member_paths: members,
            }
        })
        .collect();
    communities.sort_by(|a, b| {
        b.member_paths
            .len()
            .cmp(&a.member_paths.len())
            .then_with(|| a.label.cmp(&b.label))
    });
    Ok(communities)
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct KnowledgeGraphService {
    store: Arc<dyn Store>,
    community_cache: Mutex<Option<(Instant, Vec<Community>)>>,
}

impl KnowledgeGraphService {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            community_cache: Mutex::new(None),
        }
    }

    /// Communities over the entity graph, memoized for
    /// [`COMMUNITY_CACHE_TTL`]. Detection is [`detect_communities`].
    pub async fn communities(&self) -> Result<Vec<Community>> {
        {
            let guard = self.community_cache.lock().expect("community cache poisoned");
            if let Some((at, cached)) = guard.as_ref() {
                if at.elapsed() < COMMUNITY_CACHE_TTL {
                    return Ok(cached.clone());
                }
            }
        }
        let fresh = detect_communities(&self.store).await?;
        *self.community_cache.lock().expect("community cache poisoned") =
            Some((Instant::now(), fresh.clone()));
        Ok(fresh)
    }

    /// Traversal from a starting entity up to `depth` hops. When the seed has
    /// rows in `relation_edges`, BFS runs over those typed relations honoring
    /// `expired_at` (the store only returns active edges); otherwise it falls
    /// back to wikilink-edge traversal exactly as before.
    pub async fn traverse(
        &self,
        start_entity_path: &str,
        depth: i64,
    ) -> Result<GraphTraversalResult> {
        let seed = self.store.get_relations_for_entity(start_entity_path, 200).await?;
        if seed.is_empty() {
            return self.traverse_wikilinks(start_entity_path, depth).await;
        }

        let mut node_set: BTreeSet<String> =
            BTreeSet::from([start_entity_path.to_string()]);
        let mut edges: Vec<RelationRecord> = Vec::new();
        let mut seen_edges: HashSet<String> = HashSet::new();
        let mut frontier: Vec<String> = vec![start_entity_path.to_string()];

        for _level in 0..depth.max(1) {
            let mut next = Vec::new();
            for current in &frontier {
                // Matches both directions; expired relations are filtered out
                // by the store (`expired_at IS NULL`).
                for rel in self.store.get_relations_for_entity(current, 200).await? {
                    if !seen_edges.insert(rel.id.clone()) {
                        continue;
                    }
                    let other = if rel.src_entity == *current {
                        rel.dst_entity.clone()
                    } else {
                        rel.src_entity.clone()
                    };
                    edges.push(rel);
                    if node_set.insert(other.clone()) {
                        next.push(other);
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        // Resolve node records from the entity table (by id or name), falling
        // back to document lookups for endpoints never registered as entities.
        let entities = self.store.list_entities().await?;
        let by_key: HashMap<&str, &EntityRecord> = entities
            .iter()
            .flat_map(|e| [(e.id.as_str(), e), (e.name.as_str(), e)])
            .collect();

        let mut nodes = Vec::new();
        for id in &node_set {
            if let Some(e) = by_key.get(id.as_str()) {
                nodes.push((*e).clone());
                continue;
            }
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
            edges,
            depth_reached: depth,
        })
    }

    /// Legacy traversal over wikilink edges (backlinks), unchanged behavior.
    async fn traverse_wikilinks(
        &self,
        start_entity_path: &str,
        depth: i64,
    ) -> Result<GraphTraversalResult> {
        let mut node_set: BTreeSet<String> =
            BTreeSet::from([start_entity_path.to_string()]);
        let mut edge_list: Vec<(String, String)> = Vec::new();
        let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
        let mut frontier: Vec<String> = vec![start_entity_path.to_string()];

        for _level in 0..depth.max(1) {
            let mut next = Vec::new();
            for current in &frontier {
                // Undirected walk: inbound via the edges table's backlink
                // index, outbound via the document's outgoing_links. Each
                // undirected pair is recorded once (first-seen direction).
                let mut targets = self.store.backlinks(current, 200).await?;
                if let Ok(Some(doc)) = self.store.get_document(current).await {
                    targets.extend(doc.outgoing_links.iter().cloned());
                }
                for target in targets {
                    let pair = if target.as_str() <= current.as_str() {
                        (target.clone(), current.clone())
                    } else {
                        (current.clone(), target.clone())
                    };
                    if seen_pairs.insert(pair) {
                        edge_list.push((target.clone(), current.clone()));
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DocKind, DocumentInput, VerifiedEntry};
    use crate::services::communities::CommunitiesService;
    use crate::store::sqlite::SqliteStore;

    async fn make_store() -> Arc<dyn Store> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kg-test.db");
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.migrate().await.unwrap();
        std::mem::forget(dir);
        Arc::new(store)
    }

    fn doc(rel_path: &str, title: &str) -> DocumentInput {
        DocumentInput {
            rel_path: rel_path.to_string(),
            kind: DocKind::Page,
            origin: "wiki".into(),
            title: Some(title.to_string()),
            summary: None,
            body: format!("# {title}\n"),
            frontmatter: serde_json::json!({}),
            word_count: 2,
            outgoing_links: vec![],
            hash: "a".repeat(64),
            mtime: 1_700_000_000_000,
            content_type: Some("text/markdown".into()),
            okf_type: Some("Page".into()),
            tags: vec![],
            status: Some("stable".into()),
            stale_after: None,
            resource: None,
            generated_by: Some("human:test".into()),
            generated_at: Some("2026-01-01T00:00:00Z".into()),
            verified: Some(vec![VerifiedEntry { by: "human:test".into(), at: "2026-01-02T00:00:00Z".into() }]),
            provenance: None,
            updated_at: None,
            updated_by: None,
        }
    }

    fn rel(id: &str, src: &str, dst: &str, expired_at: Option<String>) -> RelationRecord {
        RelationRecord {
            id: id.to_string(),
            src_entity: src.to_string(),
            dst_entity: dst.to_string(),
            relation_type: "REFERENCES".into(),
            fact: format!("{src} references {dst}"),
            source_doc: src.to_string(),
            valid_at: None,
            invalid_at: None,
            expired_at,
        }
    }

    #[tokio::test]
    async fn traverse_uses_typed_relations_and_skips_expired() {
        let store = make_store().await;
        for (p, t) in [("wiki/a.md", "A"), ("wiki/b.md", "B"), ("wiki/c.md", "C")] {
            store.upsert_document(&doc(p, t)).await.unwrap();
        }
        store.upsert_entity("e-a", "wiki/a", "Document", "wiki/a.md").await.unwrap();
        store.upsert_entity("e-b", "wiki/b", "Document", "wiki/b.md").await.unwrap();
        store.upsert_entity("e-c", "wiki/c", "Document", "wiki/c.md").await.unwrap();
        // Active a -> b, expired b -> c.
        store.upsert_relation(&rel("r1", "/wiki/a", "/wiki/b", None)).await.unwrap();
        store.upsert_relation(&rel("r2", "/wiki/b", "/wiki/c", Some("2026-01-01T00:00:00Z".into()))).await.unwrap();

        let svc = KnowledgeGraphService::new(store.clone());
        let res = svc.traverse("/wiki/a", 3).await.unwrap();
        let ids: Vec<&str> = res.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"/wiki/a") && ids.contains(&"/wiki/b"));
        assert!(!ids.contains(&"/wiki/c"), "expired relation must not be traversed");
        assert_eq!(res.edges.len(), 1);
        assert_eq!(res.edges[0].relation_type, "REFERENCES");
    }

    #[tokio::test]
    async fn traverse_falls_back_to_wikilinks_without_typed_edges() {
        let store = make_store().await;
        let mut a = doc("wiki/a.md", "A");
        a.outgoing_links = vec!["wiki/b.md".into()];
        let mut d = doc("wiki/b.md", "B");
        d.outgoing_links = vec!["wiki/c.md".into()];
        store.upsert_document(&a).await.unwrap();
        store.upsert_document(&d).await.unwrap();
        store.upsert_document(&doc("wiki/c.md", "C")).await.unwrap();
        store.replace_edges("wiki/b.md", &["wiki/c.md".into()]).await.unwrap();
        store.replace_edges("wiki/a.md", &["wiki/b.md".into()]).await.unwrap();

        let svc = KnowledgeGraphService::new(store.clone());
        let res = svc.traverse("wiki/a.md", 2).await.unwrap();
        assert!(res.nodes.iter().any(|n| n.name == "C"));
        assert_eq!(res.edges.len(), 2);
        assert!(res.edges.iter().all(|e| e.relation_type == "LINKS_TO"));
    }

    #[tokio::test]
    async fn detect_communities_groups_and_ignores_expired() {
        let store = make_store().await;
        for (p, t) in [
            ("wiki/rust.md", "Rust"),
            ("wiki/tokio.md", "Tokio"),
            ("wiki/async.md", "Async"),
            ("wiki/chef.md", "Chef"),
            ("wiki/recipes.md", "Recipes"),
        ] {
            store.upsert_document(&doc(p, t)).await.unwrap();
        }
        // Cluster 1 via wikilinks, cluster 2 via active typed relations.
        store.replace_edges("wiki/tokio.md", &["wiki/rust.md".into()]).await.unwrap();
        store.replace_edges("wiki/async.md", &["wiki/rust.md".into()]).await.unwrap();
        store.upsert_relation(&rel("r1", "/wiki/chef", "/wiki/recipes", None)).await.unwrap();
        // Expired typed relation must not bridge the clusters.
        store.upsert_relation(&rel("r2", "/wiki/recipes", "/wiki/rust", Some("2026-01-01T00:00:00Z".into()))).await.unwrap();

        let communities = detect_communities(&store).await.unwrap();
        assert_eq!(communities.len(), 2, "got: {communities:?}");
        let sizes: Vec<usize> = communities.iter().map(|c| c.member_paths.len()).collect();
        assert_eq!(sizes, vec![3, 2]);
        let rust_comm = communities.iter().find(|c| c.member_paths.contains(&"wiki/rust".to_string())).unwrap();
        assert_eq!(rust_comm.member_paths.len(), 3);
        assert_eq!(rust_comm.label, "Rust", "highest-degree member titles the community");
    }

    #[tokio::test]
    async fn communities_service_lists_and_resolves_docs() {
        let store = make_store().await;
        for (p, t) in [("wiki/x.md", "X"), ("wiki/y.md", "Y")] {
            store.upsert_document(&doc(p, t)).await.unwrap();
        }
        store.replace_edges("wiki/x.md", &["wiki/y.md".into()]).await.unwrap();

        let svc = CommunitiesService::new(Arc::new(KnowledgeGraphService::new(store.clone())), store.clone());
        let list = svc.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].size, 2);

        let docs = svc.docs(&list[0].id).await.unwrap().expect("community exists");
        let paths: Vec<&str> = docs.iter().map(|d| d.rel_path.as_str()).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"wiki/x.md") && paths.contains(&"wiki/y.md"));

        assert!(svc.docs("comm-nonexistent").await.unwrap().is_none());
    }
}
