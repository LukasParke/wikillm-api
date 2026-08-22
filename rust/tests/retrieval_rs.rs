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

use wikillm_api::domain::ChunkHit;
use wikillm_api::services::search::{collapse_near_dups, Scored};

fn scored(rel_path: &str, content: &str, score: f64) -> Scored {
    Scored {
        score,
        hit: ChunkHit {
            chunk_id: format!("{rel_path}#0"),
            document_id: rel_path.to_string(),
            rel_path: rel_path.to_string(),
            kind: "wiki".to_string(),
            origin: "wiki".to_string(),
            title: None,
            okf_type: None,
            tags: Vec::new(),
            status: None,
            stale_after: None,
            verified: None,
            hash: "h".to_string(),
            mtime: 0,
            heading_path: None,
            content: content.to_string(),
            score,
        },
    }
}

#[test]
fn near_dup_collapse_keeps_higher_scored_of_similar_pair() {
    let hits = vec![
        scored("a.md", "The deployment pipeline runs automated smoke tests before every rollout", 2.0),
        scored("b.md", "the deployment pipeline runs automated smoke tests before every ROLLOUT", 1.5),
        scored("c.md", "Kubernetes ingress controllers route external traffic to clustered services", 1.0),
    ];
    let out = collapse_near_dups(hits);
    assert_eq!(out.len(), 2);
    // Case/punctuation-only variant collapses into the higher-scored hit.
    assert_eq!(out[0].hit.rel_path, "a.md");
    assert_eq!(out[1].hit.rel_path, "c.md");
}

#[test]
fn near_dup_collapse_is_order_independent() {
    let hits = vec![
        scored("low.md", "The deployment pipeline runs automated smoke tests before every rollout", 1.0),
        scored("high.md", "The deployment pipeline runs automated smoke tests before every rollout!", 3.0),
    ];
    let out = collapse_near_dups(hits);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].hit.rel_path, "high.md");
}

#[test]
fn partially_similar_contents_survive_collapse() {
    let hits = vec![
        scored("x.md", "alpha beta gamma delta epsilon", 2.0),
        scored("y.md", "alpha beta gamma zeta eta theta", 1.9),
    ];
    // Overlap is one shared trigram out of six — well below the 0.8 cutoff.
    assert_eq!(collapse_near_dups(hits).len(), 2);
}

// Draft exclusion (wave-2): pages with status 'draft' (e.g. promoter
// output under derived/promotions/) stay out of search results unless
// the caller explicitly filters for that status.

use std::sync::Arc;

use wikillm_api::domain::{ChunkInput, DocKind, DocumentInput, SearchFilters};
use wikillm_api::store::sqlite::SqliteStore;
use wikillm_api::store::Store;

async fn make_draft_store() -> Arc<dyn Store> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
    store.migrate().await.unwrap();
    std::mem::forget(dir);
    Arc::new(store)
}

fn probe_doc(rel_path: &str, status: &str) -> DocumentInput {
    DocumentInput {
        rel_path: rel_path.to_string(),
        kind: DocKind::Page,
        origin: "wiki".into(),
        title: Some("Promotion probe".into()),
        summary: None,
        body: "# Promotion probe\n\npromotedraftprobe fixture page.".into(),
        frontmatter: serde_json::json!({"type": "concept"}),
        word_count: 6,
        outgoing_links: vec![],
        hash: "a".repeat(64),
        mtime: 1_700_000_000_000,
        content_type: Some("text/markdown".into()),
        okf_type: Some("Concept".into()),
        tags: vec![],
        status: Some(status.to_string()),
        stale_after: None,
        resource: None,
        generated_by: Some("wikillm-promoter".into()),
        generated_at: None,
        verified: None,
        provenance: None,
        updated_at: None,
        updated_by: None,
    }
}

async fn seed_probe_chunk(store: &Arc<dyn Store>, rel_path: &str, content: &str) {
    store.upsert_document(&probe_doc(rel_path, if rel_path.starts_with("derived/") { "draft" } else { "stable" })).await.unwrap();
    let doc = store.get_document(rel_path).await.unwrap().unwrap();
    store.replace_chunks(&doc.id, &[
        ChunkInput { ordinal: 0, heading_path: Some("Promotion probe".into()), content: content.into(), distilled: None },
    ]).await.unwrap();
}

#[tokio::test]
async fn search_excludes_draft_pages_unless_status_requested() {
    let store = make_draft_store().await;
    seed_probe_chunk(&store, "wiki/promo/stable.md", "promotedraftprobe stable content.").await;
    seed_probe_chunk(&store, "derived/promotions/deploy-pipeline.md", "promotedraftprobe draft content.").await;

    // No filters object at all: drafts hidden.
    let hits = store.search_fts("promotedraftprobe", 10, None).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "wiki/promo/stable.md");

    // Filters present but without statuses: still hidden.
    let hits = store.search_fts(
        "promotedraftprobe",
        10,
        Some(&SearchFilters { origins: Some(vec!["wiki".into()]), ..Default::default() }),
    ).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "wiki/promo/stable.md");

    // Explicit statuses filter opts back into drafts.
    let hits = store.search_fts(
        "promotedraftprobe",
        10,
        Some(&SearchFilters { statuses: Some(vec!["draft".into()]), ..Default::default() }),
    ).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel_path, "derived/promotions/deploy-pipeline.md");

    let hits = store.search_fts(
        "promotedraftprobe",
        10,
        Some(&SearchFilters { statuses: Some(vec!["draft".into(), "stable".into()]), ..Default::default() }),
    ).await.unwrap();
    assert_eq!(hits.len(), 2);
}
