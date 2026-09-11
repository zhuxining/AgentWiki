//! Deterministic, report-only validation of Markdown, frontmatter and
//! internal links against the rules carried by `AGENTWIKI.md`.

use camino::Utf8Path;

use crate::error::Result;
use crate::model::{Frontmatter, Issue, PathScope, Rule, Severity};
use crate::rules;

/// Validate one document (when `scope` is `Some`) or the whole wiki.
///
/// Issues are sorted by path then kind for stable output. `AGENTWIKI.md`
/// itself is never validated as content.
pub fn validate_wiki(root: &Utf8Path, scope: Option<&PathScope>) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    let paths = match scope {
        Some(p) => vec![p.clone()],
        None => crate::markdown::snapshot(root)?,
    };

    // Known document set, for broken-link detection.
    let known: std::collections::HashSet<String> = crate::markdown::snapshot(root)?
        .into_iter()
        .map(|p| p.0.to_string())
        .collect();

    // A broken rule file is reported once and disables rule-driven checks
    // this round, never misreporting missing fields.
    let (rule, rule_broken) = load_rules(root);
    if let Some(issue) = rule_broken {
        issues.push(issue);
    }

    // Tag counts across the archive; only the full-archive mode collects them
    // (judging a tag as "new" needs the whole-archive view).
    let tag_counts = if scope.is_none() {
        Some(collect_tag_counts(root, &paths)?)
    } else {
        None
    };

    for path in &paths {
        if path.0.as_str() == "AGENTWIKI.md" {
            continue;
        }
        validate_one(
            root,
            path,
            &known,
            rule.as_ref(),
            tag_counts.as_ref(),
            &mut issues,
        );
    }
    issues.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind)));
    Ok(issues)
}

fn validate_one(
    root: &Utf8Path,
    path: &PathScope,
    known: &std::collections::HashSet<String>,
    rule: Option<&Rule>,
    tag_counts: Option<&std::collections::BTreeMap<String, usize>>,
    issues: &mut Vec<Issue>,
) {
    let full = root.join(path.0.as_path());
    let Ok(raw) = std::fs::read_to_string(&full) else {
        issues.push(Issue {
            path: path.0.to_string(),
            kind: "io.read".into(),
            message: "cannot read file".into(),
            severity: Severity::Error,
        });
        return;
    };

    let (frontmatter, body) = crate::markdown::parse_frontmatter(&raw);

    // Open-but-unclosed YAML fence: parse_frontmatter yields an empty map and
    // the raw text as body — detectable exactly when the body equals the raw.
    let trimmed = raw.trim_start_matches('\u{feff}');
    if trimmed.starts_with("---") && body == trimmed {
        issues.push(Issue {
            path: path.0.to_string(),
            kind: "markdown.parse".into(),
            message: "malformed YAML frontmatter (unterminated header)".into(),
            severity: Severity::Error,
        });
    }

    // Rule-driven checks (required fields / type / filename / tags).
    if let Some(rule) = rule {
        check_rules(path, &frontmatter, rule, tag_counts, issues);
    }

    // Broken internal links (wikilinks whose target is not a known document).
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
                severity: Severity::Warning,
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
            severity: Severity::Error,
        });
    }
}

