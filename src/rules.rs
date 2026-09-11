//! `AGENTWIKI.md` 规则文件的解析与路径匹配（纯函数，无 I/O）：
//! `parse_rules` 将 Frontmatter 解码为 [`Rule`]（拒绝未知字段）；
//! `matches_section`/`matching_sections` 实现目录子树 + fnmatch 匹配
//! （`*` 跨 `/`）；`effective_rules` 合并根规则与匹配 sections。

use crate::error::Result;
use crate::model::{EffectiveRules, Frontmatter, Rule, RuleSection};

/// Decode rule-file frontmatter into a [`Rule`], applying contract defaults
/// (`version = 1`, `name = "AgentWiki"`, `default_type = "note"`) and
/// rejecting unknown fields.
///
/// # Errors
/// `Parse` when the map violates the rule contract or `version < 1`.
pub fn parse_rules(fm: &Frontmatter) -> Result<Rule> {
    let rule: Rule =
        serde_json::from_value(serde_json::Value::Object(fm.clone())).map_err(|e| {
            crate::error::AgentWikiError::Parse {
                path: "AGENTWIKI.md".into(),
                message: format!("invalid rule file: {e}"),
            }
        })?;
    if rule.version < 1 {
        return Err(crate::error::AgentWikiError::Parse {
            path: "AGENTWIKI.md".into(),
            message: format!("rule version must be >= 1, got {}", rule.version),
        });
    }
    Ok(rule)
}

