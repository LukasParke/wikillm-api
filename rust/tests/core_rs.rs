//! Integration tests for the ported filesystem safety layer, OKF module,
//! and chunkers. Mirrors tests/unit/{paths,atomic,lock,okf,chunkers}.test.ts
//! plus a real-filesystem watcher check.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::timeout;
use wikillm_api::domain::VerifiedEntry;
use wikillm_api::error::Error;
use wikillm_api::fs::atomic::{
    atomic_write, cleanup_temp_files, hash_content, read_file_atomic, remove_if_exists,
};
use wikillm_api::fs::lock::PathLock;
use wikillm_api::fs::paths::{is_ignored_path, normalize_rel_path, resolve_wiki_path};
use wikillm_api::fs::watcher::Watcher;
use wikillm_api::ingest::chunkers::{chunk_code, chunk_markdown, detect_language, ChunkOptions};
use wikillm_api::okf::parse::{
    extract_links, extract_wikilinks, parse_markdown_document, resolve_link_target,
};
use wikillm_api::okf::trust::{actor_from_source, derive_trust_tier, is_stale, normalize_verified};
use wikillm_api::okf::validate::{validate_bundle, validate_concept_file, IssueLevel};
use wikillm_api::okf::ParsedDocument;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wikillm-core-rs-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn repeat_line(text: &str, times: usize) -> String {
    vec![text; times].join("\n")
}

fn entry(by: &str) -> VerifiedEntry {
    VerifiedEntry {
        by: by.to_string(),
        at: String::new(),
    }
}

const HAPPY_CONCEPT: &str = "---\ntype: entity\ntitle: OpenAI\nverified:\n  - by: \"human:luke\"\n    at: \"2026-01-15T10:00:00Z\"\nstale_after: \"2027-01-01T00:00:00Z\"\n---\n\n# OpenAI\n\nSee [[GPT-4]] and the [spec](../spec/okf.md#overview).\nAlso an image: ![logo](assets/logo.png) and [site](https://openai.com).\n";

// ---------------------------------------------------------------------------
// Path guards
// ---------------------------------------------------------------------------

