use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use text_splitter::{ChunkConfig, TextSplitter};

use super::types::{PathScope, Slice};

const MAX_CHUNK_CHARS: usize = 1_200;
const OVERLAP_CHARS: usize = 150;

pub fn chunk_document(summary: &str, body: &str, path: &PathScope) -> Vec<Slice> {
    let filename = path.0.file_stem().unwrap_or_else(|| path.0.as_str());
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
    let mut slices = Vec::new();
    let mut ordinal = 0u32;
    if sections.is_empty()
        || (!sections.iter().any(|(_, content)| !content.is_empty()) && body.trim().is_empty())
    {
        slices.push(empty_slice(
            summary,
            filename,
            path,
            ordinal,
            breadcrumb(&headings),
        ));
        return slices;
    }
    for (section, content) in sections {
        for fragment in split_oversized(&content) {
            let source = format!("{section}\n{fragment}").trim().to_string();
            let search_text = format!("{filename}\n{summary}\n{source}")
                .trim()
                .to_string();
            slices.push(Slice {
                path: path.clone(),
                chunk_id: hex::encode(Sha256::digest(
                    format!("{}\0{}\0{}", path.0, ordinal, source).as_bytes(),
                )),
                ordinal,
                section: section.clone(),
                content: fragment,
                search_text,
                source_hash: hex::encode(Sha256::digest(source.as_bytes())),
            });
            ordinal += 1;
        }
    }
    slices
}

fn empty_slice(
    summary: &str,
    filename: &str,
    path: &PathScope,
    ordinal: u32,
    section: String,
) -> Slice {
    let source = section.clone();
    let search_text = format!("{filename}\n{summary}\n{source}")
        .trim()
        .to_string();
    Slice {
        path: path.clone(),
        chunk_id: hex::encode(Sha256::digest(
            format!("{}\0{}\0{}", path.0, ordinal, source).as_bytes(),
        )),
        ordinal,
        section,
        content: String::new(),
        search_text,
        source_hash: hex::encode(Sha256::digest(source.as_bytes())),
    }
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
        let slices = chunk_document("", "### Nested\n\n证据\n", &PathScope("a.md".into()));
        assert_eq!(slices[0].section, "Nested");
    }

    #[test]
    fn code_headings_stay_inside_their_section() {
        let slices = chunk_document(
            "",
            "## Parent\n\n```sh\n# shell comment\n```\n\ntext\n\nSibling\n=======\n\nnext\n",
            &PathScope("a.md".into()),
        );
        assert_eq!(slices.len(), 2);
        assert!(slices[0].content.contains("# shell comment"));
        assert_eq!(slices[1].section, "Sibling");
    }
}
