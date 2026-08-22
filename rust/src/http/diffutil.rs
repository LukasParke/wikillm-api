//! Minimal LCS-based unified line diff. No external crates: the DP table is
//! O(len(a) * len(b)) over wiki-page-sized inputs, which is fine.
//!
//! Output follows the standard unified format (`---`, `+++`, `@@ -s,c +s,c @@`)
//! with configurable trailing context so clients can render it verbatim.

/// One classified line of a raw diff stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// Line present in both inputs.
    Context(String),
    /// Line removed from `a`.
    Del(String),
    /// Line added in `b`.
    Add(String),
}

impl DiffLine {
    pub fn marker(&self) -> &'static str {
        match self {
            DiffLine::Context(_) => " ",
            DiffLine::Del(_) => "-",
            DiffLine::Add(_) => "+",
        }
    }

    pub fn text(&self) -> &str {
        match self {
            DiffLine::Context(s) | DiffLine::Del(s) | DiffLine::Add(s) => s,
        }
    }
}
/// Split into lines WITHOUT trailing newline markers; a trailing newline does
/// not produce an empty final line (matching git's default rendering).
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Longest-common-subsequence alignment between two line slices.
fn lcs_ops(a: &[&str], b: &[&str]) -> Vec<DiffLine> {
    let n = a.len();
    let m = b.len();
    // dp[i][j] = LCS length of a[i..] and b[j..]
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(DiffLine::Context(a[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffLine::Del(a[i].to_string()));
            i += 1;
        } else {
            ops.push(DiffLine::Add(b[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffLine::Del(a[i].to_string()));
        i += 1;
    }
    while j < m {
        ops.push(DiffLine::Add(b[j].to_string()));
        j += 1;
    }
    ops
}

/// Render a unified diff with `context` unchanged lines around each hunk.
/// Identical inputs yield an empty string; labels appear on the `---`/`+++`
/// headers verbatim.
pub fn unified_diff(a_label: &str, b_label: &str, a: &str, b: &str, context: usize) -> String {
    let old = split_lines(a);
    let new = split_lines(b);
    if old == new {
        return String::new();
    }
    let ops = lcs_ops(&old, &new);
    let mut hunks: Vec<(usize, usize)> = Vec::new(); // inclusive start, exclusive end over ops
    for (idx, op) in ops.iter().enumerate() {
        if matches!(op, DiffLine::Context(_)) {
            continue;
        }
        match hunks.last_mut() {
            Some((_, end)) if idx - *end <= 2 * context => *end = idx + 1,
            _ => hunks.push((idx, idx + 1)),
        }
    }

    let mut out = String::new();
    out.push_str(&format!("--- {a_label}\n"));
    out.push_str(&format!("+++ {b_label}\n"));

    let ctx = context;

    for (start, end) in hunks {
        let lo = start.saturating_sub(ctx);
        let hi = (end + ctx).min(ops.len());

        // Old-side line numbers count Del+Context ops; new-side count Add+Context.
        let mut old_no = 1usize;
        let mut new_no = 1usize;
        for op in ops.iter().take(lo) {
            match op {
                DiffLine::Context(_) => {
                    old_no += 1;
                    new_no += 1;
                }
                DiffLine::Del(_) => old_no += 1,
                DiffLine::Add(_) => new_no += 1,
            }
        }
        let old_len = ops[lo..hi]
            .iter()
            .filter(|op| !matches!(op, DiffLine::Add(_)))
            .count();
        let new_len = ops[lo..hi]
            .iter()
            .filter(|op| !matches!(op, DiffLine::Del(_)))
            .count();

        // Git convention: a zero-length side reports the line BEFORE the
        // hunk (0 when the whole file is empty).
        let old_start = if old_len == 0 { old_no.saturating_sub(1) } else { old_no };
        let new_start = if new_len == 0 { new_no.saturating_sub(1) } else { new_no };
        out.push_str(&format!("@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"));
        for op in &ops[lo..hi] {
            out.push_str(&format!("{}{}\n", op.marker(), op.text()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers(diff: &str) -> Vec<String> {
        diff.lines()
            .filter(|l| !l.starts_with("---") && !l.starts_with("+++"))
            .map(String::from)
            .collect()
    }

    #[test]
    fn identical_inputs_yield_empty_diff() {
        assert_eq!(unified_diff("a", "b", "one\ntwo\n", "one\ntwo\n", 3), "");
        assert_eq!(unified_diff("a", "b", "", "", 3), "");
    }

    #[test]
    fn single_line_change_produces_one_hunk() {
        let d = unified_diff("a", "b", "one\ntwo\nthree\n", "one\nTWO\nthree\n", 1);
        let body = markers(&d);
        assert_eq!(body, vec!["@@ -1,3 +1,3 @@", " one", "-two", "+TWO", " three"]);
    }

    #[test]
    fn insertions_and_deletions_are_classified() {
        // context=0: the unchanged " c" separates two hunks.
        let d = unified_diff("a", "b", "a\nb\nc\n", "a\nx\ny\nc\nd\n", 0);
        let body = markers(&d);
        assert_eq!(
            body,
            vec![
                "@@ -2,1 +2,2 @@",
                "-b",
                "+x",
                "+y",
                "@@ -3,0 +5,1 @@",
                "+d"
            ]
        );
    }

    #[test]
    fn distant_changes_form_separate_hunks() {
        let a = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let b = "1\nTWO\n3\n4\n5\n6\n7\n8\n9\nTEN\n";
        let d = unified_diff("a", "b", a, b, 1);
        let body = markers(&d);
        assert_eq!(body.len(), 9);
        assert_eq!(body[0], "@@ -1,3 +1,3 @@");
        assert_eq!(body[5], "@@ -9,2 +9,2 @@");
        assert!(body.contains(&"-2".to_string()));
        assert!(body.contains(&"+TWO".to_string()));
        assert!(body.contains(&"-10".to_string()));
        assert!(body.contains(&"+TEN".to_string()));
    }

    #[test]
    fn empty_to_content_is_pure_addition() {
        let d = unified_diff("a", "b", "", "hello\nworld\n", 3);
        let body = markers(&d);
        assert_eq!(body, vec!["@@ -0,0 +1,2 @@", "+hello", "+world"]);
    }

    #[test]
    fn content_to_empty_is_pure_deletion() {
        let d = unified_diff("a", "b", "gone\n", "", 3);
        let body = markers(&d);
        assert_eq!(body, vec!["@@ -1,1 +0,0 @@", "-gone"]);
    }

    #[test]
    fn headers_use_labels() {
        let d = unified_diff("notes.md@1", "notes.md@current", "x\n", "y\n", 1);
        assert!(d.starts_with("--- notes.md@1\n+++ notes.md@current\n"));
    }

    #[test]
    fn lcs_ops_prefers_deletions_before_additions_on_ties() {
        let ops = lcs_ops(&["b"], &["x"]);
        assert_eq!(
            ops,
            vec![DiffLine::Del("b".into()), DiffLine::Add("x".into())]
        );
    }
}

