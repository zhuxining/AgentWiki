//! Deterministic validation of Markdown, frontmatter and internal links.
//!
//! Validation is **report-only**: it never rewrites Markdown. It compares a
//! document's structure against the reserved `AGENTWIKI.md` rules (or an
//! explicit set) and the set of known documents, and returns a list of
//! [`Issue`]s for the caller (CLI / MCP) to present.

use camino::Utf8Path;

use crate::error::Result;
use crate::model::PathScope;

/// A single, actionable validation finding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Issue {
    /// Wiki-relative document path.
    pub path: String,
    /// Stable machine kind, e.g. `frontmatter.required`, `link.broken`, `markdown.parse`.
    pub kind: String,
    /// Human-readable explanation.
    pub message: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Validate one document (when `scope` is `Some`) or the whole wiki.
///
/// Reports every finding; never rewrites. A `None` scope validates every `*.md`
/// (except the reserved `AGENTWIKI.md`, which is the rules file, not content).
pub fn validate_wiki(root: &Utf8Path, scope: Option<&PathScope>) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    let paths = match scope {
        Some(p) => vec![p.clone()],
        None => crate::markdown::snapshot(root)?,
    };

    // Build the set of known targets so we can flag broken internal links.
    let known: std::collections::HashSet<String> = crate::markdown::snapshot(root)?
        .into_iter()
        .map(|p| p.0.to_string())
        .collect();

    for path in &paths {
        if path.0.as_str() == "AGENTWIKI.md" {
            continue;
        }
        validate_one(root, path, &known, &mut issues);
    }
    Ok(issues)
}

fn validate_one(
    root: &Utf8Path,
    path: &PathScope,
    known: &std::collections::HashSet<String>,
    issues: &mut Vec<Issue>,
) {
    let full = root.join(path.0.as_path());
    let Ok(raw) = std::fs::read_to_string(&full) else {
        issues.push(Issue {
            path: path.0.to_string(),
            kind: "io.read".into(),
            message: "cannot read file".into(),
        });
        return;
    };

    let (frontmatter, body) = crate::markdown::parse_frontmatter(&raw);

    // 1) Parse-level: an unparseable (non-map) frontmatter yields an empty map.
    //    Detect a malformed YAML header by checking for open but unclosed fences.
    let trimmed = raw.trim_start_matches('\u{feff}');
    if trimmed.starts_with("---") && body == trimmed {
        issues.push(Issue {
            path: path.0.to_string(),
            kind: "markdown.parse".into(),
            message: "malformed YAML frontmatter (unterminated header)".into(),
        });
    }

    // 2) Required fields from the reserved rule file.
    if let Some(rules) = load_rules(root) {
        for field in &rules.required_fields {
            if !frontmatter.contains_key(field.as_str()) {
                issues.push(Issue {
                    path: path.0.to_string(),
                    kind: "frontmatter.required".into(),
                    message: format!("missing required field `{field}`"),
                });
            }
        }
    }

    // 3) Broken internal links (wikilinks whose target is not a known document).
    let (edges, _warns) = crate::graph::extract_edges(path, &frontmatter, &body);
    for e in edges {
        if e.status == crate::model::EdgeStatus::Resolved {
            continue;
        }
        if !known.contains(e.to.0.as_str()) {
            issues.push(Issue {
                path: path.0.to_string(),
                kind: "link.broken".into(),
                message: format!("internal link target not present: {}", e.to.0),
            });
        }
    }

    // 4) Structural: non-empty content must produce at least one slice.
    let slices = crate::markdown::chunk_document(
        frontmatter
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        &body,
        path,
    );
    if body.trim_end().is_empty() && slices.iter().all(|s| s.content.is_empty()) {
        issues.push(Issue {
            path: path.0.to_string(),
            kind: "content.empty".into(),
            message: "document has no body content".into(),
        });
    }
}

/// Minimal rules loader: read `root/AGENTWIKI.md` frontmatter as the rule set.
///
/// Kept dependency-light (`serde_yaml`), consistent with the rest of the crate.
fn load_rules(root: &Utf8Path) -> Option<crate::model::Rule> {
    let p = root.join("AGENTWIKI.md");
    let raw = std::fs::read_to_string(&p).ok()?;
    let (fm, _) = crate::markdown::parse_frontmatter(&raw);
    let required_fields = fm
        .get("required_fields")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some(crate::model::Rule {
        scope: None,
        required_fields,
        tag_aliases: Default::default(),
        sections: Vec::new(),
    })
}
