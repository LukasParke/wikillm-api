//! Promotion service (wave-2): clusters unpromoted promote-candidate
//! memories and synthesizes draft wiki pages from each cluster.
//!
//! Grouping uses embedding clusters when an embedder is configured,
//! otherwise word-set Jaccard overlap (threshold [`JACCARD_THRESHOLD`]).
//! Each cluster costs exactly one LLM call producing the page body; the
//! OKF frontmatter (type/status/generated/provenance) is composed locally
//! so the emitted file always matches the expected shape regardless of
//! model output drift.
//!
//! Pages are written under `derived/promotions/` via the atomic file
//! writer; indexing happens through the normal watcher pass (the HTTP
//! write pipeline lives in http/mod.rs which this module does not touch).
//!
//! NOTE (integration wiring, see handoff): this service relies on two
//! Store-trait methods added during integration:
//!   - `async fn list_promotable_memories(&self, limit: i64)
//!        -> Result<Vec<crate::services::memory::AgentMemory>>`
//!      -- `SELECT * FROM memories WHERE promote_candidate = 1
//!         AND promoted_at IS NULL ORDER BY created_at ASC LIMIT ?`
//!   - `async fn mark_memory_promoted(&self, id: &str, promoted_at: &str)
//!        -> Result<()>`
//!      -- `UPDATE memories SET promoted_at = ? WHERE id = ?`

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Result;
use crate::fs::atomic;
use crate::llm::embedder::EmbedderLike;
use crate::llm::provider::DynLlmProvider;
use crate::services::memory::AgentMemory;
use crate::services::settings::SettingsService;
use crate::store::Store;

/// Word-set Jaccard threshold for grouping without embeddings.
const JACCARD_THRESHOLD: f64 = 0.25;
/// Cosine threshold for greedy embedding-cluster grouping.
const COSINE_THRESHOLD: f64 = 0.75;
/// Candidate pool fetched per run; groups form from this window.
const CANDIDATE_POOL: i64 = 200;

const PROMOTION_SYSTEM: &str = "You are a wiki-page synthesis engine. Given numbered agent memories that were clustered as related, write one cohesive wiki page. Respond ONLY with strict JSON: {\"title\":\"...\",\"summary\":\"...\",\"details\":[\"...\"]}. Rules: title is a short noun phrase (no file extension); summary is 2-4 sentences; details is an array of 2-6 short markdown strings (paragraphs or '- ' bullets); preserve concrete facts, attribute uncertainty when memories conflict; never invent facts absent from the memories.";

/// One synthesized draft page produced by a promotion run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PromotedPage {
    /// Wiki-relative path of the written draft.
    pub rel_path: String,
    pub title: String,
    /// Memory ids folded into this page.
    pub memory_ids: Vec<String>,
}

pub struct PromotionService {
    store: Arc<dyn Store>,
    settings: Arc<SettingsService>,
    llm: Option<DynLlmProvider>,
    embedder: Option<Box<dyn EmbedderLike>>,
    /// Filesystem directory receiving drafts (<wiki_root>/derived/promotions).
    out_dir: PathBuf,
}

impl PromotionService {
    pub fn new(
        store: Arc<dyn Store>,
        settings: Arc<SettingsService>,
        llm: Option<DynLlmProvider>,
        embedder: Option<Box<dyn EmbedderLike>>,
        out_dir: PathBuf,
    ) -> Self {
        Self { store, settings, llm, embedder, out_dir }
    }

