//! Bundle-level OKF validation over the wiki root.

use crate::config::Config;
use crate::error::Result;
use crate::okf::validate::{validate_bundle, BundleValidationReport};
use std::path::Path;

pub struct OkfService {
    config: Config,
    layout_setting: std::sync::Arc<tokio::sync::RwLock<String>>,
}

impl OkfService {
    pub fn new(config: Config, layout_setting: std::sync::Arc<tokio::sync::RwLock<String>>) -> Self {
        Self { config, layout_setting }
    }

    fn collect_markdown(root: &str) -> Vec<(String, String)> {
        fn visit(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                let full = entry.path();
                let rel = full.strip_prefix(root).unwrap_or(&full).to_string_lossy().replace('\\', "/");
                if crate::fs::paths::is_ignored_path(&rel) {
                    continue;
                }
                if full.is_dir() {
                    visit(&full, root, out);
                } else if full.extension().map(|e| e == "md").unwrap_or(false) {
                    if let Ok(content) = std::fs::read_to_string(&full) {
                        out.push((rel, content));
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(Path::new(root), Path::new(root), &mut out);
        out
    }

    pub async fn validate_bundle(&self) -> Result<BundleValidationReport> {
        let files = Self::collect_markdown(&self.config.wiki_root);
        Ok(validate_bundle(files.iter().map(|(r, c)| (r.as_str(), c.as_str()))))
    }

    pub async fn layout_profile(&self) -> &'static str {
        let configured = self.layout_setting.read().await.clone();
        if configured != "auto" {
            return if configured == "wikillm" { "wikillm" } else { "okf" };
        }
        let root = Path::new(&self.config.wiki_root);
        let root_index = root.join("index.md");
        if root_index.is_file() {
            if let Ok(content) = std::fs::read_to_string(&root_index) {
                if content.contains("okf_version") {
                    return "okf";
                }
            }
        }
        if root.join("wiki").is_dir() { "wikillm" } else { "okf" }
    }
}
