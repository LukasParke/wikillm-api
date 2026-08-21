//! Markdown and source-code chunkers for indexing.
//!
//! Markdown splits on ATX headings building full ancestor heading paths;
//! small consecutive chunks merge; oversized sections fall back through
//! paragraphs → lines → hard slices. Code chunking scans declaration lines
//! (brace-language keywords, visibility/indented methods, Python def/class),
//! builds an indent-based tree, and emits per-symbol chunks sharing a symbol
//! path like `CheckpointLoader > loadManifest()`.

use std::sync::LazyLock;

use regex::Regex;

const DEFAULT_MAX_CHARS: usize = 1200;
const DEFAULT_MIN_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub ordinal: i64,
    pub heading_path: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOptions {
    pub max_chars: usize,
    pub min_chars: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_chars: DEFAULT_MAX_CHARS,
            min_chars: DEFAULT_MIN_CHARS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawChunk {
    heading_path: Option<String>,
    content: String,
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

static ATX_HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*#*\s*$").unwrap());
static BLANK_LINE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{2,}").unwrap());

// ---------------------------------------------------------------------------
// Generic size-bounded splitting
// ---------------------------------------------------------------------------

/// Split text into pieces of at most `max_chars` characters.
/// Tier 1: paragraphs (blank-line separated). Tier 2: lines. Tier 3: hard slices.
fn split_to_size(text: &str, max_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    split_tier(trimmed, max_chars)
}

fn split_tier(text: &str, max_chars: usize) -> Vec<String> {
    if char_len(text) <= max_chars {
        return vec![text.to_string()];
    }
    let parts: Vec<&str> = BLANK_LINE_RE
        .split(text)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() > 1 {
        let pieces: Vec<String> = parts.iter().flat_map(|p| split_tier(p, max_chars)).collect();
        return pack_pieces(&pieces, "\n\n", max_chars);
    }
    let lines: Vec<&str> = text.split('\n').map(str::trim_end).collect();
    if lines.len() > 1 {
        let pieces: Vec<String> = lines.iter().flat_map(|l| split_line(l, max_chars)).collect();
        return pack_pieces(&pieces, "\n", max_chars);
    }
    split_line(text, max_chars)
}

fn split_line(line: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max_chars {
        return vec![line.to_string()];
    }
    chars
        .chunks(max_chars)
        .map(|slice| slice.iter().collect())
        .collect()
}

fn pack_pieces(pieces: &[String], separator: &str, max_chars: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut buffer = String::new();
    for piece in pieces {
        if buffer.is_empty() {
            buffer = piece.clone();
        } else if char_len(&buffer) + separator.len() + char_len(piece) <= max_chars {
            buffer.push_str(separator);
            buffer.push_str(piece);
        } else {
            out.push(std::mem::take(&mut buffer));
            buffer = piece.clone();
        }
    }
    if !buffer.is_empty() {
        out.push(buffer);
    }
    out
}

// ---------------------------------------------------------------------------
// Markdown chunking
// ---------------------------------------------------------------------------

/// Split on ATX headings (`#`..`######` at line start), tracking the full
/// ancestor chain for the heading path.
fn split_markdown_sections(body: &str) -> Vec<RawChunk> {
    struct Accumulator<'a> {
        path: Option<Vec<String>>,
        lines: Vec<&'a str>,
    }

    fn flush(acc: &Accumulator<'_>, out: &mut Vec<RawChunk>) {
        let content = acc.lines.join("\n");
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return;
        }
        out.push(RawChunk {
            heading_path: acc.path.as_ref().map(|p| p.join(" > ")),
            content: trimmed.to_string(),
        });
    }

    let mut sections: Vec<RawChunk> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut current = Accumulator {
        path: None,
        lines: Vec::new(),
    };

    for line in body.split('\n') {
        let Some(caps) = ATX_HEADING_RE.captures(line) else {
            current.lines.push(line);
            continue;
        };
        flush(&current, &mut sections);
        let level = caps[1].len();
        let title = caps[2].trim().to_string();
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            stack.pop();
        }
        stack.push((level, title));
        current.path = Some(stack.iter().map(|(_, t)| t.clone()).collect());
        current.lines.clear();
    }
    flush(&current, &mut sections);
    sections
}