/// Rule-driven checks: required fields, type/filename constraints of the most
/// specific matching section, and tag normalisation diagnostics.
fn check_rules(
    path: &PathScope,
    frontmatter: &Frontmatter,
    rule: &Rule,
    tag_counts: Option<&std::collections::BTreeMap<String, usize>>,
    issues: &mut Vec<Issue>,
) {
    let path_str = path.0.as_str();
    let eff = rules::effective_rules(path_str, rule);
    let error = |kind: &str, message: String| Issue {
        path: path_str.to_string(),
        kind: kind.to_string(),
        message,
        severity: Severity::Error,
    };

    // Required fields: root + every matching section, already deduplicated.
    for field in &eff.required_fields {
        if !frontmatter.contains_key(field.as_str()) {
            issues.push(error(
                "frontmatter.required",
                format!("missing required field `{field}`"),
            ));
        }
    }

    // Type / filename: the most specific matching section governs (sections
    // are ordered ascending by pattern length — RULES.md).
    if let Some(section) = eff.sections.last() {
        let actual_type = frontmatter
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or(&eff.default_type);
        if !section.types.is_empty() && !section.types.iter().any(|t| t == actual_type) {
            issues.push(error(
                "type.not_allowed",
                format!(
                    "type `{actual_type}` is not allowed in `{}` (allowed: {})",
                    section.path,
                    section.types.join(", ")
                ),
            ));
        }
        if let Some(pattern) = &section.filename_pattern {
            let file_name = path
                .0
                .file_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| path_str.to_string());
            if !rules::fnmatch(pattern, &file_name) {
                issues.push(error(
                    "path.filename",
                    format!("file name `{file_name}` does not match `{pattern}`"),
                ));
            }
        }
    }

    // Tags: structural validity, canonical-form suggestion, new-tag warning.
    match frontmatter.get("tags") {
        Some(serde_json::Value::Array(items)) => {
            let mut structurally_valid = true;
            for item in items {
                match item.as_str() {
                    Some(tag) if is_valid_tag(tag) => {
                        check_tag(path_str, tag, rule, tag_counts, issues);
                    }
                    _ => structurally_valid = false,
                }
            }
            if !structurally_valid {
                issues.push(error(
                    "tags.invalid",
                    "`tags` must be a list of non-empty strings without control characters".into(),
                ));
            }
        }
        Some(_) => issues.push(error(
            "tags.invalid",
            "`tags` must be a list of non-empty strings".into(),
        )),
        None => {}
    }
}

/// A tag is structurally valid when trimmed-non-empty and free of control chars.
fn is_valid_tag(tag: &str) -> bool {
    !tag.trim().is_empty() && !tag.chars().any(char::is_control)
}

/// Diagnose one tag: canonical, alias/case variant, or new (full-archive only).
fn check_tag(
    path: &str,
    tag: &str,
    rule: &Rule,
    tag_counts: Option<&std::collections::BTreeMap<String, usize>>,
    issues: &mut Vec<Issue>,
) {
    if rule.tag_aliases.contains_key(tag) {
        return; // canonical form
    }
    let lowered = tag.to_lowercase();
    let canonical = rule
        .tag_aliases
        .iter()
        .find(|(canon, aliases)| {
            canon.to_lowercase() == lowered || aliases.iter().any(|a| a.to_lowercase() == lowered)
        })
        .map(|(canon, _)| canon.clone());
    if let Some(canon) = canonical {
        issues.push(Issue {
            path: path.to_string(),
            kind: "tags.non_canonical".into(),
            message: format!("tag `{tag}` is not canonical; use `{canon}`"),
            severity: Severity::Warning,
        });
        return;
    }
    // Unconfigured tag: warn as new only with the whole-archive view (a tag
    // seen in exactly one document is a first occurrence).
    if let Some(counts) = tag_counts
        && counts.get(tag).copied().unwrap_or(0) <= 1
    {
        issues.push(Issue {
            path: path.to_string(),
            kind: "tags.new".into(),
            message: format!("tag `{tag}` is new; consider adding it to `tag_aliases`"),
            severity: Severity::Warning,
        });
    }
}

/// Per-tag occurrence counts across the archive (full-archive validation).
fn collect_tag_counts(
    root: &Utf8Path,
    paths: &[PathScope],
) -> Result<std::collections::BTreeMap<String, usize>> {
    let mut counts = std::collections::BTreeMap::new();
    for p in paths {
        if p.0.as_str() == "AGENTWIKI.md" {
            continue;
        }
        let full = root.join(p.0.as_path());
        let Ok(raw) = std::fs::read_to_string(&full) else {
            continue;
        };
        let (fm, _) = crate::markdown::parse_frontmatter(&raw);
        let Some(serde_json::Value::Array(items)) = fm.get("tags") else {
            continue;
        };
        for item in items {
            if let Some(tag) = item.as_str() {
                *counts.entry(tag.to_string()).or_insert(0) += 1;
            }
        }
    }
    Ok(counts)
}

