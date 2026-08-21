//! Single indexing path for every document (FS-backed or connector-fed):
//! parse → OKF extraction → chunk → store → link edges → embed queue.
//!
//! The embed queue is a separate worker (`EmbedQueue`) so no self-referential
//! spawn is needed: the pipeline sends document ids over an unbounded channel
//! and the worker owns the distill→embed sequencing.

use crate::domain::*;
use crate::error::{Error, Result};
use crate::store::Store;
use crate::llm::embedder::EmbedderLike;
use crate::okf::trust::normalize_verified;
use crate::okf::parse::{extract_links, extract_wikilinks, resolve_link_target};
use crate::services::search::SharedLlm;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct FileAttribution {
    pub source: Option<String>,
    pub operation_id: Option<String>,
}

#[derive(Clone)]
pub struct RuntimeFlags {
    pub llm: SharedLlm,
    pub embedder: Arc<std::sync::RwLock<Option<Arc<dyn EmbedderLike>>>>,
    pub distill_enabled: Arc<std::sync::atomic::AtomicBool>,
}

impl RuntimeFlags {
    pub fn embedder(&self) -> Option<Arc<dyn EmbedderLike>> {
        self.embedder.read().ok().and_then(|g| g.clone())
    }
    pub fn distill_enabled(&self) -> bool {
        self.distill_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct IndexPipeline {
    wiki_root: String,
    store: Arc<dyn Store>,
    flags: RuntimeFlags,
    embed_tx: tokio::sync::mpsc::UnboundedSender<String>,
    change_emitter: Mutex<Option<Box<dyn Fn(ChangeEventData) + Send + Sync>>>,
}

impl IndexPipeline {
    pub fn new(
        wiki_root: &str,
        store: Arc<dyn Store>,
        flags: RuntimeFlags,
        embed_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            wiki_root: wiki_root.to_string(),
            store,
            flags,
            embed_tx,
            change_emitter: Mutex::new(None),
        }
    }

    /// Live broadcasting for API-attributed changes; external ones flow
    /// through the watcher to avoid double emission.
    pub async fn set_change_emitter(
        &self,
        emit: Box<dyn Fn(ChangeEventData) + Send + Sync>,
    ) {
        *self.change_emitter.lock().await = Some(emit);
    }

    /// Index a file under WIKI_ROOT. Returns a change event when the document
    /// was created/modified; None when unchanged (idempotent replays).
    pub async fn handle_file_change(
        &self,
        rel_path: &str,
        attribution: FileAttribution,
    ) -> Result<Option<ChangeEventData>> {
        let doc = self.read_fs_document(rel_path).await?;
        let Some(doc) = doc else {
            let existing = self.store.get_document(rel_path).await?;
            return match existing {
                Some(e) if e.origin == "wiki" => {
                    self.store.delete_document(rel_path).await?;
                    Ok(Some(
                        self.emit(rel_path, "deleted", Some(e.hash), None, &attribution)
                            .await,
                    ))
                }
                _ => Ok(None),
            };
        };
        let existing = self.store.get_document(rel_path).await?;
        if existing
            .as_ref()
            .is_some_and(|e| e.hash == doc.hash && e.origin == "wiki")
        {
            return Ok(None);
        }
        self.index_document(&doc).await?;
        Ok(Some(self.emit(
            rel_path,
            if existing.is_some() { "modified" } else { "created" },
            existing.map(|e| e.hash),
            Some(doc.hash.clone()),
            &attribution,
        )
        .await))
    }

    /// Index connector-materialized content (not backed by WIKI_ROOT).
    pub async fn index_external_content(
        &self,
        rel_path: &str,
        content: &str,
        origin: &str,
        title: Option<&str>,
        content_type: Option<&str>,
        mtime: Option<i64>,
    ) -> Result<()> {
        let hash = crate::fs::atomic::hash_content(content);
        let existing = self.store.get_document(rel_path).await?;
        if existing.as_ref().is_some_and(|e| e.hash == hash) {
            return Ok(());
        }
        let parsed = crate::okf::parse::parse_markdown_document(content)?;
        let links = extract_links(&parsed.body);
        let wikilinks = extract_wikilinks(&parsed.body);
        let mut resolved: Vec<String> = Vec::new();
        for link in links.iter().chain(wikilinks.iter()) {
            if let Some(t) = resolve_link_target(link, rel_path) {
                resolved.push(format!("/{t}"));
            }
        }
        resolved.dedup();

        let fm = &parsed.frontmatter;
        let doc = DocumentInput {
            rel_path: rel_path.to_string(),
            kind: DocKind::Doc,
            origin: origin.to_string(),
            title: title
                .map(String::from)
                .or_else(|| fm.get("title").and_then(|v| v.as_str()).map(String::from))
                .or_else(|| Some(basename_title(rel_path))),
            summary: fm.get("description").and_then(|v| v.as_str()).map(String::from),
            body: parsed.body.clone(),
            frontmatter: fm.clone(),
            word_count: count_words(&parsed.body),
            outgoing_links: resolved,
            hash,
            mtime: mtime.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
            content_type: content_type.map(String::from).or(Some("text/markdown".into())),
            ..DocumentInput::default()
        };
        self.index_document(&doc).await
    }

    pub async fn remove_document(&self, rel_path: &str) -> Result<()> {
        self.store.delete_document(rel_path).await
    }

    /// Remove all indexed documents for a connector origin.
    pub async fn remove_origin_documents(&self, origin: &str) -> Result<()> {
        self.store.delete_derived_for_origin(origin).await
    }

    /// Full rebuild of the wiki-origin index from WIKI_ROOT.
    pub async fn reindex_all(&self) -> Result<usize> {
        self.store.delete_derived_for_origin("wiki").await?;
        let files = walk_files(&self.wiki_root);
        let mut count = 0usize;
        for rel in files {
            match self.handle_file_change(&rel, FileAttribution::default()).await {
                Ok(_) => count += 1,
                Err(e) => eprintln!("reindex failed for {rel}: {e}"),
            }
        }
        Ok(count)
    }

    // ------------------------------------------------------------------

    pub async fn index_document(&self, doc: &DocumentInput) -> Result<()> {
        self.store.upsert_document(doc).await?;
        let record = self
            .store
            .get_document(&doc.rel_path)
            .await?
            .ok_or_else(|| Error::Other("post-upsert read failed".into()))?;
        let chunks = build_chunks(&record);
        self.store.replace_chunks(&record.id, &chunks).await?;

        let targets: Vec<String> = record
            .outgoing_links
            .iter()
            .map(|link| link.trim_start_matches('/').to_string())
            .filter(|t| !t.is_empty() && t != &record.rel_path)
            .collect();
        self.store.replace_edges(&record.rel_path, &targets).await?;

        if self.flags.embedder().is_some() && !chunks.is_empty() {
            let _ = self.embed_tx.send(record.id.clone());
        }
        Ok(())
    }

    async fn read_fs_document(&self, rel_path: &str) -> Result<Option<DocumentInput>> {
        use std::path::Path;
        let abs = Path::new(&self.wiki_root).join(rel_path);
        if !abs.is_file() {
            return Ok(None);
        }
        let meta = std::fs::metadata(&abs)?;
        let mtime = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if rel_path.ends_with(".md") {
            let raw = std::fs::read_to_string(&abs)?;
            let parsed = crate::okf::parse::parse_markdown_document(&raw)?;
            let links = extract_links(&parsed.body);
            let wikilinks = extract_wikilinks(&parsed.body);
            let mut resolved: Vec<String> = Vec::new();
            for link in links.iter().chain(wikilinks.iter()) {
                if let Some(t) = resolve_link_target(link, rel_path) {
                    resolved.push(format!("/{t}"));
                }
            }
            resolved.dedup();

            let fm = parsed.frontmatter;
            let str_field = |k: &str| fm.get(k).and_then(|v| v.as_str()).map(String::from);
            let tags = || -> Vec<String> {
                fm.get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                    .unwrap_or_default()
            };
            let _generated = || -> (Option<String>, Option<String>) {
                match fm.get("generated") {
                    Some(Value::Object(o)) => (
                        o.get("by").and_then(|v| v.as_str()).map(String::from),
                        o.get("at").and_then(|v| v.as_str()).map(String::from),
                    ),
                    _ => (None, None),
                }
            };
            let (gen_by, gen_at) = generated_fields(&fm);

            return Ok(Some(DocumentInput {
                rel_path: rel_path.to_string(),
                kind: DocKind::Page,
                origin: "wiki".into(),
                title: str_field("title")
                    .or_else(|| Some(basename_title(rel_path))),
                summary: str_field("description"),
                body: parsed.body.clone(),
                frontmatter: fm.clone(),
                word_count: count_words(&parsed.body),
                outgoing_links: dedupe(resolved),
                hash: crate::fs::atomic::hash_content(&raw),
                mtime,
                content_type: Some("text/markdown".into()),
                okf_type: str_field("type"),
                tags: tags(),
                status: str_field("status"),
                stale_after: str_field("stale_after"),
                resource: str_field("resource"),
                generated_by: gen_by,
                generated_at: gen_at,
                verified: normalize_verified(&fm.get("verified").cloned().unwrap_or(Value::Null)),
                provenance: fm.get("sources").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter(|e| e.is_object())
                        .cloned()
                        .collect::<Vec<Value>>()
                }),
                updated_at: None,
                updated_by: None,
            }));
        }

