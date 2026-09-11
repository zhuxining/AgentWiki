//! Read-only access to the Markdown wiki: path safety, frontmatter parsing,
//! heading-aware chunking, and full-archive scanning.
//!
//! This module never writes Markdown — the native tools of the agent own all
//! document mutations. It only reads.

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};
use text_splitter::{ChunkConfig, TextSplitter};

use crate::error::{AgentWikiError, Result};
use crate::model::{Document, Fingerprint, Frontmatter, PathScope, Slice};

/// Unified characters per slice; oversized sections are split on paragraphs.
const MAX_CHUNK_CHARS: usize = 1_200;
/// Overlap carried into the next fragment when splitting an oversized section.
const OVERLAP_CHARS: usize = 150;

/// Default `AGENTWIKI.md` seeded into a new wiki root.
///
/// This is the *minimal* starter template (name / purpose / required_fields /
/// default_type). `docs/AGENTWIKI.md` is the complete reference example with
/// `sections` and `tag_aliases`; the two need not match verbatim, and existing
/// rule files are never overwritten. Required fields are entirely driven by the
/// rule file — the system defines none by default beyond this modest starter set.
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
    // A missing leaf can still have an existing parent symlink outside root.
    for ancestor in joined
        .ancestors()
        .skip(1)
        .take_while(|ancestor| root.exists() && ancestor.starts_with(root))
    {
        if let Ok(actual) = ancestor.canonicalize() {
            let canonical_root = root.canonicalize().map_err(|source| AgentWikiError::Io {
                path: root.to_path_buf(),
                source,
            })?;
            if !actual.starts_with(canonical_root) {
                return Err(AgentWikiError::PathOutsideRoot(p.to_path_buf()));
            }
            break;
        }
    }
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
///
/// `title` is reserved for the future semantic leg (ARCHITECTURE §2.4);
/// current hashing covers section + content only.
pub fn chunk_document(title: &str, body: &str, path: &PathScope) -> Vec<Slice> {
    // Reserved for the semantic leg (see doc comment above).
    let _ = title;
    let mut sections: Vec<(String, String)> = Vec::new();
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut heading = None;
    let mut text = String::new();
    let mut start = 0;
    let breadcrumb = |stack: &[(usize, String)]| {
        stack
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    };
    for (event, range) in Parser::new(body).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let content = body[start..range.start].trim();
                if !content.is_empty() {
                    sections.push((breadcrumb(&headings), content.to_owned()));
                }
                heading = Some(level as usize);
                text.clear();
            }
            Event::Text(value) | Event::Code(value) if heading.is_some() => text.push_str(&value),
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = heading.take() {
                    while headings.last().is_some_and(|(old, _)| *old >= level) {
                        headings.pop();
                    }
                    headings.push((level, text.clone()));
                }
                start = range.end;
            }
            _ => {}
        }
    }
    let content = body[start..].trim();
    if !content.is_empty() {
        sections.push((breadcrumb(&headings), content.to_owned()));
    }

    // A document with no non-empty sections must still be addressable.
    let mut slices = Vec::new();
    let mut ordinal = 0u32;
    let has_content = sections.iter().any(|(_, c)| !c.is_empty());
    if sections.is_empty() || (!has_content && body.trim().is_empty()) {
        slices.push(empty_slice(path, ordinal, breadcrumb(&headings)));
        return slices;
    }

    for (section, content) in sections {
        for fragment in split_oversized(&content) {
            let source = format!("{section}\n{fragment}").trim().to_string();
            let chunk_id = hex::encode(Sha256::digest(
                format!("{}\u{0}{}\u{0}{}", path.0, ordinal, source).as_bytes(),
            ));
            let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
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
    let config = ChunkConfig::new(MAX_CHUNK_CHARS)
        .with_overlap(OVERLAP_CHARS)
        .expect("overlap is smaller than chunk capacity")
        .with_trim(true);
    TextSplitter::new(config)
        .chunks(content)
        .map(ToOwned::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Document reading & archive scanning
// ---------------------------------------------------------------------------

/// Read one Markdown file at `root/<path>` into a [`Document`].
///
/// Line endings are normalised. The fingerprint is computed from the raw bytes.
pub fn read_document(root: &Utf8Path, path: &PathScope) -> Result<Document> {
    read_document_with_body(root, path).map(|(document, _)| document)
}

pub fn read_document_with_body(root: &Utf8Path, path: &PathScope) -> Result<(Document, String)> {
    scope_path(root, &path.0)?;
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

    let raw = std::str::from_utf8(&bytes).map_err(|error| AgentWikiError::Parse {
        path: path.0.clone(),
        message: error.to_string(),
    })?;
    let (frontmatter, body) = parse_frontmatter(raw);
    if raw.trim_start_matches('\u{feff}').starts_with("---\n") && body == raw {
        return Err(AgentWikiError::Parse {
            path: path.0.clone(),
            message: "malformed YAML frontmatter".into(),
        });
    }
    let title = frontmatter
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();

    Ok((
        Document {
            path: path.clone(),
            title,
            frontmatter,
            fingerprint: Fingerprint {
                content_hash: hex::encode(Sha256::digest(&bytes)),
                mtime_ns,
                size: bytes.len() as u64,
            },
        },
        body,
    ))
}

/// Read one Markdown file and return its body with the frontmatter stripped.
///
/// Used by `sync` to chunk a document and extract edges. The leading YAML
/// frontmatter, when present, is removed; a missing frontmatter returns the
/// raw text as the body. Errors on read/fingerprint failures.
pub fn read_body(root: &Utf8Path, path: &PathScope) -> Result<String> {
    read_document_with_body(root, path).map(|(_, body)| body)
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

    #[test]
    fn frontmatter_plain_document() {
        let (fm, body) = parse_frontmatter("# Only body\n");
        assert!(fm.is_empty());
        assert!(body.starts_with("# Only body"));
    }

    #[test]
    fn frontmatter_bom_prefix() {
        let raw = "\u{feff}---\ntitle: T\n---\nbody\n";
        let (fm, body) = parse_frontmatter(raw);
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("T"));
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn frontmatter_crlf_line_endings() {
        let raw = "---\r\ntitle: T\r\ntags: [a, b]\r\n---\r\nbody\r\n";
        let (fm, body) = parse_frontmatter(raw);
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("T"));
        let tags: Vec<&str> = fm
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(tags, vec!["a", "b"]);
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn frontmatter_unterminated_header_is_lossy() {
        // Open fence without a closing one: empty map, raw text as body so the
        // file stays readable and validate can flag `markdown.parse`.
        let raw = "---\ntitle: T\n";
        let (fm, body) = parse_frontmatter(raw);
        assert!(fm.is_empty());
        assert_eq!(body, raw);
    }

    #[test]
    fn frontmatter_scalar_header_is_lossy() {
        // A header whose YAML is not a map (scalar) must not panic and yields
        // an empty map with the raw text as body.
        let raw = "---\n42\n---\nbody\n";
        let (fm, body) = parse_frontmatter(raw);
        assert!(fm.is_empty());
        assert!(body.contains("body"));
    }

    #[test]
    fn frontmatter_nested_values() {
        let raw =
            "---\nrel:\n  - type: depends_on\n    target: x.md\ncount: 2\nok: true\n---\nbody\n";
        let (fm, _) = parse_frontmatter(raw);
        let rel = fm.get("rel").and_then(|v| v.as_array()).unwrap();
        assert_eq!(rel[0].get("target").and_then(|v| v.as_str()), Some("x.md"));
        // Numbers travel as f64 through `to_json`; assert on the float view.
        assert_eq!(fm.get("count").and_then(|v| v.as_f64()), Some(2.0));
        assert_eq!(fm.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn snapshot_excludes_hidden_dirs_and_agentwiki() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".obsidian/plugins")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("AGENTWIKI.md"), "---\n---").unwrap();
        std::fs::write(root.join("a.md"), "# a").unwrap();
        std::fs::write(root.join("notes/b.md"), "# b").unwrap();
        std::fs::write(root.join(".obsidian/plugins/c.md"), "# c").unwrap();
        let paths = snapshot(&root).unwrap();
        let strs: Vec<String> = paths.iter().map(|p| p.0.to_string()).collect();
        assert_eq!(strs, vec!["a.md", "notes/b.md"]);
    }
}
