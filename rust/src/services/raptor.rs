//! RAPTOR-style recursive summarization tree: cluster chunks, summarize
//! clusters via LLM, store summaries as additional searchable chunks.

use crate::error::Result;
use crate::llm::provider::DynLlmProvider;

pub struct RaptorTree {
    pub levels: Vec<Vec<ChunkSummary>>,
}

#[derive(Debug, Clone)]
pub struct ChunkSummary {
    pub level: i32,
    pub content: String,
    pub children_count: usize,
}

const SUMMARY_SYSTEM: &str = "Summarize the following text passages into a single concise paragraph that captures the key information. Focus on facts, relationships, and actionable knowledge.";

/// Minimum cosine similarity for a chunk to join an existing cluster.
const CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.3;

/// Build a RAPTOR tree over chunks. Each level clusters similar chunks and
/// produces LLM summaries until fewer than 2 items remain or max_depth hit.
pub async fn build_raptor_tree(
    chunk_contents: &[String],
    llm: &DynLlmProvider,
    max_depth: i32,
    max_cluster_size: usize,
) -> Result<RaptorTree> {
    let mut levels = Vec::new();
    let mut current = chunk_contents.to_vec();
    let mut level = 0;

    while current.len() >= 2 && level < max_depth {
        // Prefer embedding-similarity clustering when embeddings are
        // available; fall back to positional slicing otherwise (e.g. no
        // embed model configured or the embedding call failed).
        let clusters: Vec<Vec<usize>> = match llm.embed(&current).await {
            Ok(vectors) if vectors.len() == current.len() => {
                cluster_by_similarity(&vectors, CLUSTER_SIMILARITY_THRESHOLD, max_cluster_size)
            }
            _ => positional_clusters(current.len(), max_cluster_size),
        };

        let mut next_level = Vec::new();
        for cluster in &clusters {
            let combined = cluster
                .iter()
                .filter_map(|&i| current.get(i))
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            let truncated = combined.chars().take(4000).collect::<String>();
            let messages: Vec<(&str, &str)> = vec![
                ("system", SUMMARY_SYSTEM),
                ("user", &truncated),
            ];
            let summary = match llm.chat(&messages, 0.3, 300).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            next_level.push(ChunkSummary {
                level,
                content: format!("Summary of {} passages:\n{}", cluster.len(), summary),
                children_count: cluster.len(),
            });
        }

        current = next_level.iter().map(|s| s.content.clone()).collect();
        levels.push(next_level);
        level += 1;
    }

    Ok(RaptorTree { levels })
}

/// Convert tree summaries to searchable chunk inputs.
pub fn tree_to_chunks(tree: &RaptorTree) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    for (level_idx, level) in tree.levels.iter().enumerate() {
        for item in level {
            out.push((level_idx as i32 + 1000, format!("[Level {} Summary]\n{}", level_idx + 1, item.content)));
        }
    }
    out
}

/// Greedy nearest-centroid clustering over chunk embeddings (an avg-link
/// approximation): each chunk joins the most similar existing cluster whose
/// running centroid is at least `threshold` cosine similarity — provided the
/// cluster still has room under `max_cluster_size` — else seeds a new one.
fn cluster_by_similarity(
    vectors: &[Vec<f32>],
    threshold: f32,
    max_cluster_size: usize,
) -> Vec<Vec<usize>> {
    struct Cluster {
        members: Vec<usize>,
        /// Running (unnormalized) sum of member vectors; compare via cosine.
        centroid: Vec<f32>,
    }
    let capacity = max_cluster_size.max(1);
    let mut clusters: Vec<Cluster> = Vec::new();
    for (index, vector) in vectors.iter().enumerate() {
        let mut best: Option<(usize, f32)> = None;
        for (ci, cluster) in clusters.iter().enumerate() {
            if cluster.members.len() >= capacity {
                continue;
            }
            let sim = cosine(vector, &cluster.centroid);
            if sim >= threshold && best.map_or(true, |(_, top)| sim > top) {
                best = Some((ci, sim));
            }
        }
        match best {
            Some((ci, _)) => {
                let cluster = &mut clusters[ci];
                cluster.members.push(index);
                for (sum, v) in cluster.centroid.iter_mut().zip(vector) {
                    *sum += *v;
                }
            }
            None => clusters.push(Cluster {
                members: vec![index],
                centroid: vector.clone(),
            }),
        }
    }
    clusters.into_iter().map(|c| c.members).collect()
}

/// Positional slicing fallback, preserving the historical grouping shape:
/// one cluster when everything fits, otherwise fixed-width slices of width
/// `len / max_cluster_size`.
fn positional_clusters(len: usize, max_cluster_size: usize) -> Vec<Vec<usize>> {
    if len <= max_cluster_size.max(1) {
        return vec![(0..len).collect()];
    }
    let width = (len / max_cluster_size.max(1)).max(1);
    (0..len)
        .collect::<Vec<_>>()
        .chunks(width)
        .map(<[usize]>::to_vec)
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}
