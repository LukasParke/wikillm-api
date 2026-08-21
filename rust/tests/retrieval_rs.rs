//! Retrieval primitive tests: RRF fusion, recency decay, and planner parsing.

use wikillm_api::services::query::{parse_tool_plan, ToolPlanEntry};
use wikillm_api::services::search::{recency_boost, rrf_fuse};

const DAY_MS: i64 = 86_400_000;
const NOW: i64 = 1_700_000_000_000;

#[test]
fn rrf_consensus_beats_single_list_top_rank() {
    let fused = rrf_fuse(
        &[
            vec!["a".into(), "b".into(), "c".into()],
            vec!["b".into(), "c".into(), "d".into()],
        ],
        60,
    );
    // "b" appears in both lists (rank 2 + rank 1) and must outrank "a",
    // which only tops the first list.
    assert_eq!(fused[0].0, "b");
    let score_of = |key: &str| fused.iter().find(|(k, _)| k == key).unwrap().1;
    assert!(score_of("b") > score_of("a"));
    assert!(score_of("b") > score_of("d"));
}

#[test]
fn rrf_first_rank_score_is_one_over_k_plus_one() {
    let fused = rrf_fuse(&[vec!["x".into()]], 60);
    assert_eq!(fused.len(), 1);
    assert!((fused[0].1 - 1.0 / 61.0).abs() < 1e-12);
}

#[test]
fn rrf_dedupes_to_union_size() {
    let fused = rrf_fuse(&[vec!["a".into(), "b".into()], vec!["b".into(), "c".into()]], 60);
    assert_eq!(fused.len(), 3);
    let keys: std::collections::HashSet<&String> = fused.iter().map(|(k, _)| k).collect();
    assert_eq!(keys.len(), 3);
}

#[test]
fn rrf_empty_input_yields_empty_output() {
    assert!(rrf_fuse(&[], 60).is_empty());
    assert!(rrf_fuse(&[Vec::new(), Vec::new()], 60).is_empty());
}

#[test]
fn recency_boost_is_exp_minus_one_at_thirty_days() {
    let boost = recency_boost(NOW - 30 * DAY_MS, NOW);
    assert!((boost - (-1.0f64).exp()).abs() < 1e-9, "got {boost}");
}

#[test]
fn recency_boost_future_dated_clamps_to_one() {
    let boost = recency_boost(NOW + DAY_MS, NOW);
    assert!((boost - 1.0).abs() < 1e-12, "got {boost}");
}

#[test]
fn recency_boost_near_zero_at_three_hundred_sixty_five_days() {
    let boost = recency_boost(NOW - 365 * DAY_MS, NOW);
    assert!(boost < 0.001, "got {boost}");
}

fn plan(raw: &str) -> Vec<ToolPlanEntry> {
    parse_tool_plan(raw)
}

#[test]
fn planner_valid_json_filters_unknown_tools() {
    let raw = "Sure! {\"tools\":[{\"name\":\"search_pages\",\"query\":\"rust async\"},{\"name\":\"bogus\",\"query\":\"x\"}]} done";
    let tools = plan(raw);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "search_pages");
    assert_eq!(tools[0].query, "rust async");
}

#[test]
fn planner_malformed_json_yields_empty_plan() {
    assert!(plan("not json at all").is_empty());
    assert!(plan("{\"tools\":").is_empty());
}

#[test]
fn planner_non_array_tools_yields_empty_plan() {
    assert!(plan("{\"tools\":\"search_pages\"}").is_empty());
}

#[test]
fn planner_missing_query_defaults_to_empty_string() {
    let tools = plan("{\"tools\":[{\"name\":\"recent_changes\"}]}");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "recent_changes");
    assert_eq!(tools[0].query, "");
}
