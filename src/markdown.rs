//! Read-only access to the Markdown wiki: path safety, frontmatter parsing,
//! heading-aware chunking, and full-archive scanning.
//!
//! This module never writes Markdown — the native tools of the agent own all
//! document mutations. It only reads.

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};

use crate::error::{AgentWikiError, Result};
use crate::model::{Document, Fingerprint, Frontmatter, PathScope, Slice};

/// Unified characters per slice; oversized sections are split on paragraphs.
const MAX_CHUNK_CHARS: usize = 1_200;
/// Overlap carried into the next fragment when splitting an oversized section.
const OVERLAP_CHARS: usize = 150;

/// Default `AGENTWIKI.md` seeded into a new wiki root.
///
/// Mirrors the reference template in `docs/AGENTWIKI.md`. Required fields are
/// entirely driven by this file — the system defines none by default beyond a
/// modest starter set.
pub const DEFAULT_AGENTWIKI: &str = "---\n\
name: AgentWiki 示例知识库\n\
purpose: 为 Agent 提供可检索的团队知识、操作指南和项目资料\n\
\n\
required_fields:\n\
  - title\n\
  - type\n\
  - tags\n\
  - created_at\n\
  - updated_at\n\
  - owner\n\
default_type: note\n\
---\n\
\n\
# Wiki 使用指南\n\
\n\
检索命中后请使用原生文件工具读取关键文档原文，再形成结论。\n\
新建或首次修改陌生目录前，先调用 `get_wiki_rules` 获取适用规则。\n\
使用原生工具修改 Markdown 后，调用 `validate_wiki` 检查格式、Frontmatter、目录约束和内部链接。\n\
";

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// Validate and convert a user/scan-provided path into a [`PathScope`] that is
/// guaranteed to live under `root`.
///
/// Rejects absolute escapes and `..` traversal. `root` must itself be absolute.
pub fn scope_path(root: &Utf8Path, p: &Utf8Path) -> Result<PathScope> {
    if !root.is_absolute() {
        return Err(AgentWikiError::Config(
            "wiki root must be an absolute path".into(),
        ));
    }
    let joined = root.join(p);
    // Reject any `..` component or symlink escape by comparing the normalized
    // lexical path. `components()` collapses `.` and honours leading `..` only
    // if it would escape — we forbid that outright.
    if p.components()
        .any(|c| matches!(c, camino::Utf8Component::ParentDir))
    {
        return Err(AgentWikiError::PathOutsideRoot(p.to_path_buf()));
    }
    if p.is_absolute() {
        return Err(AgentWikiError::PathOutsideRoot(p.to_path_buf()));
    }
    // Guard against symlinks escaping the root: compare canonicalized joined
    // path against canonicalized root. If it fails we conservatively reject.
    match (root.canonicalize(), joined.canonicalize()) {
        (Ok(rc), Ok(jc)) if jc.starts_with(&rc) => {}
        (Ok(_), Ok(_)) => return Err(AgentWikiError::PathOutsideRoot(p.to_path_buf())),
        // Canonicalize can fail on a not-yet-created target; lexical check stands.
        _ => {}
    }
    let _ = joined;
    Ok(PathScope(p.to_path_buf()))
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// Split raw document text into optional YAML frontmatter and body.
///
/// Returns `(frontmatter_map, body)`. A missing or malformed (non-map)
/// frontmatter yields an empty map and the body starts at the first `---`.
pub fn parse_frontmatter(raw: &str) -> (Frontmatter, String) {
    let raw_trim = raw.trim_start_matches('\u{feff}');
    let Some(body) = raw_trim.strip_prefix("---") else {
        return (Frontmatter::new(), raw.to_string());
    };
    let Some(sep) = body.find("\n---") else {
        return (Frontmatter::new(), raw.to_string());
    };
    let yaml = &body[..sep];
    let rest = &body[sep + 4..];
    match serde_yaml::from_str::<serde_yaml::Mapping>(yaml) {
        Ok(map) => {
            let mut out = Frontmatter::new();
            for (k, v) in map {
                if let (serde_yaml::Value::String(k), Some(ok)) = (k, to_json(&v)) {
                    out.insert(k, ok);
                }
            }
            (out, rest.to_string())
        }
        Err(_) => (Frontmatter::new(), raw.to_string()),
    }
}

/// Best-effort conversion of a YAML scalar into a JSON value.
fn to_json(v: &serde_yaml::Value) -> Option<serde_json::Value> {
    match v {
        serde_yaml::Value::Null => None,
        serde_yaml::Value::Bool(b) => Some(serde_json::Value::Bool(*b)),
        serde_yaml::Value::Number(n) => n.as_f64().map(serde_json::Value::from).or_else(|| {
            n.as_u64()
                .map(serde_json::Value::from)
                .or_else(|| n.as_i64().map(serde_json::Value::from))
        }),
        serde_yaml::Value::String(s) => Some(serde_json::Value::String(s.clone())),
        serde_yaml::Value::Sequence(seq) => {
            let items: Vec<serde_json::Value> = seq.iter().filter_map(to_json).collect();
            Some(serde_json::Value::Array(items))
        }
        serde_yaml::Value::Mapping(map) => {
            let mut out = Frontmatter::new();
            for (k, v) in map {
                if let (serde_yaml::Value::String(k), Some(v)) = (k, to_json(v)) {
                    out.insert(k.clone(), v);
                }
            }
            Some(serde_json::Value::Object(out))
        }
        serde_yaml::Value::Tagged(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Heading-aware chunking
// ---------------------------------------------------------------------------

/// Split a document body into heading-aware slices.
///
/// Mirrors the behavioural contract of the original design:
/// - headings stack into a section breadcrumb (`" / "`-joined);
/// - empty sections (a heading followed immediately by a deeper heading) are
///   skipped;
/// - a document with no content still yields one (empty) slice so it stays
///   addressable;
/// - oversized sections are split by paragraph with a small overlap.
pub fn chunk_document(title: &str, body: &str, path: &PathScope) -> Vec<Slice> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut headings: Vec<String> = Vec::new();
    let mut content_lines: Vec<&str> = Vec::new();

    let flush =
        |sections: &mut Vec<(String, String)>, headings: &[String], content: &mut Vec<&str>| {
            let text = content.join("\n").trim().to_string();
            content.clear();
            if text.is_empty() {
                return;
            }
            sections.push((headings.join(" / "), text));
        };

    for line in body.split_terminator('\n') {
        if let Some(h) = heading_of(line) {
            flush(&mut sections, &headings, &mut content_lines);
            // Replace everything from this level down: `level-1..` truncation.
            let level = h.0;
            let next_headings = headings[..level - 1]
                .iter()
                .cloned()
                .chain(std::iter::once(h.1))
                .collect::<Vec<_>>();
            headings = next_headings;
        } else {
            content_lines.push(line);
        }
    }
    flush(&mut sections, &headings, &mut content_lines);

    // A document with no non-empty sections must still be addressable.
    let mut slices = Vec::new();
    let mut ordinal = 0u32;
    let has_content = sections.iter().any(|(_, c)| !c.is_empty());
    if sections.is_empty() || (!has_content && body.trim().is_empty()) {
        slices.push(empty_slice(
            path,
            ordinal,
            sections.first().map(|s| s.0.clone()).unwrap_or_default(),
        ));
        return slices;
    }

    let tags: Vec<String> = Vec::new();
    // `tags` is filled by the caller via `Document.frontmatter`; kept simple here.
    let _ = tags;

    for (section, content) in sections {
        for fragment in split_oversized(&content) {
            let source = format!("{section}\n{fragment}").trim().to_string();
            let chunk_id = hex::encode(Sha256::digest(
                format!("{}\u{0}{}\u{0}{}", path.0, ordinal, source).as_bytes(),
            ));
            let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
            let _ = (title, &tags);
            // embedding_hash includes title/tags/source; computed where the
            // semantic leg needs it (see tantivy_svc::embedding_hash).
            slices.push(Slice {
                path: path.clone(),
                chunk_id,
                ordinal,
                section: section.clone(),
                content: fragment,
                source_hash,
            });
            ordinal += 1;
        }
    }
    slices
}

/// Extract `(level, heading_text)` for an ATX heading line.
fn heading_of(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes > 6 {
        return None;
    }
    let rest = trimmed[hashes..].trim();
    if rest.is_empty() {
        return None;
    }
    Some((hashes, rest.trim_end().trim().to_string()))
}

fn empty_slice(path: &PathScope, ordinal: u32, section: String) -> Slice {
    let source = section.clone();
    Slice {
        path: path.clone(),
        chunk_id: hex::encode(Sha256::digest(
            format!("{}\u{0}{}\u{0}{}", path.0, ordinal, source).as_bytes(),
        )),
        ordinal,
        section,
        content: String::new(),
        source_hash: hex::encode(Sha256::digest(source.as_bytes())),
    }
}

/// Split a section into fragments never exceeding `MAX_CHUNK_CHARS`, carrying a
/// small overlap between fragments and hard-wrapping a single oversized
/// paragraph via a sliding window.
fn split_oversized(content: &str) -> Vec<String> {
    if content.chars().count() <= MAX_CHUNK_CHARS {
        return vec![content.to_string()];
    }
    let paragraphs: Vec<&str> = content
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for paragraph in paragraphs {
        if paragraph.chars().count() > MAX_CHUNK_CHARS {
            if !current.is_empty() {
                out.push(current.clone());
                current.clear();
            }
            out.extend(window(paragraph));
            continue;
        }
        let candidate = if current.is_empty() {
            paragraph.to_string()
        } else {
            format!("{current}\n\n{paragraph}")
        };
        if candidate.chars().count() <= MAX_CHUNK_CHARS {
            current = candidate;
        } else {
            if !current.is_empty() {
                out.push(current.clone());
            }
            let overlap = overlap_for(&current, paragraph);
            current = if overlap.is_empty() {
                paragraph.to_string()
            } else {
                format!("{overlap}\n\n{paragraph}")
            };
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Overlap prefix budgeted so the next fragment stays within the limit.
fn overlap_for(previous: &str, upcoming: &str) -> String {
    let budget = MAX_CHUNK_CHARS as isize - upcoming.chars().count() as isize - 2;
    if budget <= 0 {
        return String::new();
    }
    let take = usize::min(OVERLAP_CHARS, budget as usize);
    previous
        .chars()
        .rev()
        .take(take)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .trim_start()
        .to_string()
}

/// Hard-wrap one oversized paragraph as a sliding window with overlap.
fn window(text: &str) -> Vec<String> {
    let step = MAX_CHUNK_CHARS - OVERLAP_CHARS;
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = usize::min(start + MAX_CHUNK_CHARS, chars.len());
        let window_s: String = chars[start..end].iter().collect();
        if !out.is_empty() && window_s.chars().count() <= OVERLAP_CHARS {
            break;
        }
        out.push(window_s);
        if end >= chars.len() {
            break;
        }
        start += step;
    }
    out
}

// ---------------------------------------------------------------------------
// Document reading & archive scanning
// ---------------------------------------------------------------------------

/// Read one Markdown file at `root/<path>` into a [`Document`].
///
/// Line endings are normalised. The fingerprint is computed from the raw bytes.
pub fn read_document(root: &Utf8Path, path: &PathScope) -> Result<Document> {
    let full = root.join(path.0.as_path());
    let meta = std::fs::metadata(&full).map_err(|e| AgentWikiError::Io {
        path: full.clone(),
        source: e,
    })?;
    let bytes = std::fs::read(&full).map_err(|e| AgentWikiError::Io {
        path: full.clone(),
        source: e,
    })?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    let raw = String::from_utf8_lossy(&bytes);
    let (frontmatter, _body) = parse_frontmatter(&raw);
    let title = frontmatter
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();

    Ok(Document {
        path: path.clone(),
        title,
        frontmatter,
        fingerprint: Fingerprint {
            content_hash: hex::encode(Sha256::digest(&bytes)),
            mtime_ns,
            size: bytes.len() as u64,
        },
    })
}

/// Read one Markdown file and return its body with the frontmatter stripped.
///
/// Used by `sync` to chunk a document and extract edges. The leading YAML
/// frontmatter, when present, is removed; a missing frontmatter returns the
/// raw text as the body. Errors on read/fingerprint failures.
pub fn read_body(root: &Utf8Path, path: &PathScope) -> Result<String> {
    let full = root.join(path.0.as_path());
    let bytes = std::fs::read(&full).map_err(|e| AgentWikiError::Io {
        path: full.clone(),
        source: e,
    })?;
    let raw = String::from_utf8_lossy(&bytes);
    let (_, body) = parse_frontmatter(&raw);
    Ok(body)
}

/// Recursively collect every `*.md` path (except the reserved `AGENTWIKI.md`
/// and hidden directories) relative to `root`.
pub fn snapshot(root: &Utf8Path) -> Result<Vec<PathScope>> {
    let mut out = Vec::new();
    collect_md(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_md(root: &Utf8Path, dir: &Utf8Path, out: &mut Vec<PathScope>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| AgentWikiError::Io {
        path: dir.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| AgentWikiError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let p = entry.path();
        // Extend the root-relative path safely.
        let rel = p.strip_prefix(root).unwrap_or(p.as_path());
        let Some(rel_utf) = Utf8PathBuf::from_path_buf(rel.to_path_buf()).ok() else {
            continue;
        };
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if rel_utf
                .as_str()
                .split(std::path::MAIN_SEPARATOR)
                .any(|c| c.starts_with('.') && c != ".")
            {
                continue; // hidden directory — skip
            }
            collect_md(root, Utf8Path::from_path(&p).unwrap(), out)?;
        } else if p
            .extension()
            .map(|e| e == std::ffi::OsStr::new("md"))
            .unwrap_or(false)
            && rel_utf.as_str() != "AGENTWIKI.md"
        {
            out.push(PathScope(rel_utf));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(root: &camino::Utf8Path, p: &str) -> PathScope {
        scope_path(root, &Utf8PathBuf::from(p)).unwrap()
    }

    #[test]
    fn scope_rejects_parent_escape() {
        let root = Utf8PathBuf::from("/wiki");
        assert!(scope_path(&root, &Utf8PathBuf::from("../etc/passwd")).is_err());
        assert!(scope_path(&root, &Utf8PathBuf::from("a/../../../x.md")).is_err());
    }

    #[test]
    fn chunk_by_headings() {
        let body = "# Title\n\nintro\n## Sub\n\ndetail\n\nmore\n";
        let slices = chunk_document("T", body, &scope(&Utf8PathBuf::from("/w"), "a.md"));
        // "# Title" empty section skipped; "Sub" holds detail+more.
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0].section, "Title");
        assert!(slices[0].content.contains("intro"));
        assert_eq!(slices[1].section, "Title / Sub");
    }
}
