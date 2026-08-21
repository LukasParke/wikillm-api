//! Filesystem path safety for the wiki root.
//!
//! Ports the TypeScript `fs/paths.ts` guard: traversal rejection, reserved
//! segment blocking, the top-level allowlist, and realpath containment.
//! `PathError` codes surface as `Error::Path("CODE: message")`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

const RESERVED_SEGMENTS: [&str; 4] = [".git", ".obsidian", "node_modules", ".trash"];

const ALLOWED_TOP_LEVEL: [&str; 4] = ["agents.md", "claude.md", "index.md", "log.md"];

fn path_error(code: &str, message: impl std::fmt::Display) -> Error {
    Error::Path(format!("{code}: {message}"))
}

/// Best-effort `decodeURIComponent`; falls back to the raw string on invalid
/// percent-escape sequences.
fn percent_decode(raw: &str) -> String {
    match percent_encoding::percent_decode_str(raw).decode_utf8() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => raw.to_string(),
    }
}

/// Lexically normalize a (possibly relative) path the way POSIX
/// `path.normalize` does: backslashes become slashes, duplicate separators
/// collapse, `.` segments drop, and `..` resolves against the stack (leading
/// `..` segments are kept; above an absolute root they collapse away).
fn normalize_rel(raw: &str) -> String {
    let slashed = raw.replace('\\', "/");
    let absolute = slashed.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in slashed.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if absolute {
                    continue;
                }
                match stack.last() {
                    Some(&"..") | None => stack.push(".."),
                    Some(_) => {
                        stack.pop();
                    }
                }
            }
            seg => stack.push(seg),
        }
    }
    let joined = stack.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Normalize a wiki-relative path: percent-decode, backslash conversion, and
/// lexical `.`/`..` resolution.
pub fn normalize_rel_path(rel_path: &str) -> String {
    normalize_rel(&percent_decode(rel_path))
}

fn safe_realpath(p: &Path) -> PathBuf {
    match fs::canonicalize(p) {
        Ok(real) => real,
        Err(_) => {
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(p)
            };
            lexically_normalize(&abs)
        }
    }
}

fn lexically_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            comp => out.push(comp.as_os_str()),
        }
    }
    out
}

/// Resolve a wiki-relative path to an absolute path, enforcing every safety
/// guard. Returns `Error::Path("CODE: message")` mirroring the TS `PathError`
/// codes: `MISSING_PATH`, `TRAVERSAL`, `EMPTY_PATH`, `RESERVED`,
/// `INVALID_NAMESPACE`, `OUTSIDE_ROOT`.
pub fn resolve_wiki_path(wiki_root: &str, rel_path: &str) -> Result<String> {
    if rel_path.is_empty() {
        return Err(path_error("MISSING_PATH", "Missing path"));
    }

    let normalized = normalize_rel(&percent_decode(rel_path));

    if normalized.starts_with("..") || normalized.starts_with('/') {
        return Err(path_error("TRAVERSAL", "Path traversal attempt"));
    }

    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(path_error("EMPTY_PATH", "Empty path"));
    }

    for seg in &segments {
        if *seg == ".." || *seg == "." {
            return Err(path_error("TRAVERSAL", "Path traversal attempt"));
        }
        if RESERVED_SEGMENTS.contains(seg) {
            return Err(path_error("RESERVED", format!("Reserved segment: {seg}")));
        }
    }

    // Only allow direct access to top-level markdowns in the allowed set.
    if segments.len() == 1 && !ALLOWED_TOP_LEVEL.contains(&segments[0].to_lowercase().as_str()) {
        return Err(path_error(
            "INVALID_NAMESPACE",
            "Path must be inside wiki/ or raw/",
        ));
    }

    let root = Path::new(wiki_root);
    let mut abs = root.to_path_buf();
    for seg in &segments {
        abs.push(seg);
    }

    let root_real = safe_realpath(root);
    let target_real = safe_realpath(&abs);

    if target_real != root_real && !target_real.starts_with(&root_real) {
        return Err(path_error("OUTSIDE_ROOT", "Resolved path escapes wiki root"));
    }

    Ok(target_real.to_string_lossy().into_owned())
}

/// True for paths the watcher/indexer should skip: reserved segments
/// (`.git`, `.obsidian`, `node_modules`, `.trash`), temp downloads, and
/// Finder metadata.
pub fn is_ignored_path(rel_path: &str) -> bool {
    rel_path.split('/').any(|seg| {
        RESERVED_SEGMENTS.contains(&seg)
            || seg.ends_with(".tmp") || seg.ends_with(".db") || seg.ends_with(".db-wal") || seg.ends_with(".db-shm")
            || seg.ends_with(".crdownload")
            || seg == ".DS_Store"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_rel_paths() {
        assert_eq!(normalize_rel_path("wiki/./a//b.md"), "wiki/a/b.md");
        assert_eq!(normalize_rel_path("wiki/x/../y.md"), "wiki/y.md");
        assert_eq!(normalize_rel_path("a\\b.md"), "a/b.md");
        assert_eq!(normalize_rel_path("../a.md"), "../a.md");
    }

    #[test]
    fn ignores_temp_and_reserved() {
        assert!(is_ignored_path("wiki/page.md.tmp"));
        assert!(is_ignored_path(".obsidian/workspace.json"));
        assert!(is_ignored_path("a/.DS_Store"));
        assert!(!is_ignored_path("wiki/page.md"));
    }
}