        // non-markdown: text-like files are indexed as single-content sources
        let ext = std::path::Path::new(rel_path)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let chunkable =
            [".txt", ".csv", ".json", ".yaml", ".yml", ".toml"].contains(&ext.as_str())
                || crate::ingest::chunkers::detect_language(rel_path).is_some();
        let raw = std::fs::read(&abs)?;
        let content = String::from_utf8_lossy(&raw).to_string();
        Ok(Some(DocumentInput {
            rel_path: rel_path.to_string(),
            kind: DocKind::Source,
            origin: "wiki".into(),
            title: Some(basename_title(rel_path)),
            summary: None,
            body: if chunkable { content.clone() } else { String::new() },
            frontmatter: Value::Null,
            word_count: if chunkable { count_words(&content) } else { 0 },
            outgoing_links: Vec::new(),
            hash: crate::fs::atomic::hash_content(&content),
            mtime,
            content_type: Some(infer_content_type(ext.as_str()).to_string()),
            ..DocumentInput::default()
        }))
    }

    async fn emit(
        &self,
        rel_path: &str,
        change_type: &str,
        old_hash: Option<String>,
        new_hash: Option<String>,
        attribution: &FileAttribution,
    ) -> ChangeEventData {
        let change = ChangeEventData {
            id: ulid::Ulid::new().to_string(),
            rel_path: rel_path.to_string(),
            change_type: change_type.to_string(),
            old_hash,
            new_hash,
            source: attribution.source.clone(),
            operation_id: attribution.operation_id.clone(),
            detected_at: now_iso(),
        };
        let _ = self.store.insert_change(&change).await;
        if attribution.source.as_deref() == Some("api") {
            if let Some(emitter) = self.change_emitter.lock().await.as_ref() {
                emitter(change.clone());
            }
        }
        change
    }
}

