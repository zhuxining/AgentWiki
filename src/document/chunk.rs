use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use text_splitter::{ChunkConfig, MarkdownSplitter};

use super::types::{Frontmatter, PathScope, RetrievalUnitKind, Slice};

/// The embedding model silently truncates input at this many tokens; fastembed
/// sets it on the tokenizer and never reports the loss.
const MODEL_TOKEN_LIMIT: usize = 512;
/// Reserve for special tokens and for slack in the character-to-token estimate.
const TOKEN_MARGIN: usize = 16;
/// Everything in `search_text` except the body must leave room for at least this
/// much body text, otherwise the embedded tail would be truncated.
const MIN_BODY_CHARS: usize = 96;
/// Upper bound for a body unit when the fixed part is small.
const MAX_BODY_CHARS: usize = 448;
/// Per-component cap, so one bloated field cannot starve the rest of the unit.
const COMPONENT_CHARS: usize = 240;
/// Room left for the non-body part of `search_text`, including its separator.
const FIXED_BUDGET: usize = MODEL_TOKEN_LIMIT - TOKEN_MARGIN - MIN_BODY_CHARS - 1;

pub fn chunk_document(
    frontmatter: &Frontmatter,
    body: &str,
    path: &PathScope,
    modified_at_ns: i64,
) -> Vec<Slice> {
    let filename = path.0.file_stem().unwrap_or_else(|| path.0.as_str());
    let summary = frontmatter
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut outline_headings = Vec::new();
    let mut heading = None;
    let mut text = String::new();
    let mut start = 0;
    let breadcrumb = |stack: &[(usize, String)]| {
        stack
            .iter()
            .map(|(_, text)| text.as_str())
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
                    outline_headings.push(text.clone());
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
    let title = frontmatter
        .get("title")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(filename);
    let tag_values = frontmatter
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|value| value.trim().to_lowercase())
        .collect::<Vec<_>>();
    let tags = tag_values.join(", ");
    let note_type = frontmatter
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("note")
        .to_owned();
    let facets = frontmatter
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "type" | "tags" | "summary" | "title" | "aliases"
            )
        })
        .map(|(key, value)| {
            format!(
                "{key}={}",
                serde_json::to_string(value).expect("frontmatter JSON value serializes")
            )
        })
        .collect::<Vec<_>>();
    let alias_values = frontmatter
        .get("aliases")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|value| value.trim().to_lowercase())
        .collect::<Vec<_>>();
    let aliases = alias_values.join(", ");
    let document_fixed = fit_fixed(&[path.0.as_str(), title, &aliases, summary, &tags]);
    let outline = fit_outline(&outline_headings, body_budget(&document_fixed));
    let document_search = format!("{document_fixed}\n{outline}").trim().to_owned();
    let mut slices = vec![Slice {
        path: path.clone(),
        chunk_id: hex::encode(Sha256::digest(format!("{}\0document", path.0).as_bytes())),
        unit_kind: RetrievalUnitKind::Document,
        note_type: note_type.clone(),
        tags: tag_values.clone(),
        facets: facets.clone(),
        frontmatter: frontmatter.clone(),
        modified_at_ns,
        ordinal: 0,
        section: String::new(),
        content: summary.to_owned(),
        search_text: document_search,
    }];
    let mut ordinal = 1u32;
    if sections.is_empty()
        || (!sections.iter().any(|(_, content)| !content.is_empty()) && body.trim().is_empty())
    {
        return slices;
    }
    for (section, content) in sections {
        // Priority order: path and title first, then the section path, so a bloated
        // summary only pushes out the least useful component.
        let fixed = fit_fixed(&[path.0.as_str(), title, &section, summary, &tags]);
        for fragment in split_oversized(&content, body_budget(&fixed)) {
            let source = format!("{section}\n{fragment}").trim().to_string();
            let search_text = format!("{fixed}\n{fragment}").trim().to_string();
            slices.push(Slice {
                path: path.clone(),
                chunk_id: hex::encode(Sha256::digest(
                    format!("{}\0{}\0{}", path.0, ordinal, source).as_bytes(),
                )),
                unit_kind: RetrievalUnitKind::Fragment,
                note_type: note_type.clone(),
                tags: tag_values.clone(),
                facets: facets.clone(),
                frontmatter: Frontmatter::new(),
                modified_at_ns,
                ordinal,
                section: section.clone(),
                content: fragment,
                search_text,
            });
            ordinal += 1;
        }
    }
    slices
}

/// Character budget for one body unit.
///
/// `bge-small-zh-v1.5` uses a WordPiece vocabulary in which every token covers at
/// least one input character, so a character count is an upper bound on the token
/// count. Subtracting the characters of everything else stored in `search_text`
/// therefore keeps the embedded text inside the model limit instead of letting it
/// be truncated. A future byte-level BPE model would invalidate this bound.
fn body_budget(fixed: &str) -> usize {
    MODEL_TOKEN_LIMIT
        .saturating_sub(fixed.chars().count() + 1)
        .saturating_sub(TOKEN_MARGIN)
        .clamp(MIN_BODY_CHARS, MAX_BODY_CHARS)
}

