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
        // Simple clustering: split into groups of max_cluster_size
        let clusters: Vec<&[String]> = if current.len() <= max_cluster_size {
            vec![&current]
        } else {
            current.chunks(current.len() / max_cluster_size.max(1)).collect()
        };

        let mut next_level = Vec::new();
        for cluster in clusters {
            let combined = cluster.join("\n\n---\n\n");
            let truncated = combined.chars().take(4000).collect::<String>();
            let messages: Vec<(&str, &str)> = vec![
                ("system", SUMMARY_SYSTEM),
                ("user", &truncated),
            ];
            let summary = match llm.chat(&messages, 0.3, 300).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            next_level.push(format!("Summary of {} passages:\n{}", cluster.len(), summary));
        }

        levels.push(
            next_level
                .iter()
                .map(|s| ChunkSummary { level, content: s.clone(), children_count: max_cluster_size })
                .collect(),
        );
        current = next_level;
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
