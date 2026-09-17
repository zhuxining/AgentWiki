//! Read-only access to the Markdown wiki: path safety, frontmatter parsing,
//! heading-aware chunking, and full-archive scanning.
//!
//! This module never writes Markdown — the native tools of the agent own all
//! document mutations. It only reads.

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
// Document reading & archive scanning
// ---------------------------------------------------------------------------

pub fn read_document_with_body(root: &Utf8Path, path: &PathScope) -> Result<(Document, String)> {
    super::path::scope_path(root, &path.0)?;
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
    Ok((
        Document {
            path: path.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