fn merge_small_chunks(chunks: &[RawChunk], min_chars: usize, max_chars: usize) -> Vec<RawChunk> {
    let mut merged: Vec<RawChunk> = Vec::new();
    for chunk in chunks {
        if let Some(previous) = merged.last_mut() {
            if char_len(&chunk.content) < min_chars
                && previous.heading_path == chunk.heading_path
                && char_len(&previous.content) + char_len(&chunk.content) <= max_chars * 3 / 2
            {
                previous.content.push_str("\n\n");
                previous.content.push_str(&chunk.content);
                continue;
            }
        }
        merged.push(RawChunk {
            heading_path: chunk.heading_path.clone(),
            content: chunk.content.clone(),
        });
    }
    merged
}

/// Chunk a markdown body into size-bounded chunks with heading paths.
pub fn chunk_markdown(body: &str, opts: ChunkOptions) -> Vec<Chunk> {
    let max_chars = opts.max_chars;
    let min_chars = opts.min_chars;
    let sections = split_markdown_sections(body);
    let merged = merge_small_chunks(&sections, min_chars, max_chars);
    let mut raw: Vec<RawChunk> = Vec::new();
    for chunk in merged {
        if char_len(&chunk.content) <= max_chars {
            raw.push(chunk);
        } else {
            for piece in split_to_size(&chunk.content, max_chars) {
                raw.push(RawChunk {
                    heading_path: chunk.heading_path.clone(),
                    content: piece,
                });
            }
        }
    }
    raw.into_iter()
        .enumerate()
        .map(|(index, chunk)| Chunk {
            ordinal: index as i64,
            heading_path: chunk.heading_path,
            content: chunk.content,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Code chunking
// ---------------------------------------------------------------------------

/// Map a filename extension to a language name; `None` for unknown or
/// missing extensions.
pub fn detect_language(filename: &str) -> Option<&'static str> {
    let base = filename.rsplit(['/', '\\']).next()?;
    let dot = base.rfind('.')?;
    if dot == 0 {
        return None;
    }
    Some(match base[dot + 1..].to_lowercase().as_str() {
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "sh" => "shell",
        "sql" => "sql",
        "md" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "html" => "html",
        "css" => "css",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
struct DeclMatch {
    indent: usize,
    name: String,
    callable: bool,
    line_index: usize,
}

#[derive(Debug, Clone)]
struct DeclNode {
    decl: DeclMatch,
    span_end: usize,
    children: Vec<DeclNode>,
}

static KEYWORD_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[ \t]*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?:pub\s+)?(class|function|struct|impl|fn|func|def)\s+([A-Za-z_$][A-Za-z0-9_$]*)",
    )
    .unwrap()
});
static MODIFIER_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[ \t]*(?:public|private|protected)(?:\s+(?:static|readonly|async|override|final|abstract))*\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^>\n]*>)?\(",
    )
    .unwrap()
});
static METHOD_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[ \t]+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^>\n]*>)?\([^;{}]*\)\s*(?::\s*[^{;=]+)?\{")
        .unwrap()
});
static PY_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[ \t]*(?:async\s+)?(def|class)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static COMMENT_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ \t]*(//|/\*|\*|#)").unwrap());

const RESERVED_NAMES: [&str; 8] = [
    "if", "for", "while", "switch", "catch", "return", "new", "function",
];

const CALLABLE_KEYWORDS: [&str; 4] = ["function", "fn", "func", "def"];

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

fn scan_decls(lines: &[&str], language_hint: Option<&str>) -> Vec<DeclMatch> {
    let python_only = language_hint == Some("python");
    let mut decls: Vec<DeclMatch> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if COMMENT_LINE_RE.is_match(line) {
            continue;
        }

        if !python_only {
            if let Some(caps) = KEYWORD_DECL_RE.captures(line) {
                let keyword = &caps[1];
                decls.push(DeclMatch {
                    indent: indent_of(line),
                    name: caps[2].to_string(),
                    callable: CALLABLE_KEYWORDS.contains(&keyword),
                    line_index: i,
                });
                continue;
            }
        }

        if let Some(caps) = PY_DECL_RE.captures(line) {
            let keyword = &caps[1];
            decls.push(DeclMatch {
                indent: indent_of(line),
                name: caps[2].to_string(),
                callable: keyword == "def",
                line_index: i,
            });
            continue;
        }
        if python_only {
            continue;
        }

        if let Some(caps) = MODIFIER_DECL_RE.captures(line) {
            decls.push(DeclMatch {
                indent: indent_of(line),
                name: caps[1].to_string(),
                callable: true,
                line_index: i,
            });
            continue;
        }

        if let Some(caps) = METHOD_DECL_RE.captures(line) {
            let name = &caps[1];
            if !line.contains("=>") && !RESERVED_NAMES.contains(&name) {
                decls.push(DeclMatch {
                    indent: indent_of(line),
                    name: name.to_string(),
                    callable: true,
                    line_index: i,
                });
            }
        }
    }
    decls
}

