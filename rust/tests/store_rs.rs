use wikillm_api::domain::*;
use wikillm_api::store::sqlite::SqliteStore;
use wikillm_api::store::Store;
use std::sync::Arc;

async fn make_store() -> Arc<dyn Store> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
    store.migrate().await.unwrap();
    std::mem::forget(dir);
    Arc::new(store)
}

fn sample_doc(rel_path: &str) -> DocumentInput {
    DocumentInput {
        rel_path: rel_path.to_string(),
        kind: DocKind::Page,
        origin: "wiki".into(),
        title: Some("OpenAI".into()),
        summary: Some("An AI company.".into()),
        body: "# OpenAI\n\nOpenAI builds large language models such as GPT for language understanding.".into(),
        frontmatter: serde_json::json!({"type": "Company", "tags": ["ai"]}),
        word_count: 10,
        outgoing_links: vec!["/wiki/concepts/gpt.md".into()],
        hash: "a".repeat(64),
        mtime: 1_700_000_000_000,
        content_type: Some("text/markdown".into()),
        okf_type: Some("Company".into()),
        tags: vec!["ai".into()],
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

#[tokio::test]
async fn upsert_and_get_document() {
    let store = make_store().await;
    store.upsert_document(&sample_doc("wiki/entities/openai.md")).await.unwrap();
    let doc = store.get_document("wiki/entities/openai.md").await.unwrap().unwrap();
    assert_eq!(doc.title.as_deref(), Some("OpenAI"));
    assert_eq!(doc.okf_type.as_deref(), Some("Company"));
    assert_eq!(doc.tags, vec!["ai"]);
}

#[tokio::test]
async fn stable_id_across_upserts() {
    let store = make_store().await;
    store.upsert_document(&sample_doc("wiki/x.md")).await.unwrap();
    let first = store.get_document("wiki/x.md").await.unwrap().unwrap().id;
    store.upsert_document(&sample_doc("wiki/x.md")).await.unwrap();
    let second = store.get_document("wiki/x.md").await.unwrap().unwrap().id;
    assert_eq!(first, second);
}

#[tokio::test]
async fn pagination() {
    let store = make_store().await;
    for p in ["wiki/a.md", "wiki/b.md", "wiki/c.md"] {
        store.upsert_document(&sample_doc(p)).await.unwrap();
    }
    let page1 = store.list_documents(&ListOptions { folder: Some("wiki".into()), ..Default::default() }, 2, None).await.unwrap();
    assert_eq!(page1.items.len(), 2);
    assert!(page1.next_cursor.is_some());
    let page2 = store.list_documents(&ListOptions { folder: Some("wiki".into()), ..Default::default() }, 2, page1.next_cursor.as_deref()).await.unwrap();
    assert!(!page2.items.is_empty());
}

#[tokio::test]
async fn fts_search_with_filters() {
    let store = make_store().await;
    store.upsert_document(&sample_doc("wiki/entities/openai.md")).await.unwrap();
    let doc = store.get_document("wiki/entities/openai.md").await.unwrap().unwrap();
    store.replace_chunks(&doc.id, &[
        ChunkInput { ordinal: 0, heading_path: Some("OpenAI".into()), content: "OpenAI builds GPT language models.".into(), distilled: None },
    ]).await.unwrap();

    let hits = store.search_fts("GPT language", 5, Some(&SearchFilters { kinds: Some(vec!["page".into()]), ..Default::default() })).await.unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].rel_path, "wiki/entities/openai.md");

    let scoped = store.search_fts("GPT", 5, Some(&SearchFilters { path_prefixes: Some(vec!["raw".into()]), ..Default::default() })).await.unwrap();
    assert!(scoped.is_empty());
}

#[tokio::test]
async fn edges_and_backlinks() {
    let store = make_store().await;
    store.replace_edges("wiki/gpt.md", &["wiki/openai.md".into()]).await.unwrap();
    store.replace_edges("wiki/overview.md", &["wiki/openai.md".into()]).await.unwrap();
    let mut links = store.backlinks("wiki/openai.md", 100).await.unwrap();
    links.sort();
    assert_eq!(links, vec!["wiki/gpt.md", "wiki/overview.md"]);
    store.replace_edges("wiki/gpt.md", &[]).await.unwrap();
    assert_eq!(store.backlinks("wiki/openai.md", 100).await.unwrap(), vec!["wiki/overview.md"]);
}

#[tokio::test]
async fn connectors_and_state() {
    let store = make_store().await;
    let now = chrono::Utc::now().to_rfc3339();
    store.put_connector(&ConnectorConfig {
        id: "git-docs".into(), kind: "git".into(),
        config: serde_json::json!({"url": "https://example.com/repo.git"}),
        enabled: true, created_at: now.clone(), updated_at: now,
    }).await.unwrap();
    assert!(store.get_connector("git-docs").await.unwrap().is_some());
    store.set_connector_state("git-docs", &serde_json::json!({"commit": "abc"})).await.unwrap();
    assert_eq!(store.get_connector_state("git-docs").await.unwrap().unwrap(), serde_json::json!({"commit": "abc"}));
    assert!(store.delete_connector("git-docs").await.unwrap());
}

#[tokio::test]
async fn projects_crud() {
    let store = make_store().await;
    store.put_project(&ProjectInput { name: "test".into(), prefixes: vec!["wiki/".into()], connectors: vec![], description: None }).await.unwrap();
    assert!(store.get_project("test").await.unwrap().is_some());
    assert!(store.delete_project("test").await.unwrap());
}

#[tokio::test]
async fn settings_roundtrip() {
    let store = make_store().await;
    store.set_setting("public_read", &serde_json::json!(false), "test").await.unwrap();
    let settings = store.get_settings().await.unwrap();
    assert_eq!(settings.get("public_read"), Some(&serde_json::json!(false)));
    assert!(store.delete_setting("public_read").await.unwrap());
    assert!(!store.delete_setting("public_read").await.unwrap());
}

#[tokio::test]
async fn stats_and_feedback() {
    let store = make_store().await;
    let before = store.stats_overview().await.unwrap();
    store.record_query(&QueryRecord {
        id: "q1".into(), created_at: chrono::Utc::now().to_rfc3339(),
        query: "test".into(), mode: "hybrid".into(), project: None,
        latency_ms: 10.0, result_count: 3, zero_hit: false,
        top_paths: vec![], source: Some("test".into()), error: None,
    }).await.unwrap();
    store.record_feedback("q1", true, None).await.unwrap();
    let after = store.stats_overview().await.unwrap();
    assert_eq!(after.queries, before.queries + 1);
    assert_eq!(after.feedback_total, before.feedback_total + 1);
}

#[tokio::test]
async fn delete_derived_by_origin() {
    let store = make_store().await;
    store.upsert_document(&sample_doc("ext/x.md")).await.unwrap();
    store.delete_derived_for_origin("wiki").await.unwrap_or(());
    // ext/x.md should survive since origin is "wiki" not "ext"
    // actually sample_doc has origin "wiki" so it gets deleted; use a different origin
    let mut doc = sample_doc("ext/x.md");
    doc.origin = "web-x".into();
    store.upsert_document(&doc).await.unwrap();
    store.delete_derived_for_origin("web-x").await.unwrap();
    assert!(store.get_document("ext/x.md").await.unwrap().is_none());
}
