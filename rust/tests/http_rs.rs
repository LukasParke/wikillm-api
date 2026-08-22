//! Integration tests for the wave-3 HTTP surface helpers. Full handler
//! round-trips need the complete AppState (wired in main.rs), so the
//! router-level smoke runs in the verification gate; here we exercise the
//! public diff engine that backs GET /v1/pages/{*path}/diff.

use wikillm_api::http::diffutil::{unified_diff, DiffLine};

fn body(diff: &str) -> Vec<&str> {
    diff.lines()
        .filter(|l| !l.starts_with("---") && !l.starts_with("+++"))
        .collect()
}

#[test]
fn diff_of_identical_pages_is_empty() {
    assert_eq!(unified_diff("a", "b", "# Title\n\nbody\n", "# Title\n\nbody\n", 3), "");
}

#[test]
fn diff_headers_carry_page_labels() {
    let d = unified_diff(
        "wiki/notes.md@1",
        "wiki/notes.md@current",
        "old line\n",
        "new line\n",
        3,
    );
    assert!(d.starts_with("--- wiki/notes.md@1\n+++ wiki/notes.md@current\n"));
}

#[test]
fn diff_hunk_marks_adds_dels_and_context() {
    let d = unified_diff("a", "b", "one\ntwo\nthree\n", "one\nTWO\nthree\nfour\n", 1);
    let lines = body(&d);
    assert_eq!(lines[0], "@@ -1,3 +1,4 @@");
    assert_eq!(
        lines[1..],
        [" one", "-two", "+TWO", " three", "+four"]
    );
}

#[test]
fn diff_of_page_delete_reports_full_removal() {
    // Deleting a page records an empty body; the diff against `current`
    // must therefore be a pure deletion hunk with a zero-length new side.
    let d = unified_diff("wiki/notes.md@2", "wiki/notes.md@current", "gone\n", "", 3);
    let lines = body(&d);
    assert_eq!(lines, ["@@ -1,1 +0,0 @@", "-gone"]);
}

#[test]
fn diff_line_markers_are_unified_style() {
    assert_eq!(DiffLine::marker(&DiffLine::Context("x".into())), " ");
    assert_eq!(DiffLine::marker(&DiffLine::Del("x".into())), "-");
    assert_eq!(DiffLine::marker(&DiffLine::Add("x".into())), "+");
}