/// Background distill→embed worker. Owns the receive side of the queue.
pub struct EmbedQueue;

impl EmbedQueue {
    /// Runs until the send half is dropped.
    pub async fn run(
        mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
        store: Arc<dyn Store>,
        flags: RuntimeFlags,
    ) {
        while let Some(document_id) = rx.recv().await {
            if flags.distill_enabled() {
                if let Some(llm) = flags.llm.read().ok().and_then(|g| g.clone()) {
                    if let Err(err) = distill_document(&store, document_id.as_str(), llm).await {
                        eprintln!("distill failed for {document_id}: {err}");
                    }
                }
            }
            if let Err(err) = embed_document(&store, &flags, document_id.as_str()).await {
                eprintln!("embed failed for {document_id}: {err}");
            }
        }
    }
}

async fn embed_document(store: &Arc<dyn Store>, flags: &RuntimeFlags, document_id: &str) -> Result<()> {
    let Some(embedder) = flags.embedder() else {
        return Ok(());
    };
    let chunks = store.get_chunks_for_document(document_id).await?;
    if chunks.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = chunks
        .iter()
        .map(|c| {
            let d = c.distilled.as_ref();
            let prefix = [d.and_then(|d| d.question.clone()), d.and_then(|d| d.summary.clone())]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n");
            if prefix.is_empty() {
                c.content.clone()
            } else {
                format!("{prefix}\n{}", c.content)
            }
        })
        .collect();
    let vectors = embedder.embed(&texts).await?;
    let items: Vec<(String, Vec<f32>)> = chunks
        .iter()
        .zip(vectors.iter())
        .map(|(c, v)| (c.id.clone(), v.clone()))
        .collect();
    store
        .upsert_embeddings(&items, embedder.model(), &now_iso())
        .await
}

