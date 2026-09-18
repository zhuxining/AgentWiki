use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use text_splitter::{ChunkConfig, TextSplitter};

use super::types::{Frontmatter, PathScope, RetrievalUnitKind, Slice};

const MAX_CHUNK_CHARS: usize = 1_200;
const OVERLAP_CHARS: usize = 150;

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
    let outline = headings
        .iter()
        .map(|(_, heading)| heading.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    let document_search = format!(
        "{}\n{title}\n{aliases}\n{summary}\n{tags}\n{outline}",
        path.0
    )
    .trim()
    .to_owned();
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
        for fragment in split_oversized(&content) {
            let source = format!("{section}\n{fragment}").trim().to_string();
            let search_text = format!("{}\n{title}\n{summary}\n{tags}\n{source}", path.0)
                .trim()
                .to_string();
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
