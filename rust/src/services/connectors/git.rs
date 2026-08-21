//! Git connector: shallow clone into a temp cache, fetch/reset per poll.
//! Emits every matched file; the index pipeline dedupes unchanged content.

use crate::domain::{ConnectorConfig};
use crate::error::{Error, Result};
use serde_json::Value;
use std::path::PathBuf;
use tokio::process::Command;

fn cache_dir(url: &str) -> PathBuf {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(url.as_bytes());
    let hex = hex::encode(hash);
    std::env::temp_dir().join(format!("wikillm-git-{}", &hex[..16]))
}

async fn git(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| Error::Other(format!("git spawn: {e}")))?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn walk_md(dir: &std::path::Path, root: &std::path::Path, extensions: &[String], out: &mut Vec<(String, String, i64)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let full = entry.path();
        let rel = full.strip_prefix(root).unwrap_or(&full).to_string_lossy().replace('\\', "/");
        if rel == ".git" || rel.starts_with(".git/") {
            continue;
        }
        if full.is_dir() {
            walk_md(&full, root, extensions, out);
        } else if full.is_file() {
            let ext = full.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            if extensions.contains(&ext) {
                let mtime = full
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                if let Ok(content) = std::fs::read_to_string(&full) {
                    out.push((rel, content, mtime));
                }
            }
        }
    }
}

/// Returns (docs as (rel_path, title, content, mtime)), new state.
pub async fn poll(config: &Value, state: &Value) -> Result<(Vec<(String, String, String, i64)>, Value)> {
    let url = config.get("url").and_then(|v| v.as_str()).ok_or_else(|| Error::Validation("git connector requires config.url".into()))?.to_string();
    let branch = config.get("branch").and_then(|v| v.as_str());
    let extensions: Vec<String> = config
        .get("extensions")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec![".md".into()]);
    let dir = cache_dir(&url);

    if dir.join(".git").exists() {
        let fetch_ref = branch.unwrap_or("HEAD");
        git(&dir, &["fetch", "--depth", "1", "origin", fetch_ref]).await?;
        git(&dir, &["reset", "--hard", "FETCH_HEAD"]).await?;
    } else {
        let mut args = vec!["clone", "--depth", "1"];
        if let Some(b) = branch {
            args.extend(["--branch", b]);
        }
        args.push(url.as_str());
        let parent = dir.parent().unwrap_or(std::path::Path::new("."));
        let _ = std::fs::create_dir_all(parent);
        git(parent, &args).await?;
    }
    let commit = git(&dir, &["rev-parse", "HEAD"]).await?.trim().to_string();
    if state.get("commit").and_then(|v| v.as_str()) == Some(commit.as_str()) {
        return Ok((Vec::new(), state.clone()));
    }
    let mut files = Vec::new();
    walk_md(&dir, &dir, &extensions, &mut files);
    files.sort();
    Ok((
        files
            .into_iter()
            .map(|(rel, content, mtime)| (rel.clone(), content, rel, mtime))
            .map(|(a, b, c, d)| (a, b, c, d))
            .collect(),
        serde_json::json!({ "commit": commit }),
    ))
}

// ConnectorConfig re-export guard for signature parity with TS naming.
#[allow(dead_code)]
fn _cfg_shape(_: ConnectorConfig) {}
