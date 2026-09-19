//! Read-only access to the Markdown wiki: path safety, frontmatter parsing,
//! heading-aware chunking, and full-archive scanning.
//!
//! This module never writes Markdown — the native tools of the agent own all
//! document mutations. It only reads.

use std::io::Read;

use camino::Utf8Path;
use sha2::{Digest, Sha256};

use crate::document::types::{Document, Fingerprint, Frontmatter, PathScope};
use crate::error::{AgentWikiError, Result};

/// Default `AGENTWIKI.md` seeded into a new wiki root.
///
/// This is the *minimal* starter template (`default_type` and an empty
/// `required_fields` extension point). Built-in required fields are enforced by
/// governance. `docs/AGENTWIKI.md` is the complete reference example with
/// `sections` and `tag_aliases`; the two need not match verbatim, and existing
/// rule files are never overwritten. Required fields are entirely driven by the
/// rule file — the system defines none by default beyond this modest starter set.
pub const DEFAULT_AGENTWIKI: &str = "---\n\
default_type: note\n\
required_fields: []\n\
---\n\
\n\
# Wiki 使用指南\n\
\n\
检索命中后请使用原生文件工具读取关键文档原文，再形成结论。\n\
新建或首次修改陌生目录前，先调用 `get_wiki_rules` 获取适用规则。\n\
使用原生工具修改 Markdown 后，调用 `validate_wiki` 检查格式、Frontmatter、目录约束和内部链接。\n\
";

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

