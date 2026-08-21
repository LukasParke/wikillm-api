//! OKF v0.2 bundle validation: per-file frontmatter/type checks plus
//! bundle-wide statistics.

use std::collections::BTreeMap;

use chrono::Utc;
use serde_json::Value;

use crate::okf::parse::split_frontmatter;
use crate::okf::trust::{derive_trust_tier, is_stale, normalize_verified};

static LOG_DATE_HEADING_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^## \d{4}-\d{2}-\d{2}\s*$").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IssueLevel {
    Error,
    Warning,
}

impl IssueLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueLevel::Error => "error",
            IssueLevel::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ValidationIssue {
    pub level: IssueLevel,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BundleStats {
    pub concepts: usize,
    pub by_type: BTreeMap<String, usize>,
    pub trust_tiers: BTreeMap<String, usize>,
    pub stale_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BundleValidationReport {
    pub valid: bool,
    pub errors: Vec<ValidationIssue>,
    pub warnings: Vec<ValidationIssue>,
    pub stats: BundleStats,
}

struct Frontmatter {
    data: Value,
    error: Option<String>,
}

/// Mirrors gray-matter semantics: absent delimiters mean no frontmatter (an
/// empty mapping, no error); present-but-unparseable YAML and non-mapping
/// YAML are errors.
fn parse_frontmatter(raw: &str) -> Frontmatter {
    let (src, _) = split_frontmatter(raw);
    let Some(src) = src else {
        return Frontmatter {
            data: Value::Object(serde_json::Map::new()),
            error: None,
        };
    };
    match serde_yaml::from_str::<Value>(src.trim()) {
        Ok(Value::Object(map)) => Frontmatter {
            data: Value::Object(map),
            error: None,
        },
        Ok(_) => Frontmatter {
            data: Value::Object(serde_json::Map::new()),
            error: Some("frontmatter is not a mapping".to_string()),
        },
        Err(cause) => Frontmatter {
            data: Value::Object(serde_json::Map::new()),
            error: Some(format!("unparseable YAML frontmatter: {cause}")),
        },
    }
}

fn is_concept_frontmatter(data: &Value) -> bool {
    data.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| !t.trim().is_empty())
}

fn issue(level: IssueLevel, path: &str, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        level,
        path: path.to_string(),
        message: message.into(),
    }
}

/// Validate a single concept file per OKF v0.2. Reserved filenames
/// (`index.md`/`log.md` at any depth) are exempt from the frontmatter/type
/// requirement; log.md date headings and root index.md okf_version produce
/// warnings. AGENTS.md/CLAUDE.md (case-insensitive basename) are bundle
/// configuration, not concepts. Cross-file link validation is out of scope.
pub fn validate_concept_file(rel_path: &str, raw: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let base = rel_path.rsplit('/').next().unwrap_or("");
    let lowered = base.to_lowercase();
    if lowered == "agents.md" || lowered == "claude.md" {
        return issues;
    }

    let frontmatter = parse_frontmatter(raw);
    if let Some(message) = &frontmatter.error {
        issues.push(issue(IssueLevel::Error, rel_path, message.clone()));
    }

    if base == "log.md" {
        for (index, line) in raw.lines().enumerate() {
            let is_h2 = line.starts_with("##") && !line.starts_with("###");
            if is_h2 && !LOG_DATE_HEADING_RE.is_match(line) {
                let quoted = serde_json::to_string(line).unwrap_or_else(|_| line.to_string());
                issues.push(issue(
                    IssueLevel::Warning,
                    &format!("{rel_path}#L{}", index + 1),
                    format!("log heading must match \"## YYYY-MM-DD\", got {quoted}"),
                ));
            }
        }
    } else if base == "index.md" {
        if rel_path == "index.md" {
            let is_string = frontmatter
                .data
                .get("okf_version")
                .map_or(true, Value::is_string);
            if !is_string {
                issues.push(issue(
                    IssueLevel::Warning,
                    rel_path,
                    "okf_version must be a string when present",
                ));
            }
        }
    } else if !is_concept_frontmatter(&frontmatter.data) {
        issues.push(issue(
            IssueLevel::Error,
            rel_path,
            "missing or empty 'type' in frontmatter",
        ));
    }
    issues
}

/// Aggregate per-file issues and concept statistics over a bundle. Stats
/// count concept files only (reserved `index.md`/`log.md` excluded).
pub fn validate_bundle<'a, I>(files: I) -> BundleValidationReport
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut trust_tiers: BTreeMap<String, usize> = BTreeMap::new();
    let mut concepts = 0usize;
    let mut stale_count = 0usize;

    for (rel_path, content) in files {
        for issue in validate_concept_file(rel_path, content) {
            if issue.level == IssueLevel::Error {
                errors.push(issue);
            } else {
                warnings.push(issue);
            }
        }

        let base = rel_path.rsplit('/').next().unwrap_or("");
        if base == "index.md" || base == "log.md" {
            continue;
        }
        let frontmatter = parse_frontmatter(content);
        if frontmatter.error.is_some() || !is_concept_frontmatter(&frontmatter.data) {
            continue;
        }
        concepts += 1;
        let concept_type = frontmatter
            .data
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        *by_type.entry(concept_type).or_insert(0) += 1;
        let tier = derive_trust_tier(
            normalize_verified(frontmatter.data.get("verified").unwrap_or(&Value::Null)).as_ref(),
        );
        *trust_tiers.entry(tier.to_string()).or_insert(0) += 1;
        if is_stale(
            frontmatter.data.get("stale_after").and_then(Value::as_str),
            Utc::now(),
        ) {
            stale_count += 1;
        }
    }

    BundleValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings,
        stats: BundleStats {
            concepts,
            by_type,
            trust_tiers,
            stale_count,
        },
    }
}