    /// Run one promotion pass. Returns the pages written this run.
    ///
    /// `limit <= 0` falls back to the `promotion_max_pages` setting; an
    /// explicit positive limit is additionally capped by that setting.
    /// No-op unless `promotion_enabled` is true and an LLM is configured.
    pub async fn run(&self, limit: i64) -> Result<Vec<PromotedPage>> {
        if !self.settings.get_bool("promotion_enabled").await? {
            return Ok(Vec::new());
        }
        let configured = self.settings.get_i64("promotion_max_pages").await?;
        let mut effective = if limit > 0 { limit } else { configured };
        if configured > 0 {
            effective = effective.min(configured);
        }
        if effective <= 0 {
            return Ok(Vec::new());
        }
        let llm = match &self.llm {
            Some(llm) => llm,
            None => {
                tracing::warn!("promotion enabled but no LLM provider configured; skipping");
                return Ok(Vec::new());
            }
        };

        let candidates = self.store.list_promotable_memories(CANDIDATE_POOL).await?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let groups = self.group_candidates(&candidates).await;
        let now = chrono::Utc::now().to_rfc3339();
        let mut used_slugs: HashSet<String> = HashSet::new();
        let mut pages = Vec::new();

        for group in groups {
            if pages.len() as i64 >= effective {
                break;
            }
            let members: Vec<&AgentMemory> = group.iter().map(|&i| &candidates[i]).collect();
            let (title, summary, details) = match self.synthesize(llm, &members).await {
                Some(parts) => parts,
                None => continue, // unusable model output: leave unpromoted for retry
            };
            let slug = unique_slug(slugify(&title), &mut used_slugs);
            let rel_path = format!("derived/promotions/{slug}.md");
            let markdown =
                render_page(&title, &summary, &details, &members, &now);
            atomic::atomic_write(self.out_dir.join(format!("{slug}.md")), markdown)?;

            for m in &members {
                self.store.mark_memory_promoted(&m.id, &now).await?;
            }
            tracing::info!(
                path = %rel_path,
                memories = members.len(),
                "promoted memory cluster to draft page"
            );
            pages.push(PromotedPage {
                rel_path,
                title,
                memory_ids: members.iter().map(|m| m.id.clone()).collect(),
            });
        }

        Ok(pages)
    }

    /// Cluster candidate indices: embedding greedy nearest-centroid when
    /// the embedder is available, word-set Jaccard otherwise. Falls back
    /// to Jaccard if embedding fails mid-run.
    async fn group_candidates(&self, candidates: &[AgentMemory]) -> Vec<Vec<usize>> {
        let contents: Vec<&str> = candidates.iter().map(|m| m.content.as_str()).collect();
        if let Some(embedder) = &self.embedder {
            let owned: Vec<String> = contents.iter().map(|c| c.to_string()).collect();
            match embedder.embed(&owned).await {
                Ok(vectors) if vectors.len() == contents.len() => {
                    return group_by_vectors(&vectors, COSINE_THRESHOLD);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "promotion embedding failed; using Jaccard grouping");
                }
                _ => tracing::warn!("embedding count mismatch; using Jaccard grouping"),
            }
        }
        group_by_jaccard(&contents, JACCARD_THRESHOLD)
    }

    /// One LLM call per cluster. Returns `None` when the response cannot
    /// be parsed into the expected shape (caller leaves the cluster
    /// unpromoted so a later run can retry).
    async fn synthesize(
        &self,
        llm: &DynLlmProvider,
        members: &[&AgentMemory],
    ) -> Option<(String, String, Vec<String>)> {
        let mut user = String::from("Memories:\n");
        for (i, m) in members.iter().enumerate() {
            let session = m.source_session_id.as_deref().unwrap_or("-");
            user.push_str(&format!(
                "{}. [{}] (session: {}) {}\n",
                i + 1,
                crate::services::memory::memory_type_to_str(&m.memory_type),
                session,
                m.content
            ));
        }
        let messages: Vec<(&str, &str)> = vec![
            ("system", PROMOTION_SYSTEM),
            ("user", user.trim_end()),
        ];
        let raw = match llm.chat(&messages, 0.2, 1500).await {
            Ok(raw) => raw,
            Err(err) => {
                tracing::warn!(error = %err, "promotion synthesis call failed; skipping cluster");
                return None;
            }
        };
        parse_synthesis(&raw)
    }
}

#[derive(serde::Deserialize)]
struct SynthesisOut {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    details: Vec<String>,
}