/// Best-effort conversion of a YAML value into a JSON value.
fn to_json(v: &serde_yaml::Value) -> Option<serde_json::Value> {
    match v {
        serde_yaml::Value::Null => Some(serde_json::Value::Null),
        serde_yaml::Value::Bool(b) => Some(serde_json::Value::Bool(*b)),
        serde_yaml::Value::Number(n) => n
            .as_i64()
            .map(serde_json::Value::from)
            .or_else(|| n.as_u64().map(serde_json::Value::from))
            .or_else(|| n.as_f64().map(serde_json::Value::from)),
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
// Document reading & archive scanning
// ---------------------------------------------------------------------------

const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// A stable document read, parsed only when its content hash has changed.
pub enum DocumentRead {
    Unchanged(Fingerprint),
    Changed(Document, String),
}

pub fn read_document_with_body(root: &Utf8Path, path: &PathScope) -> Result<(Document, String)> {
    match read_document_if_changed(root, path, None)? {
        DocumentRead::Changed(document, body) => Ok((document, body)),
        DocumentRead::Unchanged(_) => Err(AgentWikiError::Parse {
            path: path.0.clone(),
            message: "document read unexpectedly skipped parsing without a previous hash".into(),
        }),
    }
}

/// Read bounded bytes once, skipping parsing when `previous_hash` matches.
/// Files that change size or modification time during the read must be retried.
pub fn read_document_if_changed(
    root: &Utf8Path,
    path: &PathScope,
    previous_hash: Option<&str>,
) -> Result<DocumentRead> {
    super::path::scope_path(root, &path.0)?;
    let full = root.join(path.0.as_path());
    let meta = std::fs::metadata(&full).map_err(|e| AgentWikiError::Io {
        path: full.clone(),
        source: e,
    })?;
    let size_error = || AgentWikiError::Parse {
        path: path.0.clone(),
        message: format!("document exceeds size limit of {MAX_DOCUMENT_BYTES} bytes"),
    };
    if meta.len() > MAX_DOCUMENT_BYTES {
        return Err(size_error());
    }
    let modified = meta.modified().map_err(|source| AgentWikiError::Io {
        path: full.clone(),
        source,
    })?;
    let file = std::fs::File::open(&full).map_err(|source| AgentWikiError::Io {
        path: full.clone(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| AgentWikiError::Io {
            path: full.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(size_error());
    }
    let after = std::fs::metadata(&full).map_err(|source| AgentWikiError::Io {
        path: full.clone(),
        source,
    })?;
    let modified_after = after.modified().map_err(|source| AgentWikiError::Io {
        path: full.clone(),
        source,
    })?;
    if meta.len() != after.len() || modified != modified_after || bytes.len() as u64 != after.len()
    {
        return Err(AgentWikiError::Parse {
            path: path.0.clone(),
            message: "document changed during read; retry required".into(),
        });
    }
    let fingerprint = Fingerprint {
        content_hash: hex::encode(Sha256::digest(&bytes)),
        mtime_ns: modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as i64)
            .unwrap_or(0),
        size: bytes.len() as u64,
    };
    if previous_hash == Some(fingerprint.content_hash.as_str()) {
        return Ok(DocumentRead::Unchanged(fingerprint));
    }

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
    Ok(DocumentRead::Changed(
        Document {
            path: path.clone(),
            frontmatter,
            fingerprint,
        },
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_hash_skips_even_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let path = PathScope("invalid.md".into());
        let raw = "---\ntitle: [unterminated\n---\nbody\n";
        std::fs::write(root.join(&path.0), raw).unwrap();
        let hash = hex::encode(Sha256::digest(raw.as_bytes()));

        match read_document_if_changed(root, &path, Some(&hash)).unwrap() {
            DocumentRead::Unchanged(fingerprint) => {
                assert_eq!(fingerprint.content_hash, hash);
                assert_eq!(fingerprint.size, raw.len() as u64);
            }
            DocumentRead::Changed(_, _) => panic!("matching hash must skip parsing"),
        }
        assert!(matches!(
            read_document_if_changed(root, &path, Some("different")),
            Err(AgentWikiError::Parse { .. })
        ));
        assert!(matches!(
            read_document_with_body(root, &path),
            Err(AgentWikiError::Parse { .. })
        ));
    }

    #[test]
    fn changed_hash_returns_parsed_document_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let path = PathScope("note.md".into());
        let raw = "---\ncount: 2\n---\nbody\n";
        std::fs::write(root.join(&path.0), raw).unwrap();

        match read_document_if_changed(root, &path, Some("previous")).unwrap() {
            DocumentRead::Changed(document, body) => {
                assert_eq!(document.frontmatter["count"], serde_json::json!(2));
                assert_eq!(body.trim(), "body");
                assert_eq!(
                    document.fingerprint.content_hash,
                    hex::encode(Sha256::digest(raw.as_bytes()))
                );
                assert_eq!(document.fingerprint.size, raw.len() as u64);
            }
            DocumentRead::Unchanged(_) => panic!("different hash must parse document"),
        }
        let (document, body) = read_document_with_body(root, &path).unwrap();
        assert_eq!(document.frontmatter["count"], serde_json::json!(2));
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn oversized_sparse_document_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let path = PathScope("large.md".into());
        let file = std::fs::File::create(root.join(&path.0)).unwrap();
        file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();

        assert!(matches!(
            read_document_if_changed(root, &path, None),
            Err(AgentWikiError::Parse { path: failed, message })
                if failed == path.0 && message.contains("size limit")
        ));
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
    fn frontmatter_preserves_number_types_and_precision() {
        let (fm, _) = parse_frontmatter(
            "---\ncount: 2\nnegative: -2\nlarge: 18446744073709551615\nfloat: 2.5\n---\n",
        );
        assert_eq!(fm.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(fm.get("negative"), Some(&serde_json::json!(-2)));
        assert_eq!(fm.get("large"), Some(&serde_json::json!(u64::MAX)));
        assert_eq!(fm.get("float"), Some(&serde_json::json!(2.5)));
        assert_eq!(serde_json::to_string(&fm["count"]).unwrap(), "2");
    }

    #[test]
    fn frontmatter_preserves_null_in_fields_and_collections() {
        let (fm, _) = parse_frontmatter(
            "---\nempty: null\nnested: {empty: null}\nitems: [first, null, last]\n---\n",
        );
        assert_eq!(fm.get("empty"), Some(&serde_json::Value::Null));
        assert_eq!(fm.get("nested"), Some(&serde_json::json!({"empty": null})));
        assert_eq!(
            fm.get("items"),
            Some(&serde_json::json!(["first", null, "last"]))
        );
    }

    #[test]
    fn frontmatter_nested_values() {
        let raw =
            "---\nrel:\n  - type: depends_on\n    target: x.md\ncount: 2\nok: true\n---\nbody\n";
        let (fm, _) = parse_frontmatter(raw);
        let rel = fm.get("rel").and_then(|v| v.as_array()).unwrap();
        assert_eq!(rel[0].get("target").and_then(|v| v.as_str()), Some("x.md"));
        assert_eq!(fm.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(fm.get("ok").and_then(|v| v.as_bool()), Some(true));
    }
}