fn build_tree(decls: &[DeclMatch], total_lines: usize) -> Vec<DeclNode> {
    let mut roots: Vec<DeclNode> = Vec::new();
    let mut stack: Vec<DeclNode> = Vec::new();

    fn pop_attach(stack: &mut Vec<DeclNode>, roots: &mut Vec<DeclNode>) {
        if let Some(popped) = stack.pop() {
            match stack.last_mut() {
                Some(parent) => parent.children.push(popped),
                None => roots.push(popped),
            }
        }
    }

    for (i, decl) in decls.iter().enumerate() {
        let mut span_end = total_lines;
        for later in &decls[i + 1..] {
            if later.indent <= decl.indent {
                span_end = later.line_index;
                break;
            }
        }
        let node = DeclNode {
            decl: decl.clone(),
            span_end,
            children: Vec::new(),
        };
        while matches!(stack.last(), Some(top) if top.decl.indent >= node.decl.indent) {
            pop_attach(&mut stack, &mut roots);
        }
        stack.push(node);
    }
    while !stack.is_empty() {
        pop_attach(&mut stack, &mut roots);
    }
    roots
}

fn display_name(decl: &DeclMatch) -> String {
    if decl.callable {
        format!("{}()", decl.name)
    } else {
        decl.name.clone()
    }
}

fn push_chunk(
    content: &str,
    heading_path: Option<&str>,
    out: &mut Vec<RawChunk>,
    max_chars: usize,
) {
    if char_len(content) <= max_chars {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            out.push(RawChunk {
                heading_path: heading_path.map(str::to_string),
                content: trimmed.to_string(),
            });
        }
        return;
    }
    for piece in split_to_size(content, max_chars) {
        out.push(RawChunk {
            heading_path: heading_path.map(str::to_string),
            content: piece,
        });
    }
}

fn emit_node(
    lines: &[&str],
    node: &DeclNode,
    ancestors: &[String],
    out: &mut Vec<RawChunk>,
    max_chars: usize,
) {
    let mut path: Vec<String> = ancestors.to_vec();
    path.push(display_name(&node.decl));
    let heading_path = path.join(" > ");

    if !node.children.is_empty() {
        let mut cursor = node.decl.line_index;
        for child in &node.children {
            let head = lines[cursor..child.decl.line_index].join("\n");
            push_chunk(&head, Some(&heading_path), out, max_chars);
            emit_node(lines, child, &path, out, max_chars);
            cursor = child.span_end;
        }
        let tail = lines[cursor..node.span_end].join("\n");
        push_chunk(&tail, Some(&heading_path), out, max_chars);
        return;
    }

    let body = lines[node.decl.line_index..node.span_end].join("\n");
    push_chunk(&body, Some(&heading_path), out, max_chars);
}

/// Chunk source code: declaration boundaries when symbols are detected
/// (coarse → fine via indent nesting), otherwise blank-line groups → lines →
/// hard slices. `language_hint` restricts Python to def/class patterns.
pub fn chunk_code(content: &str, language_hint: Option<&str>, opts: ChunkOptions) -> Vec<Chunk> {
    let max_chars = opts.max_chars;
    let lines: Vec<&str> = content.split('\n').collect();
    let decls = scan_decls(&lines, language_hint);
    let mut raw: Vec<RawChunk> = Vec::new();

    if decls.is_empty() {
        push_chunk(content, None, &mut raw, max_chars);
    } else {
        let roots = build_tree(&decls, lines.len());
        let first_line = decls[0].line_index;
        let preamble = lines[..first_line].join("\n");
        push_chunk(&preamble, None, &mut raw, max_chars);
        for root in &roots {
            emit_node(&lines, root, &[], &mut raw, max_chars);
        }
    }

    raw.into_iter()
        .enumerate()
        .map(|(index, chunk)| Chunk {
            ordinal: index as i64,
            heading_path: chunk.heading_path,
            content: chunk.content,
        })
        .collect()
}