/// Join the non-body parts of `search_text` in priority order under a hard budget.
///
/// A part is capped at [`COMPONENT_CHARS`] and trimmed to the remaining room rather
/// than dropped, so a long summary stays searchable by its leading text and the body
/// is never the thing that gets truncated. Titles, aliases and tags remain reachable
/// through `lookup_keys`, `tags` and `facets` regardless of this budget.
fn fit_fixed(parts: &[&str]) -> String {
    let mut fixed = String::new();
    for part in parts {
        let part = truncate_chars(part.trim(), COMPONENT_CHARS);
        if part.is_empty() {
            continue;
        }
        let separator = usize::from(!fixed.is_empty());
        let room = FIXED_BUDGET.saturating_sub(fixed.chars().count() + separator);
        let part = truncate_chars(part, room);
        if part.is_empty() {
            break;
        }
        if separator == 1 {
            fixed.push('\n');
        }
        fixed.push_str(part);
    }
    fixed
}

/// Cut to `limit` characters on a character boundary.
fn truncate_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// Keep whole headings that fit the budget so the outline never ends mid-word.
fn fit_outline(headings: &[String], budget: usize) -> String {
    let mut outline = String::new();
    for heading in headings {
        let candidate = if outline.is_empty() {
            heading.clone()
        } else {
            format!("{outline} / {heading}")
        };
        if candidate.chars().count() > budget {
            break;
        }
        outline = candidate;
    }
    outline
}

