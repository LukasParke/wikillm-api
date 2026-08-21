//! Markdown document parsing: frontmatter splitting, link/wikilink
//! extraction, and bundle-relative link resolution.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::error::{Error, Result};

static MARKDOWN_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    // !?[alt](  <target> | target  optional "title" )
    Regex::new(r#"!?\[[^\]\[]*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)"#).unwrap()
});
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]\[|]+)(?:\|[^\]\[]*)?\]\]").unwrap());
static SCHEME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.-]*:").unwrap());
static HAS_EXTENSION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[^./]+$").unwrap());

/// A parsed markdown document. `frontmatter` is always a JSON object (empty
/// when the document has no frontmatter block).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDocument {
    pub frontmatter: Value,
    pub body: String,
    pub links: Vec<String>,
    pub wikilinks: Vec<String>,
}

/// Split a raw document into `(frontmatter source, body)`. `frontmatter` is
/// `None` unless the document starts with a `---` fence that has a matching
/// closing `---` line; the body excludes both delimiter lines.
pub fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let Some(first_end) = raw.find('\n') else {
        return (None, raw);
    };
    if raw[..first_end].trim_end() != "---" {
        return (None, raw);
    }
    let rest = &raw[first_end + 1..];
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let fm = rest[..offset].trim_suffix_newline();
            let body = &rest[offset + line.len()..];
            return (Some(fm), body);
        }
        offset += line.len();
    }
    (None, raw)
}

trait TrimSuffixNewline {
    fn trim_suffix_newline(&self) -> &str;
}
impl TrimSuffixNewline for str {
    fn trim_suffix_newline(&self) -> &str {
        self.strip_suffix('\n')
            .map_or(self, |s| s.strip_suffix('\r').unwrap_or(s))
    }
}

/// Parse a markdown document into frontmatter (as a JSON object), body,
/// markdown link targets, and wikilink targets.
///
/// # Errors
/// Returns [`Error::Validation`] when frontmatter YAML is present but
/// unparseable.
pub fn parse_markdown_document(raw: &str) -> Result<ParsedDocument> {
    let (frontmatter_src, body) = split_frontmatter(raw);
    let frontmatter = match frontmatter_src {
        None => Value::Object(serde_json::Map::new()),
        Some(src) => match serde_yaml::from_str::<Value>(src.trim()) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(_) => Value::Object(serde_json::Map::new()),
            Err(cause) => {
                return Err(Error::Validation(format!(
                    "unparseable YAML frontmatter: {cause}"
                )))
            }
        },
    };
    Ok(ParsedDocument {
        frontmatter,
        body: body.to_string(),
        links: extract_links(body),
        wikilinks: extract_wikilinks(body),
    })
}

fn is_external(target: &str) -> bool {
    let lowered = target.to_lowercase();
    lowered.starts_with("http:") || lowered.starts_with("https:") || lowered.starts_with("mailto:")
}

/// Extract markdown link and image targets, skipping external http(s)/mailto
/// URLs and anchor-only references; `#anchor` suffixes are stripped;
/// order-preserving dedupe.
pub fn extract_links(body: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for caps in MARKDOWN_LINK_RE.captures_iter(body) {
        let mut target = caps[1].to_string();
        if target.starts_with('<') && target.ends_with('>') {
            target = target[1..target.len() - 1].to_string();
        }
        if is_external(&target) {
            continue;
        }
        match target.find('#') {
            Some(0) => continue,
            Some(hash) => target.truncate(hash),
            None => {}
        }
        if target.is_empty() || !seen.insert(target.clone()) {
            continue;
        }
        out.push(target);
    }
    out
}

/// Extract wikilink targets from `[[Target]]` and `[[Target|alias]]`;
/// order-preserving dedupe.
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for caps in WIKILINK_RE.captures_iter(body) {
        let target = caps[1].trim();
        if target.is_empty() || !seen.insert(target.to_string()) {
            continue;
        }
        out.push(target.to_string());
    }
    out
}

fn has_extension(segment: &str) -> bool {
    HAS_EXTENSION_RE.is_match(segment)
}

/// Resolve a link target to a bundle-relative candidate `.md` path.
///
/// - Leading `/` targets are bundle-root-relative; others resolve against the
///   directory of `source_rel_path`.
/// - `.md` is appended when the final segment lacks an extension.
/// - Returns `None` for external/mailto/anchor-only links and for paths that
///   escape the bundle root via `..`.
pub fn resolve_link_target(link: &str, source_rel_path: &str) -> Option<String> {
    if link.is_empty() || SCHEME_RE.is_match(link) || link.starts_with('#') {
        return None;
    }
    let stripped = match link.find('#') {
        Some(hash) => &link[..hash],
        None => link,
    };
    if stripped.is_empty() {
        return None;
    }

    let root_relative = stripped.starts_with('/');
    let mut stack: Vec<String> = Vec::new();
    if !root_relative {
        let parts: Vec<&str> = source_rel_path.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !part.is_empty() && *part != "." {
                stack.push((*part).to_string());
            }
        }
    }
    for part in stripped.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            seg => stack.push(seg.to_string()),
        }
    }
    if stack.is_empty() {
        return None;
    }
    let last = stack.len() - 1;
    if !has_extension(&stack[last]) {
        stack[last].push_str(".md");
    }
    Some(stack.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_variants() {
        assert_eq!(split_frontmatter("---\na: 1\n---\nbody"), (Some("a: 1"), "body"));
        assert_eq!(split_frontmatter("no fm"), (None, "no fm"));
        assert_eq!(split_frontmatter("---\nunterminated\n"), (None, "---\nunterminated\n"));
    }

    #[test]
    fn resolves_matrix() {
        assert_eq!(
            resolve_link_target("Other", "concepts/a.md"),
            Some("concepts/Other.md".into())
        );
        assert_eq!(resolve_link_target("../../../e.md", "a/b/c.md"), None);
        assert_eq!(resolve_link_target("#only", "a.md"), None);
    }
}