/// Tolerant strict-JSON parse: extracts the outermost object from the
/// model response before deserializing. Falls back to a title derived
/// from the first detail/summary line when fields are missing.
fn parse_synthesis(raw: &str) -> Option<(String, String, Vec<String>)> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')? + 1;
    if end <= start {
        return None;
    }
    let out: SynthesisOut = serde_json::from_str(&raw[start..end]).ok()?;
    let details: Vec<String> =
        out.details.into_iter().map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).collect();
    let summary = out.summary.map(|s| s.trim().to_string()).unwrap_or_default();
    let fallback = details
        .first()
        .cloned()
        .unwrap_or_else(|| summary.clone());
    let title = out
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| nonempty(&fallback))?;
    if summary.is_empty() && details.is_empty() {
        return None;
    }
    Some((title, summary, details))
}

fn nonempty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// Lowercase word tokens starting with an ASCII letter, for Jaccard grouping.
fn tokenize(content: &str) -> HashSet<String> {
    content
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().next().map_or(false, |c| c.is_ascii_alphabetic()))
        .map(str::to_string)
        .collect()
}

fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let sa = tokenize(a);
    let sb = tokenize(b);
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    inter as f64 / union as f64
}
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na * nb)) as f64
}

/// Greedy clustering over item order: join the most similar existing
/// cluster (any-member max similarity, or running centroid for vectors)
/// when it clears the threshold, otherwise open a new cluster.
/// Returns index clusters, order-stable.
fn group_indices<F>(n: usize, similarity: F, threshold: f64) -> Vec<Vec<usize>>
where
    F: Fn(usize, usize) -> f64,
{
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        let mut best: Option<(usize, f64)> = None;
        for (gi, g) in groups.iter().enumerate() {
            let sim = g.iter().map(|&j| similarity(i, j)).fold(-1.0, f64::max);
            if sim >= threshold && best.map_or(true, |(_, bs)| sim > bs) {
                best = Some((gi, sim));
            }
        }
        match best {
            Some((gi, _)) => groups[gi].push(i),
            None => groups.push(vec![i]),
        }
    }
    groups
}

fn group_by_jaccard(contents: &[&str], threshold: f64) -> Vec<Vec<usize>> {
    group_indices(contents.len(), |a, b| jaccard_similarity(contents[a], contents[b]), threshold)
}

fn group_by_vectors(vectors: &[Vec<f32>], threshold: f64) -> Vec<Vec<usize>> {
    // Running centroid per cluster: sum vector + count.
    let mut sums: Vec<Vec<f32>> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, v) in vectors.iter().enumerate() {
        let mut best: Option<(usize, f64)> = None;
        for gi in 0..groups.len() {
            let mean: Vec<f32> = sums[gi].iter().map(|s| s / counts[gi] as f32).collect();
            let sim = cosine_similarity(&mean, v);
            if sim >= threshold && best.map_or(true, |(_, bs)| sim > bs) {
                best = Some((gi, sim));
            }
        }
        match best {
            Some((gi, _)) => {
                for (cell, x) in sums[gi].iter_mut().zip(v) {
                    *cell += x;
                }
                counts[gi] += 1;
                groups[gi].push(i);
            }
            None => {
                sums.push(v.clone());
                counts.push(1);
                groups.push(vec![i]);
            }
        }
    }
    groups
}

fn slugify(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let collapsed = slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if collapsed.is_empty() {
        format!("promo-{}", &ulid::Ulid::new().to_string()[..12].to_lowercase())
    } else {
        collapsed
    }
}

