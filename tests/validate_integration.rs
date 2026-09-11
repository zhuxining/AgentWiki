//! End-to-end validation contract over a realistic wiki layout.
//!
//! Exercises `validate::validate_wiki` against a temp wiki that mirrors the
//! reference `docs/AGENTWIKI.md` rules: root required fields, `sections` with
//! types/filename patterns, `tag_aliases`, broken links and full vs
//! single-document modes.

use std::collections::HashSet;

use agentwiki::model::{Issue, PathScope, Severity};

const RULES: &str = r#"---
name: Test Wiki
purpose: 端到端校验契约
default_type: note
required_fields:
  - title
  - type
  - tags
tag_aliases:
  guide:
    - guides
  decision:
    - decisions
sections:
  - path: decisions
    description: 已确认的决策
    types: [decision]
    required_fields: [status, decided_at]
  - path: guides
    types: [guide]
    required_fields: [status]
    filename_pattern: "guide-*.md"
  - path: notes
    types: [note, guide]
---"#;

/// A fully conforming document for every section (all-canonical tags).
const CLEAN: &str = r#"---
title: 合格文档
type: note
tags: [guide, decision]
created_at: 2026-01-01
updated_at: 2026-01-02
owner: nobody
---

# 正文

内容与 [[另一篇.md]] 相关。
"#;

fn write_wiki(dir: &std::path::Path, docs: &[(&str, &str)]) -> camino::Utf8PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(dir.to_path_buf()).unwrap();
    std::fs::write(root.join("AGENTWIKI.md"), RULES).unwrap();
    for (rel, content) in docs {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    root
}

fn kinds_of(issues: &[Issue]) -> HashSet<&str> {
    issues.iter().map(|i| i.kind.as_str()).collect()
}

#[test]
fn clean_wiki_passes_all_rule_checks() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_wiki(
        dir.path(),
        &[
            ("notes/另一篇.md", CLEAN),
            (
                "notes/正文.md",
                "---\ntitle: 主文档\ntype: note\ntags: [guide]\n---\n\n# 正文\n\n见 [[另一篇.md]]。\n",
            ),
        ],
    );
    let issues = agentwiki::validate::validate_wiki(&root, None).unwrap();
    // `guide` is the canonical tag; the cross link resolves; every required
    // field is present.
    assert!(issues.is_empty(), "expected a clean wiki, got: {issues:?}");
}

#[test]
fn rule_violations_report_kinds_and_severity() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_wiki(
        dir.path(),
        &[
            // Missing root fields, wrong type for `decisions`, broken link,
            // alias tag, new tag.
            (
                "decisions/bad.md",
                "---\ntitle: 缺字段\ntags: [decision, 新标签]\n---\n\n见 [[not-there.md]]。\n",
            ),
            // `type: note` not allowed in `guides`; name violates the pattern;
            // `decisions` is an alias of `decision`.
            (
                "guides/notes.md",
                "---\ntitle: 违规指南\ntype: note\ntags: [decisions]\nstatus: draft\n---\n\n正文\n",
            ),
            // Conforming docs, the second being the first one's link target.
            ("notes/ok.md", CLEAN),
            (
                "notes/另一篇.md",
                "---\ntitle: 另一篇\ntype: note\ntags: [guide]\n---\n\n正文\n",
            ),
        ],
    );
    let issues = agentwiki::validate::validate_wiki(&root, None).unwrap();
    let kinds = kinds_of(&issues);

    // Required: root (type) + decisions section (status, decided_at).
    for expected in [
        "frontmatter.required",
        "type.not_allowed",
        "path.filename",
        "link.broken",
    ] {
        assert!(kinds.contains(expected), "missing {expected} in {issues:?}");
    }
    // `decisions` (alias) non_canonical; `新标签` new.
    assert!(kinds.contains("tags.non_canonical"), "{issues:?}");
    assert!(kinds.contains("tags.new"), "{issues:?}");

    for issue in &issues {
        match issue.kind.as_str() {
            "frontmatter.required" | "type.not_allowed" | "path.filename" => {
                assert_eq!(issue.severity, Severity::Error, "{issue:?}");
            }
            "tags.non_canonical" | "tags.new" | "link.broken" => {
                assert_eq!(issue.severity, Severity::Warning, "{issue:?}");
            }
            _ => {}
        }
        assert!(
            issue.path == "AGENTWIKI.md"
                || issue.path.starts_with("decisions/")
                || issue.path.starts_with("guides/"),
            "unexpected issue path {:?}",
            issue.path
        );
    }
}

#[test]
fn single_document_mode_skips_new_tag_but_checks_own_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_wiki(
        dir.path(),
        &[(
            "notes/only.md",
            "---\ntitle: 独篇\ntype: note\ntags: [独一无二]\n---\n\n见 [[missing.md]]。\n",
        )],
    );
    let path = PathScope(camino::Utf8PathBuf::from("notes/only.md"));
    let issues = agentwiki::validate::validate_wiki(&root, Some(&path)).unwrap();
    let kinds = kinds_of(&issues);
    // Whole-archive judgment (tags.new) is skipped; own broken link is kept.
    assert!(!kinds.contains("tags.new"), "{issues:?}");
    assert!(kinds.contains("link.broken"), "{issues:?}");
    // Full-archive mode flags the tag as new.
    let issues = agentwiki::validate::validate_wiki(&root, None).unwrap();
    assert!(kinds_of(&issues).contains("tags.new"), "{issues:?}");
}

#[test]
fn broken_rule_file_never_misreports_required_fields() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_wiki_or_rules(
        dir.path(),
        "---\nrequired_fields: [title]\ntitlee: typo\n---\n",
        &[("notes/a.md", "# 无 frontmatter\n\n正文\n")],
    );
    let issues = agentwiki::validate::validate_wiki(&root, None).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind, "rules.parse");
    assert_eq!(issues[0].path, "AGENTWIKI.md");
    assert_eq!(issues[0].severity, Severity::Error);
}

fn write_wiki_or_rules(
    dir: &std::path::Path,
    rules: &str,
    docs: &[(&str, &str)],
) -> camino::Utf8PathBuf {
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
