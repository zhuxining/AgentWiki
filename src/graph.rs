//! One-hop document-graph extraction.
//!
//! The graph is a *derived* projection of explicitly declared relations in the
//! Markdown (wikilinks `[[doc]]`, relative `.md` links, and a `relations`
//! frontmatter block). It is kept deliberately small: entities are not
//! auto-extracted.

use crate::model::{Edge, EdgeStatus, PathScope};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

/// Extract all edges declared in one document's frontmatter and body.
///
/// `body` is the Markdown body (frontmatter already stripped). Returns edges
/// plus any non-fatal warnings (e.g. an illegal `relations` shape).
pub fn extract_edges(
    from: &PathScope,
    frontmatter: &crate::model::Frontmatter,
    body: &str,
) -> (Vec<Edge>, Vec<String>) {
    let mut edges = Vec::new();
    let mut warnings = Vec::new();

    // 1) `[[wikilink]]` spans.
    for (target, section) in extract_wikilinks(body) {
        edges.push(edge(from, &target, "[[link]]", section));
    }

    // 2) Relative `.md` links and relations frontmatter.
    match frontmatter.get("relations") {
        Some(serde_json::Value::Array(relations)) => {
            for item in relations {
                match item {
                    serde_json::Value::Object(map) => {
                        let rtype = map
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("related")
                            .to_string();
                        match map.get("target").and_then(|v| v.as_str()) {
                            Some(t) => edges.push(edge(from, t, &rtype, "frontmatter".to_string())),
                            None => warnings.push("relations entry missing 'target'".to_string()),
                        }
                    }
                    other => warnings.push(format!("illegal relations entry: {other}")),
                }
            }
        }
        Some(other) => warnings.push(format!(
            "illegal relations value: must be a list, got {other}"
        )),
        None => {}
    }

    edges.retain(|e| {
        let safe = !e.to.0.is_absolute()
            && !e
                .to
                .0
                .components()
                .any(|c| matches!(c, camino::Utf8Component::ParentDir));
        if !safe {
            warnings.push(format!("link target escapes wiki: {}", e.to.0));
        }
        safe
    });
    // Dedup by (to, relation_type, section).
    edges.sort_by(|a, b| {
        (&a.to, &a.relation_type, &a.section_source).cmp(&(
            &b.to,
            &b.relation_type,
            &b.section_source,
        ))
    });
    edges.dedup_by(|a, b| {
        a.to == b.to && a.relation_type == b.relation_type && a.section_source == b.section_source
    });
    (edges, warnings)
}

fn edge(from: &PathScope, to: &str, relation_type: &str, section: String) -> Edge {
    // Resolve relative to the from document's directory.
    let to_scope = resolve_target(from, to);
    Edge {
        from: from.clone(),
        to: to_scope,
        relation_type: relation_type.to_string(),
        section_source: section,
        status: EdgeStatus::Unresolved, // resolved during sync against the wiki set
    }
}

/// Extract `[[target]]` wikilinks, tracking the heading section for attribution.
fn extract_wikilinks(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current_section = String::new();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_WIKILINKS);
    let mut heading = false;
    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = true,
            Event::Text(text) if heading => {
                current_section = text.trim().to_string();
                heading = false;
            }
            Event::End(pulldown_cmark::TagEnd::Heading(_)) => heading = false,
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => {
                let target = dest_url.split('#').next().unwrap_or("").trim();
                if !target.is_empty() {
                    out.push((target.to_string(), current_section.clone()));
                }
            }
            Event::Start(Tag::Link { dest_url, .. })
                if !dest_url.contains(":")
                    && !dest_url.starts_with("//")
                    && (dest_url.ends_with(".md") || dest_url.contains(".md#")) =>
            {
                let target = dest_url.split('#').next().unwrap_or("").trim();
                if !target.is_empty() {
                    out.push((target.to_string(), current_section.clone()));
                }
            }
            _ => {}
        }
    }
    out
}

/// Resolve a wiki-link target into a root-relative path scope.
///
/// Kept simple: a bare `name` becomes `name.md`; a path containing `/` keeps
/// its directory; absolute `/...` wikilinks are treated as relative to the
/// wiki root (leading slash ignored). Exact resolution against the wiki set is
/// done by the caller.
fn resolve_target(from: &PathScope, to: &str) -> PathScope {
    let dir = from.0.parent().unwrap_or_else(|| camino::Utf8Path::new(""));
    let normalized = to.trim_start_matches('/');
    let candidate = if normalized.ends_with(".md") {
        normalized.to_string()
    } else {
        format!("{normalized}.md")
    };
    let mut dir_utf = if to.starts_with('/') {
        camino::Utf8PathBuf::new()
    } else {
        dir.to_path_buf()
    };
    for seg in candidate.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if !dir_utf.pop() {
                    return PathScope(camino::Utf8PathBuf::from("../").join(normalized));
                }
            }
            s => dir_utf.push(s),
        }
    }
    PathScope(dir_utf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Frontmatter;

    fn scope(p: &str) -> PathScope {
        PathScope(camino::Utf8PathBuf::from(p))
    }

    #[test]
    fn wikilink_and_relations() {
        let body = "# Section\n\nsee [[other]] and [[sub/page#anchor]]\n";
        let mut fm = Frontmatter::new();
        fm.insert(
            "relations".into(),
            serde_json::json!([
                {"type": "depends_on", "target": "architecture/retrieval.md"}
            ]),
        );
        let (edges, warns) = extract_edges(&scope("doc.md"), &fm, body);
        assert!(warns.is_empty());
        // other -> other.md (relative to doc.md dir = root)
        // sub/page#anchor -> sub/page.md
        // relations target kept verbatim
        let targets: Vec<String> = edges.iter().map(|e| e.to.0.to_string()).collect();
        assert!(targets.contains(&"other.md".to_string()));
        assert!(targets.contains(&"sub/page.md".to_string()));
        assert!(targets.contains(&"architecture/retrieval.md".to_string()));
    }

    #[test]
    fn resolves_relative_directory() {
        let from = scope("dir/notes.md");
        let e = edge(&from, "sibling.md", "[[link]]", "s".into());
        assert_eq!(e.to.0.as_str(), "dir/sibling.md");
    }

    #[test]
    fn illegal_relations_warns() {
        let mut fm = Frontmatter::new();
        fm.insert("relations".into(), serde_json::json![42]);
        let (_, warns) = extract_edges(&scope("a.md"), &fm, "");
        assert_eq!(warns.len(), 1);
    }
}