/// Dedupe slugs across pages within one run (`-2`, `-3`, ... suffixes).
fn unique_slug(base: String, used: &mut HashSet<String>) -> String {
    let mut candidate = base.clone();
    let mut n = 1;
    while !used.insert(candidate.clone()) {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    candidate
}

/// Single-quote a YAML scalar (doubled internal quotes).
fn yaml_scalar(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn render_page(
    title: &str,
    summary: &str,
    details: &[String],
    members: &[&AgentMemory],
    now: &str,
) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("---\n");
    out.push_str("type: concept\n");
    out.push_str("status: draft\n");
    out.push_str("generated:\n");
    out.push_str("  by: wikillm-promoter\n");
    out.push_str(&format!("  at: {}\n", yaml_scalar(now)));
    out.push_str("provenance:\n");
    for m in members {
        out.push_str(&format!("- memory: {}\n", yaml_scalar(&m.id)));
        if let Some(sess) = &m.source_session_id {
            out.push_str(&format!("  session: {}\n", yaml_scalar(sess)));
        }
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", title.trim()));
    out.push_str("## Summary\n\n");
    out.push_str(summary.trim());
    out.push_str("\n\n## Details\n\n");
    for d in details {
        out.push_str(d.trim());
        out.push_str("\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: &str, content: &str, session: Option<&str>) -> AgentMemory {
        AgentMemory {
            id: id.into(),
            scope_key: "u|test|".into(),
            memory_type: crate::services::memory::MemoryType::Semantic,
            content: content.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            accessed_at: "2026-01-01T00:00:00Z".into(),
            access_count: 0,
            source_session_id: session.map(str::to_string),
            source_ref: None,
        }
    }

    #[test]
    fn jaccard_groups_related_contents() {
        let a = "deploy pipeline runs terraform apply in staging";
        let b = "terraform apply staging deploy pipeline gates on checks";
        let c = "the office wifi password rotates monthly";
        assert!(jaccard_similarity(a, b) >= 0.25);
        assert!(jaccard_similarity(a, c) < 0.25);

        let grouped = group_by_jaccard(&[a, b, c], JACCARD_THRESHOLD);
        assert_eq!(grouped, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn cosine_clusters_parallel_vectors_only() {
        let vectors = vec![vec![1.0, 0.0], vec![0.9, 0.1], vec![0.0, 1.0]];
        let grouped = group_by_vectors(&vectors, 0.75);
        assert_eq!(grouped, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn zero_vector_never_matches() {
        let vectors = vec![vec![1.0, 0.0], vec![0.0, 0.0]];
        assert_eq!(cosine_similarity(&vectors[0], &vectors[1]), 0.0);
        let grouped = group_by_vectors(&vectors, 0.75);
        assert_eq!(grouped, vec![vec![0], vec![1]]);
    }

    #[test]
    fn parses_strict_json_with_prose_wrapping() {
        let raw = "Here you go:\n{\"title\":\"Deploy Pipeline\",\"summary\":\"How deploys work.\",\"details\":[\"- gated on CI\",\"- terraform apply\"]}\nDone.";
        let (title, summary, details) = parse_synthesis(raw).unwrap();
        assert_eq!(title, "Deploy Pipeline");
        assert_eq!(summary, "How deploys work.");
        assert_eq!(details, vec!["- gated on CI", "- terraform apply"]);
    }

    #[test]
    fn parse_fails_on_garbage() {
        assert!(parse_synthesis("no json here").is_none());
        assert!(parse_synthesis("{\"title\":\"\",\"summary\":\"\",\"details\":[]}").is_none());
    }

    #[test]
    fn slugify_collapses_and_falls_back() {
        assert_eq!(slugify("Deploy Pipeline!"), "deploy-pipeline");
        assert!(slugify("???").starts_with("promo-"));
    }

    #[test]
    fn unique_slug_suffixes_collisions() {
        let mut used = HashSet::new();
        assert_eq!(unique_slug(slugify("Same"), &mut used), "same");
        assert_eq!(unique_slug(slugify("Same"), &mut used), "same-2");
    }

    #[test]
    fn rendered_page_has_okf_frontmatter_and_sections() {
        let members = [mem("mem-abc", "content", Some("sess-1")), mem("mem-def", "other", None)];
        let refs: Vec<&AgentMemory> = members.iter().collect();
        let page = render_page("Title", "Sum.", &["Detail one.".into()], &refs, "2026-08-22T00:00:00Z");
        assert!(page.starts_with("---\ntype: concept\nstatus: draft\ngenerated:\n  by: wikillm-promoter\n"));
        assert!(page.contains("- memory: 'mem-abc'\n  session: 'sess-1'\n"));
        assert!(page.contains("- memory: 'mem-def'\n"));
        assert!(!page.contains("session: 'sess-1'\n  session"));
        assert!(page.contains("## Summary\n\nSum."));
        assert!(page.contains("## Details\n\nDetail one."));
    }

}