/// Read and parse `root/AGENTWIKI.md` into a [`Rule`].
///
/// Missing file: `(None, None)`. Broken file: `(None, Some(rules.parse))` —
/// reported once, rule-driven checks skipped this round.
fn load_rules(root: &Utf8Path) -> (Option<Rule>, Option<Issue>) {
    let p = root.join("AGENTWIKI.md");
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return (None, None);
    };
    let (fm, _) = crate::markdown::parse_frontmatter(&raw);
    match crate::rules::parse_rules(&fm) {
        Ok(rule) => (Some(rule), None),
        Err(e) => (
            None,
            Some(Issue {
                path: "AGENTWIKI.md".into(),
                kind: "rules.parse".into(),
                message: format!("rule file could not be parsed: {e}"),
                severity: Severity::Error,
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Severity;

    /// Write a wiki into `dir`: `AGENTWIKI.md` rules plus `docs` (path, content).
    fn wiki(dir: &std::path::Path, rules: &str, docs: &[(&str, &str)]) -> camino::Utf8PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.to_path_buf()).unwrap();
        std::fs::write(root.join("AGENTWIKI.md"), rules).unwrap();
        for (rel, content) in docs {
            let full = root.join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        root
    }

    const RULES: &str = r#"---
name: Test Wiki
default_type: note
required_fields:
  - title
  - type
  - tags
tag_aliases:
  guide:
    - guides
sections:
  - path: guides
    types:
      - guide
    required_fields:
      - status
    filename_pattern: "*.md"
  - path: notes
    types:
      - note
---"#;

    #[test]
    fn requires_merged_fields_and_type() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            RULES,
            &[("guides/onboarding.md", "# Onboarding\n\ntext\n")],
        );
        let issues = validate_wiki(&root, None).unwrap();
        let kinds: Vec<&str> = issues.iter().map(|i| i.kind.as_str()).collect();
        // Root fields (title/type) + guides section field (status).
        for expected in ["frontmatter.required", "type.not_allowed"] {
            assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
        }
        // type absent -> default_type `note`, not allowed in guides (guide only).
        let type_issue = issues
            .iter()
            .find(|i| i.kind == "type.not_allowed")
            .unwrap();
        assert!(type_issue.message.contains("note"));
        assert_eq!(type_issue.severity, Severity::Error);
    }

    #[test]
    fn generic_doc_only_requires_root_fields() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            RULES,
            &[("misc/odds.md", "---\ntitle: T\n---\n\nbody\n")],
        );
        let issues = validate_wiki(&root, None).unwrap();
        let kinds: Vec<&str> = issues.iter().map(|i| i.kind.as_str()).collect();
        // `misc` matches no section: `type`/`tags` required at root level only.
        assert!(kinds.contains(&"frontmatter.required"));
        assert!(!kinds.contains(&"type.not_allowed")); // no section governs
        assert!(!kinds.contains(&"path.filename"));
    }

    #[test]
    fn filename_pattern_and_type_check() {
        // A pattern a scanned (`.md`) file can actually violate.
        let rules = r#"---
required_fields: [title, type]
sections:
  - path: guides
    types: [guide]
    filename_pattern: "guide-*.md"
---"#;
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            rules,
            &[(
                "guides/MANUAL.md",
                "---\ntitle: T\ntype: guide\n---\n\nbody\n",
            )],
        );
        let issues = validate_wiki(&root, None).unwrap();
        assert!(
            issues.iter().any(|i| i.kind == "path.filename"),
            "missing path.filename in {issues:?}"
        );
        // A conforming file name passes.
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            rules,
            &[(
                "guides/guide-intro.md",
                "---\ntitle: T\ntype: guide\n---\n\nbody\n",
            )],
        );
        let issues = validate_wiki(&root, None).unwrap();
        assert!(
            !issues.iter().any(|i| i.kind == "path.filename"),
            "unexpected path.filename in {issues:?}"
        );
    }

    #[test]
    fn invalid_tags_report_structural_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            RULES,
            &[(
                "notes/a.md",
                "---\ntitle: T\ntype: note\ntags: [ok, \"\"]\n---\n\nbody\n",
            )],
        );
        let issues = validate_wiki(&root, None).unwrap();
        let bad = issues.iter().find(|i| i.kind == "tags.invalid").unwrap();
        assert_eq!(bad.severity, Severity::Error);
        // canonical tag `ok` itself is fine (empty string is the offender).
        assert!(!issues.iter().any(|i| i.kind == "tags.non_canonical"));
    }

    #[test]
    fn alias_and_case_variants_suggest_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            RULES,
            &[(
                "notes/a.md",
                "---\ntitle: T\ntype: note\ntags: [guides, Guide]\n---\n\nbody\n",
            )],
        );
        let issues = validate_wiki(&root, None).unwrap();
        let nc: Vec<&Issue> = issues
            .iter()
            .filter(|i| i.kind == "tags.non_canonical")
            .collect();
        assert_eq!(nc.len(), 2, "both alias and case variant flagged");
        for issue in nc {
            assert!(issue.message.contains("`guide`"));
            assert_eq!(issue.severity, Severity::Warning);
        }
    }

    #[test]
    fn new_tag_warned_only_in_full_archive_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            RULES,
            &[(
                "notes/a.md",
                "---\ntitle: T\ntype: note\ntags: [branders]\n---\n\nbody\n",
            )],
        );
        // Full archive: `branders` appears in exactly one document -> new.
        let issues = validate_wiki(&root, None).unwrap();
        assert!(issues.iter().any(|i| i.kind == "tags.new"));
        // Single-document mode: no whole-archive view -> silently skipped.
        let path = PathScope(camino::Utf8PathBuf::from("notes/a.md"));
        let issues = validate_wiki(&root, Some(&path)).unwrap();
        assert!(!issues.iter().any(|i| i.kind == "tags.new"));
    }

    #[test]
    fn tag_known_across_archive_not_reported_new() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            RULES,
            &[
                (
                    "notes/a.md",
                    "---\ntitle: A\ntype: note\ntags: [shared]\n---\n\nbody\n",
                ),
                (
                    "notes/b.md",
                    "---\ntitle: B\ntype: note\ntags: [shared]\n---\n\nbody\n",
                ),
            ],
        );
        let issues = validate_wiki(&root, None).unwrap();
        assert!(!issues.iter().any(|i| i.kind == "tags.new"));
    }

    #[test]
    fn broken_rule_file_reports_rules_parse_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(
            dir.path(),
            "---\nrequired_fields: [title]\ntitel: oops\n---\n\nbody\n",
            &[("notes/a.md", "# no frontmatter at all\n\nbody\n")],
        );
        let issues = validate_wiki(&root, None).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "rules.parse");
        assert_eq!(issues[0].path, "AGENTWIKI.md");
        assert_eq!(issues[0].severity, Severity::Error);
        // No `frontmatter.required` misreporting from the broken rule file.
        assert!(!issues.iter().any(|i| i.kind == "frontmatter.required"));
    }

    #[test]
    fn empty_rule_file_skips_rule_checks() {
        let dir = tempfile::tempdir().unwrap();
        let root = wiki(dir.path(), "", &[("notes/a.md", "# hello\n\nbody\n")]);
        // Empty AGENTWIKI.md has no frontmatter -> parse gives an empty rule.
        let issues = validate_wiki(&root, None).unwrap();
        assert!(!issues.iter().any(|i| i.kind == "frontmatter.required"));
        assert!(!issues.iter().any(|i| i.kind == "rules.parse"));
    }
}