/// fnmatch glob match with `*` crossing `/` (RULES.md semantics, unlike
/// `glob`-style matching). Supports `*`, `?`, `[...]` (negation, ranges).
pub fn fnmatch(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    // `star`/`star_t`: last `*` position and the text offset it covers,
    // for backtracking when a later part fails.
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;

    while t < txt.len() {
        let matched = if p < pat.len() && pat[p] == '?' {
            p += 1;
            t += 1;
            true
        } else if p < pat.len() && pat[p] == '[' {
            match class_index(&pat, p, txt[t]) {
                Some(after) => {
                    p = after;
                    t += 1;
                    true
                }
                None => false,
            }
        } else if p < pat.len() && pat[p] == txt[t] {
            p += 1;
            t += 1;
            true
        } else {
            false
        };
        if matched {
            continue;
        }
        if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            while p < pat.len() && pat[p] == '*' {
                p += 1;
            }
            star_t = t;
            continue; // let the star match zero chars for now
        }
        match star {
            Some(sp) => {
                // Backtrack: star consumes one more character.
                p = sp + 1;
                star_t += 1;
                t = star_t;
                if t > txt.len() {
                    return false;
                }
            }
            None => return false,
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Match a character class starting at `pat[p]` (`[`) against `ch`; returns
/// the index past `]`, or `None` when not matched / unterminated.
fn class_index(pat: &[char], p: usize, ch: char) -> Option<usize> {
    let mut i = p + 1;
    let negate = matches!(pat.get(i), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pat.len() {
        let c = pat[i];
        if c == ']' && !first {
            return if matched != negate { Some(i + 1) } else { None };
        }
        first = false;
        if i + 2 < pat.len() && pat[i + 1] == '-' && pat[i + 2] != ']' {
            if pat[i] <= ch && ch <= pat[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if c == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None // unterminated '['
}

/// True when a wiki-relative `path` falls under a section pattern: plain
/// `guides` covers the `guides` subtree, patterns use fnmatch (RULES.md).
pub fn matches_section(path: &str, section_path: &str) -> bool {
    if fnmatch(section_path, path) {
        return true;
    }
    let dir = section_path.trim_end_matches('/');
    !dir.is_empty() && path.starts_with(&format!("{dir}/"))
}

/// Every section whose pattern matches `path`, sorted by pattern length
/// ascending so more specific sections are applied last (RULES.md).
pub fn matching_sections<'a>(path: &str, rule: &'a Rule) -> Vec<&'a RuleSection> {
    let mut out: Vec<&RuleSection> = rule
        .sections
        .iter()
        .filter(|s| matches_section(path, &s.path))
        .collect();
    out.sort_by_key(|s| s.path.len());
    out
}

/// Merge the root rules with every section matching `path`.
///
/// `required_fields` is the root list plus each matching section's list,
/// deduplicated in ascending-specificity order.
pub fn effective_rules(path: &str, rule: &Rule) -> EffectiveRules {
    let mut required: Vec<String> = Vec::new();
    let mut push = |list: &[String]| {
        for f in list {
            if !required.contains(f) {
                required.push(f.clone());
            }
        }
    };
    push(&rule.required_fields);
    let sections = matching_sections(path, rule);
    for s in &sections {
        push(&s.required_fields);
    }
    EffectiveRules {
        default_type: rule.default_type.clone(),
        required_fields: required,
        tag_aliases: rule.tag_aliases.clone(),
        sections: sections.iter().map(|s| (*s).clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build the full-rule fixture equivalent to `docs/AGENTWIKI.md` frontmatter.
    fn fixture() -> Rule {
        let fm: Frontmatter = Frontmatter::from(
            serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(json!({
                "name": "Team Wiki",
                "purpose": "团队知识、技术决策和操作指南",
                "default_type": "note",
                "required_fields": ["title", "type", "tags", "owner"],
                "tag_aliases": {
                    "guide": ["guides"],
                    "decision": ["decisions"],
                    "architecture": ["arch"]
                },
                "sections": [
                    {"path": "decisions", "description": "决策", "types": ["decision"],
                     "required_fields": ["status", "decided_at"], "filename_pattern": "*.md"},
                    {"path": "guides", "types": ["guide"], "required_fields": ["status"]},
                    {"path": "projects/*", "types": ["project", "note"],
                     "required_fields": ["project"]},
                    {"path": "notes", "types": ["note", "guide"], "required_fields": []}
                ]
            }))
            .unwrap(),
        );
        parse_rules(&fm).unwrap()
    }

    #[test]
    fn parses_full_rule_file() {
        let rule = fixture();
        assert_eq!(rule.name, "Team Wiki");
        assert_eq!(rule.version, 1);
        assert_eq!(rule.default_type, "note");
        assert_eq!(rule.required_fields, vec!["title", "type", "tags", "owner"]);
        assert_eq!(rule.tag_aliases["architecture"], vec!["arch"]);
        assert_eq!(rule.sections.len(), 4);
        let decisions = &rule.sections[0];
        assert_eq!(decisions.path, "decisions");
        assert_eq!(decisions.types, vec!["decision"]);
        assert_eq!(decisions.required_fields, vec!["status", "decided_at"]);
        assert_eq!(decisions.filename_pattern.as_deref(), Some("*.md"));
    }

    #[test]
    fn applies_contract_defaults() {
        let fm: Frontmatter = serde_json::from_value(json!({
            "required_fields": ["title"]
        }))
        .unwrap();
        let rule = parse_rules(&fm).unwrap();
        assert_eq!(rule.version, 1);
        assert_eq!(rule.name, "AgentWiki");
        assert_eq!(rule.default_type, "note");
        assert!(rule.purpose.is_empty());
        assert!(rule.sections.is_empty());
        assert!(rule.tag_aliases.is_empty());
    }

    #[test]
    fn rejects_unknown_fields() {
        let fm: Frontmatter = serde_json::from_value(json!({
            "required_fields": ["title"],
            "titel": ["typo"]
        }))
        .unwrap();
        assert!(parse_rules(&fm).is_err());
        let fm: Frontmatter = serde_json::from_value(json!({
            "sections": [{"path": "guides", "unknown_option": true}]
        }))
        .unwrap();
        assert!(parse_rules(&fm).is_err());
    }

    #[test]
    fn rejects_expired_version() {
        let fm: Frontmatter = serde_json::from_value(json!({"version": 0})).unwrap();
        assert!(parse_rules(&fm).is_err());
    }

    #[test]
    fn fnmatch_star_crosses_slash() {
        assert!(fnmatch("*.md", "a/b.md"));
        assert!(fnmatch("projects/*", "projects/foo.md"));
        assert!(fnmatch("projects/*", "projects/a/b.md"));
        assert!(fnmatch("*decisions*", "projects/decisions/x.md"));
        assert!(!fnmatch("decisions", "decisions-old/x.md"));
        assert!(!fnmatch("projects/*", "other/foo.md"));
    }

    #[test]
    fn fnmatch_question_and_literals() {
        assert!(fnmatch("?", "a"));
        assert!(!fnmatch("?", "ab"));
        assert!(fnmatch("a?c", "abc"));
        assert!(fnmatch("a.b", "a.b")); // `.` is a literal, not a wildcard
        assert!(!fnmatch("a.b", "axb"));
        assert!(fnmatch("", ""));
        assert!(!fnmatch("", "x"));
        assert!(fnmatch("*", ""));
    }

    #[test]
    fn fnmatch_character_classes() {
        assert!(fnmatch("[a-c]x", "bx"));
        assert!(!fnmatch("[a-c]x", "dx"));
        assert!(fnmatch("[!a]x", "bx"));
        assert!(!fnmatch("[!a]x", "ax"));
        assert!(fnmatch("[^a]x", "bx"));
        assert!(fnmatch("file[0-9].md", "file7.md"));
        assert!(!fnmatch("file[0-9].md", "filex.md"));
    }

    #[test]
    fn section_matching_subtree_and_glob() {
        for p in ["decisions/x.md", "decisions/sub/x.md"] {
            assert!(matches_section(p, "decisions"), "{p}");
        }
        assert!(matches_section("decisions", "decisions"));
        // No subtree expansion for `projects/decisions/` nor `decisions-old/`.
        assert!(!matches_section("projects/decisions/x.md", "decisions"));
        assert!(!matches_section("decisions-old/x.md", "decisions"));
        // fnmatch is whole-string: `projects/*/decisions` covers the dir
        // itself, not files under it.
        assert!(matches_section(
            "projects/alpha/decisions",
            "projects/*/decisions"
        ));
        assert!(!matches_section(
            "projects/alpha/decisions/x.md",
            "projects/*/decisions"
        ));
        assert!(matches_section(
            "archive/decisions-2024/x.md",
            "*decisions*"
        ));
        assert!(matches_section("projects/foo.md", "projects/*"));
        assert!(!matches_section("other/foo.md", "projects/*"));
    }

    #[test]
    fn matching_sections_ordered_by_specificity() {
        let rule = fixture();
        // `notes` subtree needs a literal prefix, so only `projects/*` matches.
        let sections = matching_sections("projects/alpha/notes.md", &rule);
        let paths: Vec<&str> = sections.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["projects/*"]);
        let sections = matching_sections("decisions/x.md", &rule);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].path, "decisions");
        // Ascending order when several match: `a/*/c` (len 5) vs `a` (len 1).
        let multi = Rule {
            sections: vec![
                RuleSection {
                    path: "a".into(),
                    ..Default::default()
                },
                RuleSection {
                    path: "a/*/c".into(),
                    ..Default::default()
                },
            ],
            ..fixture()
        };
        let sections = matching_sections("a/b/c", &multi);
        let paths: Vec<&str> = sections.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["a", "a/*/c"]);
    }

    #[test]
    fn effective_rules_merge_deduplicates() {
        let rule = fixture();
        // Root fields + guides-specific extras; uncovered dir keeps root only.
        let eff = effective_rules("guides/onboarding.md", &rule);
        assert_eq!(
            eff.required_fields,
            vec!["title", "type", "tags", "owner", "status"]
        );
        assert_eq!(eff.default_type, "note");
        assert_eq!(eff.sections.len(), 1);
        assert_eq!(eff.sections[0].path, "guides");
        let eff = effective_rules("misc/odds.md", &rule);
        assert_eq!(eff.required_fields, vec!["title", "type", "tags", "owner"]);
        assert!(eff.sections.is_empty());
    }

    #[test]
    fn rule_section_defaults() {
        let fm: Frontmatter = serde_json::from_value(json!({
            "sections": [{"path": "misc"}]
        }))
        .unwrap();
        let rule = parse_rules(&fm).unwrap();
        assert!(rule.sections[0].types.is_empty());
        assert!(rule.sections[0].required_fields.is_empty());
        assert!(rule.sections[0].filename_pattern.is_none());
    }
}