#[test]
fn resolves_valid_wiki_paths() {
    let root = temp_root("resolve-ok");
    fs::create_dir_all(root.join("wiki/entities")).unwrap();
    fs::write(root.join("wiki/entities/OpenAI.md"), "# OpenAI").unwrap();

    let resolved = resolve_wiki_path(
        root.to_str().unwrap(),
        "wiki/entities/OpenAI.md",
    )
    .unwrap();
    assert_eq!(
        resolved,
        fs::canonicalize(root.join("wiki/entities/OpenAI.md"))
            .unwrap()
            .to_string_lossy()
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn rejects_traversal() {
    let root = temp_root("traversal");
    // "wiki/../secret.txt" lexically collapses to a bare top-level name, so
    // it is caught by the namespace guard (matching TS path.normalize order);
    // the rest are direct traversal rejections.
    for bad in ["../secret.txt", "..\\secret.txt"] {
        let err = resolve_wiki_path(root.to_str().unwrap(), bad).unwrap_err();
        assert!(matches!(&err, Error::Path(msg) if msg.starts_with("TRAVERSAL")), "{bad} -> {err:?}");
    }
    for bad in ["wiki/../secret.txt", "/etc/passwd"] {
        let err = resolve_wiki_path(root.to_str().unwrap(), bad);
        assert!(matches!(err, Err(Error::Path(_))), "{bad} must be rejected");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn rejects_reserved_segments() {
    let root = temp_root("reserved");
    for bad in [".obsidian/app.json", "wiki/.git/config", "a/node_modules/x.js", ".trash/old.md"] {
        let err = resolve_wiki_path(root.to_str().unwrap(), bad).unwrap_err();
        assert!(matches!(&err, Error::Path(m) if m.starts_with("RESERVED")), "{bad} -> {err:?}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn enforces_top_level_allowlist_and_missing() {
    let root = temp_root("toplevel");
    let root_str = root.to_str().unwrap();

    // Missing / empty paths.
    let err = resolve_wiki_path(root_str, "").unwrap_err();
    assert!(matches!(&err, Error::Path(m) if m.starts_with("MISSING_PATH")));
    let err = resolve_wiki_path(root_str, ".").unwrap_err();
    assert!(matches!(&err, Error::Path(m) if m.starts_with("EMPTY_PATH")));

    // Allowlisted top-level markdowns (case-insensitive).
    for ok in ["index.md", "LOG.MD", "agents.md", "CLAUDE.md"] {
        resolve_wiki_path(root_str, ok)
            .unwrap_or_else(|e| panic!("{ok} should resolve: {e:?}"));
    }
    // Anything else at top level is rejected.
    let err = resolve_wiki_path(root_str, "notes.md").unwrap_err();
    assert!(matches!(&err, Error::Path(m) if m.starts_with("INVALID_NAMESPACE")));

    // wiki/ and raw/ namespaces are fine.
    resolve_wiki_path(root_str, "wiki/deep/page.md").unwrap();
    resolve_wiki_path(root_str, "raw/blob.bin").unwrap();
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn rejects_realpath_escape_via_symlink() {
    use std::os::unix::fs::symlink;

    let root = temp_root("escape");
    let outside = temp_root("outside");
    fs::write(outside.join("secret.txt"), "leak").unwrap();
    fs::create_dir_all(root.join("wiki")).unwrap();
    symlink(&outside, root.join("wiki/link")).unwrap();

    let err = resolve_wiki_path(root.to_str().unwrap(), "wiki/link/secret.txt").unwrap_err();
    assert!(
        matches!(&err, Error::Path(m) if m.starts_with("OUTSIDE_ROOT")),
        "{err:?}"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
}

#[test]
fn ignored_paths_and_normalization() {
    assert!(is_ignored_path("wiki/page.md.tmp"));
    assert!(is_ignored_path(".obsidian/workspace.json"));
    assert!(is_ignored_path("deep/.DS_Store"));
    assert!(is_ignored_path("wiki/download.crdownload"));
    assert!(!is_ignored_path("wiki/page.md"));

    assert_eq!(normalize_rel_path("wiki/./a//b.md"), "wiki/a/b.md");
    assert_eq!(normalize_rel_path("wiki/x/../y.md"), "wiki/y.md");
    assert_eq!(normalize_rel_path("a\\b.md"), "a/b.md");
}

// ---------------------------------------------------------------------------
// Atomic writes
// ---------------------------------------------------------------------------

#[test]
fn atomic_write_roundtrip_and_hash_stability() {
    let root = temp_root("atomic");
    let file = root.join("doc.md");

    atomic_write(&file, "hello world").unwrap();
    atomic_write(&file, "hello world").unwrap();

    let read = read_file_atomic(&file).unwrap();
    assert_eq!(read.content, "hello world");
    assert_eq!(read.hash, hash_content("hello world"));
    assert_eq!(read.hash, read.hash); // deterministic

    // Same content -> same hash; different content -> different.
    assert_ne!(hash_content("a"), hash_content("b"));

    // No leftover temp files after successful writes.
    let leftovers: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());

    remove_if_exists(&file).unwrap();
    remove_if_exists(&file).unwrap(); // missing is fine
    assert!(!file.exists());

    // Cleanup sweeps planted leftovers only.
    fs::write(root.join(".crash.tmp"), "junk").unwrap();
    fs::write(root.join("keep.md"), "keep").unwrap();
    cleanup_temp_files(&root);
    assert!(!root.join(".crash.tmp").exists());
    assert!(root.join("keep.md").exists());
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Path locks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lock_serializes_concurrent_access() {
    let lock = Arc::new(PathLock::new());
    let counter = Arc::new(Mutex::new(0u64));
    let running = Arc::new(AtomicUsize::new(0));
    let max_running = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let lock = Arc::clone(&lock);
        let counter = Arc::clone(&counter);
        let running = Arc::clone(&running);
        let max_running = Arc::clone(&max_running);
        handles.push(tokio::spawn(async move {
            lock.run_exclusive("shared", async {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_running.fetch_max(now, Ordering::SeqCst);
                *counter.lock().unwrap() += 1;
                tokio::time::sleep(Duration::from_millis(1)).await;
                running.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(*counter.lock().unwrap(), 50);
    assert_eq!(max_running.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn multi_acquire_is_deadlock_free_in_opposite_orders() {
    let lock = Arc::new(PathLock::new());
    let first = Arc::clone(&lock);
    let second = Arc::clone(&lock);

    let t1 = tokio::spawn(async move {
        first.run_many_exclusive(&["b", "a"], async { 1 }).await
    });
    let t2 = tokio::spawn(async move {
        second.run_many_exclusive(&["a", "b"], async { 2 }).await
    });

    let (r1, r2) = timeout(Duration::from_secs(5), async { (t1.await.unwrap(), t2.await.unwrap()) })
        .await
        .expect("multi-acquire must not deadlock");
    assert_eq!((r1, r2), (1, 2));
}

#[tokio::test]
async fn different_paths_run_in_parallel() {
    let lock = Arc::new(PathLock::new());
    let running = Arc::new(AtomicUsize::new(0));
    let max_running = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for path in ["x", "y", "z"] {
        let lock = Arc::clone(&lock);
        let running = Arc::clone(&running);
        let max_running = Arc::clone(&max_running);
        handles.push(tokio::spawn(async move {
            lock.run_exclusive(path, async {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_running.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                running.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    assert!(max_running.load(Ordering::SeqCst) >= 2);
}

// ---------------------------------------------------------------------------
// Watcher
// ---------------------------------------------------------------------------

#[tokio::test]
async fn watcher_emits_debounced_batch_for_created_files() {
    let root = temp_root("watcher");
    fs::create_dir_all(root.join("wiki")).unwrap();

    let (watcher, mut rx) =
        Watcher::start(&root).expect("watcher starts");
    // Give inotify a beat to establish the recursive watch.
    tokio::time::sleep(Duration::from_millis(150)).await;

    fs::write(root.join("wiki/hello.md"), "# Hello").unwrap();
    fs::write(root.join("wiki/world.md"), "# World").unwrap();
    // Ignored files must never surface.
    fs::write(root.join("wiki/.DS_Store"), "").unwrap();
    fs::write(root.join("wiki/skip.tmp"), "").unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_hello = false;
    let mut saw_world = false;
    while std::time::Instant::now() < deadline && !(saw_hello && saw_world) {
        match timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(batch)) => {
                // Ignored files must never surface, in any batch.
                assert!(
                    !batch.iter().any(|p| p.ends_with(".tmp") || p.ends_with(".DS_Store")),
                    "ignored files leaked through: {batch:?}"
                );
                saw_hello |= batch.iter().any(|p| p == "wiki/hello.md");
                saw_world |= batch.iter().any(|p| p == "wiki/world.md");
            }
            Ok(None) => break,
            Err(_) => {} // keep polling until the deadline
        }
    }
    assert!(saw_hello && saw_world, "never saw both created files");

    watcher.stop();
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Link extraction + resolution
// ---------------------------------------------------------------------------

#[test]
fn extract_links_skips_external_and_anchors() {
    let body = [
        "[text](target.md)",
        "![alt](img.png)",
        "[ext](https://example.com/x)",
        "[mail](mailto:a@b.c)",
        "[anchor](#section)",
    ]
    .join("\n");
    assert_eq!(extract_links(&body), ["target.md", "img.png"]);
}

#[test]
fn extract_links_strips_anchors_and_dedupes() {
    let body = "[a](docs/guide.md#intro)\n[b](docs/guide.md#advanced)\n[c](docs/guide.md)";
    assert_eq!(extract_links(body), ["docs/guide.md"]);
}

#[test]
fn extract_links_handles_titles_and_angle_targets() {
    let body = "[t](file%20name.md \"Title\")\n[u](<angled path.md>)";
    assert_eq!(extract_links(body), ["file%20name.md", "angled path.md"]);
    assert!(extract_links("just plain text\nwith no links").is_empty());
}

#[test]
fn extract_wikilinks_plain_alias_dedupe() {
    let body = "See [[Foo]] then [[Bar|the bar page]] then [[Foo]] again.";
    assert_eq!(extract_wikilinks(body), ["Foo", "Bar"]);
    assert!(extract_wikilinks("[not](a wikilink.md) [[ ]] [[]]").is_empty());
}

#[test]
fn resolve_link_target_matrix() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("Other.md", "concepts/a.md", Some("concepts/Other.md")),
        ("Other", "concepts/a.md", Some("concepts/Other.md")),
        ("/entities/OpenAI.md", "notes/deep/page.md", Some("entities/OpenAI.md")),
        ("./sibling.md", "a/b/c.md", Some("a/b/sibling.md")),
        ("../parent.md", "a/b/c.md", Some("a/parent.md")),
        ("../../escape.md", "a/b/c.md", Some("escape.md")),
        ("../../../escape.md", "a/b/c.md", None),
        ("https://example.com/x", "a.md", None),
        ("mailto:x@y.z", "a.md", None),
        ("#section-only", "a.md", None),
        ("page.md#section", "concepts/a.md", Some("concepts/page.md")),
        ("image.png", "concepts/a.md", Some("concepts/image.png")),
    ];
    for (link, source, expected) in cases {
        assert_eq!(
            resolve_link_target(link, source).as_deref(),
            *expected,
            "link {link:?} from {source:?}"
        );
    }
}

#[test]
fn parses_happy_document_and_rejects_bad_yaml() {
    let doc: ParsedDocument = parse_markdown_document(HAPPY_CONCEPT).unwrap();
    assert_eq!(doc.frontmatter["type"], "entity");
    assert!(doc.body.contains("# OpenAI"));
    assert_eq!(extract_wikilinks(&doc.body), ["GPT-4"]);
    assert_eq!(extract_links(&doc.body), ["../spec/okf.md", "assets/logo.png"]);

    let bad = "---\ntype: \"unterminated\n---\nbody";
    match parse_markdown_document(bad) {
        Err(Error::Validation(msg)) => assert!(msg.contains("frontmatter"), "{msg}"),
        other => panic!("expected validation error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Trust tiers
// ---------------------------------------------------------------------------

#[test]
fn trust_tiers_cover_absent_machine_and_human() {
    assert_eq!(derive_trust_tier(None), "unverified");
    assert_eq!(derive_trust_tier(Some(&vec![])), "unverified");
    assert_eq!(
        derive_trust_tier(Some(&vec![entry("crawler/wikillm-api")])),
        "machine-confirmed"
    );
    assert_eq!(derive_trust_tier(Some(&vec![entry("human:luke")])), "human-reviewed");
    assert_eq!(
        derive_trust_tier(Some(&vec![entry("bot"), entry("human:luke")])),
        "human-reviewed"
    );
}

#[test]
fn bare_verified_mapping_is_one_element_list() {
    use serde_json::json;

    let human = json!({ "by": "human:luke", "at": "2026-01-01" });
    let normalized = normalize_verified(&human).unwrap();
    assert_eq!(derive_trust_tier(Some(&normalized)), "human-reviewed");

    let bare_bot = json!({ "by": "bot" });
    let got = normalize_verified(&bare_bot).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].by, "bot");
    assert_eq!(got[0].at, "");

    use serde_json::Value;
    assert!(normalize_verified(&Value::Null).is_none());
    assert!(normalize_verified(&json!([])).is_none());
    // Entries missing `by` are dropped; non-objects ignored.
    assert!(normalize_verified(&json!([{ "at": "2026-01-01" }, "junk", 42])).is_none());
    let filtered = normalize_verified(&json!([{ "at": "x" }, { "by": "bot" }])).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].by, "bot");
    assert_eq!(filtered[0].at, "");
}

#[test]
fn staleness_matrix() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    assert!(is_stale(Some("2026-01-01T00:00:00Z"), now));
    assert!(!is_stale(Some("2027-01-01T00:00:00Z"), now));
    assert!(!is_stale(None, now));
    assert!(!is_stale(Some(""), now));
    assert!(!is_stale(Some("not-a-date"), now));
}

#[test]
fn actor_naming_convention() {
    assert_eq!(actor_from_source("Luke", &["luke".to_string()]), "human:Luke");
    assert_eq!(actor_from_source("user-alice", &[]), "human:user-alice");
    assert_eq!(actor_from_source("Human-Bob", &[]), "human:Human-Bob");
    assert_eq!(actor_from_source("web-crawler", &[]), "web-crawler/wikillm-api");
}

#[test]
fn errors_on_unparseable_yaml() {
    let issues = validate_concept_file("a.md", "---\ntype: \"unterminated\n---\nbody");
    assert!(
        issues.iter().any(|i| i.level == IssueLevel::Error && i.message.contains("frontmatter")),
        "{issues:?}"
    );
}

#[test]
fn exempts_agents_claude_and_reserved_any_depth() {
    assert!(validate_concept_file("AGENTS.md", "# agents").is_empty());
    assert!(validate_concept_file("claude.md", "# claude").is_empty());
    assert!(validate_concept_file("index.md", "# Home\n").is_empty());
    assert!(validate_concept_file("notes/index.md", "# Notes\n").is_empty());
    assert!(validate_concept_file("log.md", "# Log\n").is_empty());
    assert!(validate_concept_file("deep/dir/log.md", "").is_empty());
}

#[test]
fn warns_on_malformed_log_date_headings() {
    let raw = "# Log\n\n## 2026-01-01\n\n## January 2nd\n\n## 2026-1-3\n";
    let issues = validate_concept_file("log.md", raw);
    assert_eq!(issues.len(), 2);
    assert!(issues.iter().all(|i| i.level == IssueLevel::Warning));
    assert!(issues.iter().all(|i| i.path.contains("#L")));
}

#[test]
fn warns_on_non_string_okf_version_at_root_only() {
    let raw = "---\nokf_version: 0.2\n---\n# Home";
    let root_issues = validate_concept_file("index.md", raw);
    assert_eq!(root_issues.len(), 1);
    assert_eq!(root_issues[0].level, IssueLevel::Warning);
    assert!(validate_concept_file("sub/index.md", raw).is_empty());
}

#[test]
fn bundle_aggregates_errors_warnings_and_stats() {
    let report = validate_bundle([
        ("index.md", "---\nokf_version: \"0.2\"\n---\n# Home"),
        ("log.md", "# Log\n\n## bad heading\n"),
        ("entities/openai.md", HAPPY_CONCEPT),
        (
            "entities/anthropic.md",
            "---\ntype: entity\nverified:\n  - by: human:luke\nstale_after: \"2020-01-01T00:00:00Z\"\n---\nbody",
        ),
        ("notes/snippet.md", "---\ntype: note\n---\nbody"),
        ("broken.md", "no frontmatter"),
    ]);

    assert!(!report.valid);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].path, "broken.md");
    assert_eq!(report.warnings.len(), 1);

    assert_eq!(report.stats.concepts, 3);
    let by_type: std::collections::BTreeMap<String, usize> =
        [("entity".to_string(), 2), ("note".to_string(), 1)].into_iter().collect();
    assert_eq!(report.stats.by_type, by_type);
    let tiers: std::collections::BTreeMap<String, usize> = [
        ("human-reviewed".to_string(), 2),
        ("unverified".to_string(), 1),
    ]
    .into_iter()
    .collect();
    assert_eq!(report.stats.trust_tiers, tiers);
    assert_eq!(report.stats.stale_count, 1);

    let clean = validate_bundle([("a.md", "---\ntype: t\n---\nx")]);
    assert!(clean.valid);
    assert!(clean.errors.is_empty());
}

// ---------------------------------------------------------------------------
// Chunkers — markdown
// ---------------------------------------------------------------------------

#[test]
fn chunk_markdown_no_headings_single_null_chunk() {
    let chunks = chunk_markdown("Just some prose.\n\nSecond paragraph.", ChunkOptions::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].ordinal, 0);
    assert_eq!(chunks[0].heading_path, None);
}

#[test]
fn chunk_markdown_empty_body_is_empty() {
    assert!(chunk_markdown("", ChunkOptions::default()).is_empty());
    assert!(chunk_markdown("   \n\n  \n", ChunkOptions::default()).is_empty());
}

#[test]
fn chunk_markdown_builds_full_ancestor_chain() {
    let body = [
        "preamble before any heading",
        "# Install",
        "intro text",
        "## Setup",
        "setup text",
        "### Advanced",
        "advanced text",
        "## Config",
        "config text",
    ]
    .join("\n");
    let chunks = chunk_markdown(&body, ChunkOptions::default());
    let paths: Vec<_> = chunks.iter().map(|c| c.heading_path.clone()).collect();
    assert_eq!(
        paths,
        [
            None,
            Some("Install".into()),
            Some("Install > Setup".into()),
            Some("Install > Setup > Advanced".into()),
            Some("Install > Config".into()),
        ]
    );
    let ordinals: Vec<i64> = chunks.iter().map(|c| c.ordinal).collect();
    assert_eq!(ordinals, [0, 1, 2, 3, 4]);
}

#[test]
fn section_runs_until_next_heading_of_any_level() {
    let body = ["# A", "line one", "line two", "## B", "line three"].join("\n");
    let chunks = chunk_markdown(&body, ChunkOptions::default());
    let a = chunks.iter().find(|c| c.heading_path.as_deref() == Some("A")).unwrap();
    assert_eq!(a.content, "line one\nline two");
    let b = chunks
        .iter()
        .find(|c| c.heading_path.as_deref() == Some("A > B"))
        .unwrap();
    assert_eq!(b.content, "line three");
}

#[test]
fn merges_small_chunks_with_same_heading_path() {
    let body = [
        "# Topic",
        &repeat_line("filler sentence for the big section body.", 40),
        "## Sub A",
        "tiny a",
        "tiny b",
        "## Sub B",
        "sub b body that is long enough to stand on its own without merging.",
    ]
    .join("\n");
    let chunks = chunk_markdown(&body, ChunkOptions::default());
    let sub_a: Vec<_> = chunks
        .iter()
        .filter(|c| c.heading_path.as_deref() == Some("Topic > Sub A"))
        .collect();
    assert_eq!(sub_a.len(), 1);
    assert!(sub_a[0].content.contains("tiny a"));
    assert!(sub_a[0].content.contains("tiny b"));
}

#[test]
fn keeps_small_chunk_when_merge_would_exceed_cap() {
    let max_chars = 300;
    let big = "x".repeat(280);
    let small = "y".repeat(250);
    let body = format!("# A\n{big}\n## B\n{small}");
    let chunks = chunk_markdown(
        &body,
        ChunkOptions {
            max_chars,
            min_chars: 200,
        },
    );
    let a = chunks.iter().find(|c| c.heading_path.as_deref() == Some("A")).unwrap();
    let b = chunks
        .iter()
        .find(|c| c.heading_path.as_deref() == Some("A > B"))
        .unwrap();
    assert_eq!(a.content.chars().count(), 280);
    assert_eq!(b.content.chars().count(), 250);
}

#[test]
fn splits_oversized_sections_respecting_max_chars() {
    let para = repeat_line("word word word", 10);
    let body = format!("# Big\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}", para, para, para, para, para, para);
    let max_chars = 300;
    let chunks = chunk_markdown(
        &body,
        ChunkOptions {
            max_chars,
            min_chars: 200,
        },
    );
    let big_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.heading_path.as_deref() == Some("Big"))
        .collect();
    assert!(big_chunks.len() > 1);
    for chunk in &big_chunks {
        assert!(chunk.content.chars().count() <= max_chars);
    }
    let ordinals: Vec<i64> = big_chunks.iter().map(|c| c.ordinal).collect();
    let mut sorted = ordinals.clone();
    sorted.sort_unstable();
    assert_eq!(ordinals, sorted);
    assert!(ordinals.windows(2).all(|w| w[1] == w[0] + 1), "{ordinals:?}");
}

#[test]
fn hard_splits_pathological_content() {
    let blob = "z".repeat(1000);
    let body = format!("# Blob\n{blob}");
    let chunks = chunk_markdown(
        &body,
        ChunkOptions {
            max_chars: 300,
            min_chars: 200,
        },
    );
    let blob_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.heading_path.as_deref() == Some("Blob"))
        .collect();
    assert_eq!(blob_chunks.len(), 4);
    for chunk in &blob_chunks {
        assert!(chunk.content.chars().count() <= 300);
        assert!(chunk.content.chars().all(|c| c == 'z'));
    }
    let ordinals: Vec<i64> = blob_chunks.iter().map(|c| c.ordinal).collect();
    assert_eq!(ordinals, [0, 1, 2, 3]);
}

// ---------------------------------------------------------------------------
// Chunkers — code
// ---------------------------------------------------------------------------

#[test]
fn detect_language_maps_extensions() {
    assert_eq!(detect_language("main.ts"), Some("typescript"));
    assert_eq!(detect_language("app.tsx"), Some("tsx"));
    assert_eq!(detect_language("server.js"), Some("javascript"));
    assert_eq!(detect_language("cli.py"), Some("python"));
    assert_eq!(detect_language("lib.rs"), Some("rust"));
    assert_eq!(detect_language("main.go"), Some("go"));
    assert_eq!(detect_language("util.hpp"), Some("cpp"));
    assert_eq!(detect_language("a/b/c/config.yml"), Some("yaml"));
    assert_eq!(detect_language("style.css"), Some("css"));
    assert_eq!(detect_language("README.MD"), Some("markdown"));
    assert_eq!(detect_language("artifact.bin"), None);
    assert_eq!(detect_language("Makefile"), None);
    assert_eq!(detect_language(""), None);
}

#[test]
fn code_symbol_paths_for_class_methods_in_brace_languages() {
    let source = [
        "export class CheckpointLoader {",
        "  private loadManifest() {",
        "    return 1;",
        "  }",
        "",
        "  save(path: string): void {",
        "    void path;",
        "  }",
        "}",
    ]
    .join("\n");
    let chunks = chunk_code(&source, Some("typescript"), ChunkOptions::default());
    let paths: Vec<_> = chunks.iter().map(|c| c.heading_path.clone()).collect();
    assert!(paths.contains(&Some("CheckpointLoader".into())), "{paths:?}");
    assert!(paths.contains(&Some("CheckpointLoader > loadManifest()".into())));
    assert!(paths.contains(&Some("CheckpointLoader > save()".into())));
}

#[test]
fn python_def_class_indentation_nesting() {
    let source = [
        "class Trainer:",
        "    def fit(self):",
        "        pass",
        "",
        "    def predict(self):",
        "        pass",
    ]
    .join("\n");
    let chunks = chunk_code(&source, Some("python"), ChunkOptions::default());
    let paths: Vec<_> = chunks.iter().map(|c| c.heading_path.clone()).collect();
    assert!(paths.contains(&Some("Trainer".into())), "{paths:?}");
    assert!(paths.contains(&Some("Trainer > fit()".into())));
    assert!(paths.contains(&Some("Trainer > predict()".into())));
}

#[test]
fn top_level_functions_and_preamble_separate() {
    let source = ["import os", "", "def main():", "    pass", "", "def helper():", "    pass"].join("\n");
    let chunks = chunk_code(&source, Some("python"), ChunkOptions::default());
    let preamble = chunks.iter().find(|c| c.heading_path.is_none()).unwrap();
    assert!(preamble.content.contains("import os"));
    let paths: Vec<_> = chunks.iter().map(|c| c.heading_path.clone()).collect();
    assert_eq!(paths, [None, Some("main()".into()), Some("helper()".into())]);
}

#[test]
fn falls_back_to_blank_line_groups_inside_oversized_decl() {
    let group = repeat_line("statement();", 8);
    let method_body = [group.as_str(), "", group.as_str(), "", group.as_str(), "", group.as_str(), "", group.as_str()].join("\n");
    let source = ["class Big {", "  run() {", &method_body, "  }", "}"].join("\n");
    let max_chars = 250;
    let chunks = chunk_code(
        &source,
        Some("typescript"),
        ChunkOptions {
            max_chars,
            min_chars: 200,
        },
    );
    let run_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.heading_path.as_deref() == Some("Big > run()"))
        .collect();
    assert!(run_chunks.len() > 1);
    for chunk in &run_chunks {
        assert!(chunk.content.chars().count() <= max_chars);
    }
}

#[test]
fn hard_splits_pathological_code() {
    let source = format!("function blob() {{ {} }}", "x".repeat(900));
    let chunks = chunk_code(
        &source,
        Some("javascript"),
        ChunkOptions {
            max_chars: 300,
            min_chars: 200,
        },
    );
    assert!(chunks.len() >= 3);
    for chunk in &chunks {
        assert!(chunk.content.chars().count() <= 300);
        assert_eq!(chunk.heading_path.as_deref(), Some("blob()"));
    }
}

#[test]
fn code_ordinals_sequential_and_deterministic() {
    let source = [
        "class A {",
        "  one() { return 1; }",
        "  two() { return 2; }",
        "}",
        "class B {",
        "  three() { return 3; }",
        "}",
    ]
    .join("\n");
    let first = chunk_code(&source, Some("typescript"), ChunkOptions::default());
    let second = chunk_code(&source, Some("typescript"), ChunkOptions::default());
    assert_eq!(first, second);
    for (index, chunk) in first.iter().enumerate() {
        assert_eq!(chunk.ordinal, index as i64);
    }
}

#[test]
fn oversized_top_level_declaration_shares_heading_path() {
    let stmt = repeat_line("doSomething(withArgs);", 6);
    let fn_body = [stmt.clone(), stmt.clone(), stmt.clone(), stmt].join("\n\n");
    let source = ["fn compute() {", &fn_body, "}"].join("\n");
    let max_chars = 300;
    let chunks = chunk_code(
        &source,
        Some("rust"),
        ChunkOptions {
            max_chars,
            min_chars: 200,
        },
    );
    let compute_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.heading_path.as_deref() == Some("compute()"))
        .collect();
    assert!(compute_chunks.len() > 1);
    compute_chunks.windows(2).for_each(|w| {
        assert_eq!(w[1].ordinal, w[0].ordinal + 1);
    });
    for chunk in &compute_chunks {
        assert!(chunk.content.chars().count() <= max_chars);
    }
}
