//! Personalized PageRank over the link graph for multi-hop retrieval.
//! Seeds from top hybrid-search hits; propagates through wikilink edges.

use crate::store::Store;
use crate::error::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Run personalized PageRank seeded from initial document scores.
/// Returns `(rel_path, score)` pairs for documents not in the seed set,
/// sorted descending by score. These are multi-hop neighbors that pure
/// similarity search misses.
pub async fn ppr_expand(
    store: &Arc<dyn Store>,
    seeds: &[(String, f64)],
    damping: f64,
    iterations: usize,
    max_results: usize,
) -> Result<Vec<(String, f64)>> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }

    // Build adjacency list from edges originating at any known node
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_nodes: HashSet<String> = HashSet::new();

    let mut frontier: Vec<String> = seeds.iter().map(|(p, _)| p.clone()).collect();
    let mut visited = frontier.iter().cloned().collect::<HashSet<_>>();
    // Expand frontier one hop to discover the local graph structure
    for node in &frontier {
        let targets = store.backlinks(node, 200).await?;
        for t in &targets {
            adjacency.entry(t.clone()).or_default().push(node.clone());
            all_nodes.insert(t.clone());
            adjacency.entry(node.clone()).or_default().push(t.clone());
            all_nodes.insert(t.clone());
        }
    }
    // Also get outgoing links from the pipeline's edge storage
    let mut discovered: Vec<String> = Vec::new();
    for node in &frontier {
        let outgoing = store.backlinks(node, 200).await?;
        for t in outgoing {
            if !visited.contains(&t) {
                visited.insert(t.clone());
                discovered.push(t);
            }
        }
    }
    frontier.extend(discovered);

    // Initialize scores
    let seed_map: HashMap<&str, f64> =
        seeds.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let total_seed_score: f64 = seeds.iter().map(|(_, s)| s).sum();
    let n = all_nodes.len().max(1) as f64;

    let mut scores: HashMap<String, f64> = all_nodes
        .iter()
        .map(|node| {
            let init = seed_map.get(node.as_str()).copied().unwrap_or(0.0);
            (node.clone(), init / total_seed_score.max(1e-10))
        })
        .collect();

    // Power iteration
    for _ in 0..iterations {
        let mut next_scores: HashMap<String, f64> = HashMap::new();
        let mut dangling_mass = 0.0;

        for (node, score) in &scores {
            let targets = adjacency.get(node).map(Vec::as_slice).unwrap_or(&[]);
            if targets.is_empty() {
                dangling_mass += score;
                continue;
            }
            let share = score / targets.len() as f64;
            for target in targets {
                *next_scores.entry(target.clone()).or_insert(0.0) += share;
            }
        }

        // Distribute dangling mass + teleport uniformly
        for node in all_nodes.iter() {
            let entry = next_scores.entry(node.clone()).or_insert(0.0);
            *entry += dangling_mass / n + (1.0 - damping) / n;
            *entry *= damping;
        }
        scores = next_scores;
    }

    // Return non-seed results sorted desc
    let seed_set: HashSet<&str> = seeds.iter().map(|(p, _)| p.as_str()).collect();
    let mut out: Vec<(String, f64)> = scores
        .into_iter()
        .filter(|(path, _)| !seed_set.contains(path.as_str()))
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(max_results);
    Ok(out)
}