/// Split one section along Markdown semantic boundaries (paragraph, list or code
/// block, then sentence, then word) before falling back to characters, so a cut
/// only lands inside a paragraph when that paragraph alone exceeds the budget.
fn split_oversized(content: &str, budget: usize) -> Vec<String> {
    // A fixed fraction of the budget, so the overlap can never reach the capacity.
    let overlap = (budget / 8).max(1);
    let config = ChunkConfig::new(budget)
        .with_overlap(overlap)
        .expect("overlap is a fraction of the chunk budget")
        .with_trim(true);
    MarkdownSplitter::new(config)
        .chunks(content)
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_heading_without_parent_is_supported() {
        let slices = chunk_document(
            &Frontmatter::new(),
            "### Nested\n\n证据\n",
            &PathScope("a.md".into()),
            0,
        );
        assert_eq!(slices[0].unit_kind, RetrievalUnitKind::Document);
        assert_eq!(slices[1].section, "Nested");
    }

    #[test]
    fn code_headings_stay_inside_their_section() {
        let slices = chunk_document(
            &Frontmatter::new(),
            "## Parent\n\n```sh\n# shell comment\n```\n\ntext\n\nSibling\n=======\n\nnext\n",
            &PathScope("a.md".into()),
            0,
        );
        assert_eq!(slices.len(), 3);
        assert!(slices[1].content.contains("# shell comment"));
        assert_eq!(slices[2].section, "Sibling");
        assert_eq!(
            slices[0].search_text.lines().last(),
            Some("Parent / Sibling")
        );
        assert!(!slices[0].search_text.contains("shell comment"));
    }

    #[test]
    fn document_outline_preserves_siblings_and_heading_only_sections() {
        let slices = chunk_document(
            &Frontmatter::new(),
            "# Root\n\n## First\n\nfirst evidence\n\n### Child\n\nchild evidence\n\n## Empty\n\n## Last\n\nlast evidence\n",
            &PathScope("a.md".into()),
            0,
        );
        assert_eq!(slices.len(), 4);
        assert_eq!(slices[0].unit_kind, RetrievalUnitKind::Document);
        assert_eq!(
            slices[0].search_text.lines().last(),
            Some("Root / First / Child / Empty / Last")
        );
        assert_eq!(slices[1].section, "Root / First");
        assert_eq!(slices[2].section, "Root / First / Child");
        assert_eq!(slices[3].section, "Root / Last");
    }

    #[test]
    fn markdown_splitter_cuts_on_paragraphs_not_mid_sentence() {
        // Each paragraph is short; only their combination exceeds the budget, so a
        // cut may not land inside any of them.
        let paragraphs = [
            "第一段提供背景与目标。".repeat(14),
            "第二段列出约束与边界条件。".repeat(14),
            "第三段给出验证方式与验收结果。".repeat(14),
        ];
        let body = format!("## 方案\n\n{}", paragraphs.join("\n\n"));
        let slices = chunk_document(&Frontmatter::new(), &body, &PathScope("a.md".into()), 0);
        let fragments = &slices[1..];
        assert!(fragments.len() > 1, "section must be split: {fragments:?}");
        for fragment in fragments {
            assert!(
                paragraphs
                    .iter()
                    .any(|p| fragment.content.contains(p.as_str())),
                "chunk holds no whole paragraph: {}",
                fragment.content
            );
        }
        // Reassembling the chunks must not drop any paragraph text.
        for paragraph in &paragraphs {
            assert!(
                fragments
                    .iter()
                    .any(|f| f.content.contains(paragraph.as_str())),
                "a paragraph was dropped entirely"
            );
        }
    }

    #[test]
    fn code_blocks_are_kept_whole_when_they_fit_the_budget() {
        let block = "```rust\nfn main() {\n    println!(\"kept whole\");\n}\n```";
        let body = format!("## Example\n\n{block}\n\n尾随说明文字。\n");
        let slices = chunk_document(&Frontmatter::new(), &body, &PathScope("a.md".into()), 0);
        assert!(
            slices.iter().any(|slice| slice.content.contains(block)),
            "a code block that fits must not be split: {slices:?}"
        );
    }

    #[test]
    fn search_text_never_exceeds_the_model_token_budget() {
        // Chinese text is the worst case for this tokenizer: one token per character.
        // With a long summary, many tags and a deep heading path, the fixed part of
        // `search_text` eats most of the budget, and the body must shrink to fit.
        let mut frontmatter = Frontmatter::new();
        frontmatter.insert("title".into(), serde_json::json!("标题".repeat(30)));
        frontmatter.insert("summary".into(), serde_json::json!("摘要".repeat(150)));
        frontmatter.insert("tags".into(), serde_json::json!(vec!["标签".repeat(20); 4]));
        let mut body = String::new();
        for depth in 1..=6 {
            body.push_str(&format!("{} 很深的一级标题\n\n", "#".repeat(depth)));
        }
        body.push_str(&"正文内容需要被切分成多个片段。".repeat(80));
        let slices = chunk_document(
            &frontmatter,
            &body,
            &PathScope("深层/目录/文档.md".into()),
            0,
        );
        assert!(
            slices.len() > 3,
            "expected several fragments: {}",
            slices.len()
        );
        for slice in &slices {
            let chars = slice.search_text.chars().count();
            assert!(
                chars + TOKEN_MARGIN <= MODEL_TOKEN_LIMIT,
                "{} would be truncated: {chars} chars",
                slice.chunk_id
            );
        }
    }

    #[test]
    fn a_long_summary_is_bounded_but_still_searchable() {
        // A summary longer than its cap must keep its leading text: document-level
        // discovery relies on it, so dropping it entirely would be a regression.
        let mut frontmatter = Frontmatter::new();
        frontmatter.insert(
            "summary".into(),
            serde_json::json!(format!("chronoprobe {}", "背景说明".repeat(400))),
        );
        let slices = chunk_document(
            &frontmatter,
            "## 正文章节\n\n正文。",
            &PathScope("a.md".into()),
            0,
        );
        let document = &slices[0];
        let summary = document
            .search_text
            .lines()
            .find(|line| line.starts_with("chronoprobe"))
            .expect("the summary keeps its leading text");
        assert_eq!(summary.chars().count(), COMPONENT_CHARS);
        for slice in &slices {
            let chars = slice.search_text.chars().count();
            assert!(chars + TOKEN_MARGIN <= MODEL_TOKEN_LIMIT, "{chars} chars");
        }
    }

    #[test]
    fn outline_drops_trailing_headings_instead_of_cutting_one() {
        let headings: Vec<String> = (0..200).map(|i| format!("章节{i}")).collect();
        let outline = fit_outline(&headings, 60);
        assert!(outline.chars().count() <= 60);
        assert!(outline.starts_with("章节0 / 章节1"));
        // The last kept heading is complete, never a prefix of a heading name.
        assert!(headings.contains(&outline.rsplit(" / ").next().unwrap().to_owned()));
    }

    #[test]
    fn document_type_does_not_change_slicing() {
        let body = "# Title\n\nintro\n\n## Detail\n\nevidence\n";
        let mut profile = Frontmatter::new();
        profile.insert("type".into(), serde_json::json!("profile"));
        let mut event = Frontmatter::new();
        event.insert("type".into(), serde_json::json!("events"));
        let profile_units = chunk_document(&profile, body, &PathScope("a.md".into()), 1);
        let event_units = chunk_document(&event, body, &PathScope("a.md".into()), 1);

        assert_eq!(profile_units.len(), event_units.len());
        assert_eq!(
            profile_units
                .iter()
                .map(|unit| (&unit.unit_kind, &unit.section, &unit.content))
                .collect::<Vec<_>>(),
            event_units
                .iter()
                .map(|unit| (&unit.unit_kind, &unit.section, &unit.content))
                .collect::<Vec<_>>()
        );
    }
}