async fn distill_document(
    store: &Arc<dyn Store>,
    document_id: &str,
    llm: crate::llm::provider::DynLlmProvider,
) -> Result<()> {
    let chunks = store.get_chunks_for_document(document_id).await?;
    for chunk in chunks.iter().take(12) {
        if chunk.distilled.is_some() {
            continue;
        }
        let user_content: String =
            chunk.content.chars().take(2000).collect();
        let messages: Vec<crate::llm::provider::ChatMessage<'_>> = vec![
            ("system", DISTILL_SYSTEM),
            ("user", user_content.as_str()),
        ];
        let raw = llm.chat(&messages, 0.0, 200).await?;
        if let Ok(distilled) = serde_json::from_str::<Distilled>(extract_json(&raw)) {
            store
                .replace_chunk_distilled(&chunk.id, Some(distilled))
                .await?;
        }
    }
    Ok(())
}

const DISTILL_SYSTEM: &str = "Extract from the passage: a search question an engineer would type, and a one-sentence summary. Respond ONLY with JSON {\"question\":\"...\",\"summary\":\"...\"}.";

fn extract_json(raw: &str) -> &str {
    match (raw.find('{'), raw.rfind('}')) {
        (Some(s), Some(e)) if e > s => &raw[s..=e],
        _ => raw,
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn build_chunks(record: &DocumentRecord) -> Vec<ChunkInput> {
    match record.kind {
        DocKind::Page => crate::ingest::chunkers::chunk_markdown(
            &record.body,
            crate::ingest::chunkers::ChunkOptions { max_chars: 1200, min_chars: 200 },
        )
        .into_iter()
        .map(|c| ChunkInput {
            ordinal: c.ordinal,
            heading_path: c.heading_path,
            content: c.content,
            distilled: None,
        })
        .collect(),
        DocKind::Doc => {
            crate::ingest::chunkers::chunk_code(
                &record.body,
                crate::ingest::chunkers::detect_language(&record.rel_path),
                crate::ingest::chunkers::ChunkOptions { max_chars: 1200, min_chars: 200 },
            )
            .into_iter()
            .map(|c| ChunkInput {
                ordinal: c.ordinal,
                heading_path: c.heading_path,
                content: c.content,
                distilled: None,
            })
            .collect()
        }
        DocKind::Source => {
            let language = crate::ingest::chunkers::detect_language(&record.rel_path);
            if language.is_none() {
                return Vec::new();
            }
            crate::ingest::chunkers::chunk_code(
                &record.body,
                language,
                crate::ingest::chunkers::ChunkOptions { max_chars: 1200, min_chars: 200 },
            )
            .into_iter()
            .map(|c| ChunkInput {
                ordinal: c.ordinal,
                heading_path: c.heading_path,
                content: c.content,
                distilled: None,
            })
            .collect()
        }
    }
}

fn walk_files(root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let root_path = std::path::Path::new(root);
        visit(root_path, root_path, &mut out);
    fn visit(dir: &std::path::Path, root_root: &std::path::Path, out: &mut Vec<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let full = entry.path();
            let rel = full
                .strip_prefix(root_root)
                .unwrap_or(&full)
                .to_string_lossy()
                .replace('\\', "/");
            if crate::fs::paths::is_ignored_path(&rel) {
                continue;
            }
            if entry.path().is_dir() {
                visit(&full, root_root, out);
            } else if entry.path().is_file() {
                out.push(rel);
            }
        }
    }
    out
}

fn str_field(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(String::from)
}

fn generated_fields(fm: &Value) -> (Option<String>, Option<String>) {
    match fm.get("generated") {
        Some(Value::Object(o)) => (
            o.get("by").and_then(|v| v.as_str()).map(String::from),
            o.get("at").and_then(|v| v.as_str()).map(String::from),
        ),
        _ => (None, None),
    }
}

fn provenance_of(fm: &Value) -> Option<Vec<serde_json::Value>> {
    fm.get("sources")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter(|e| e.is_object()).cloned().collect())
}

fn infer_content_type(ext: &str) -> &'static str {
    match ext {
        ".txt" => "text/plain",
        ".csv" => "text/csv",
        ".json" => "application/json",
        ".yaml" | ".yml" => "text/yaml",
        ".toml" => "text/plain",
        ".html" => "text/html",
        ".pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn count_words(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

fn basename_title(rel_path: &str) -> String {
    std::path::Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().replace(['-', '_'], " "))
        .unwrap_or_default()
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|i| seen.insert(i.clone())).collect()
}
