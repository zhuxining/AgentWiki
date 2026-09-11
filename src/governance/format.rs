//! Markdown formatting adapter. Formatting is explicit and separate from
//! report-only validation.

use dprint_plugin_markdown::{configuration::ConfigurationBuilder, format_text};

pub fn format_markdown(source: &str) -> Result<Option<String>, String> {
    let config = ConfigurationBuilder::new().line_width(80).build();
    // Preserve the original YAML bytes, including comments and quoting.
    let prefix_len = if source.trim_start_matches('\u{feff}').starts_with("---\n")
        || source.starts_with("---\r\n")
    {
        let mut offset = 0;
        let mut end = None;
        for (index, line) in source.split_inclusive('\n').enumerate() {
            offset += line.len();
            if index > 0 && line.trim_end_matches(['\r', '\n']) == "---" {
                end = Some(offset);
                break;
            }
        }
        end.ok_or_else(|| "unterminated frontmatter".to_string())?
    } else {
        0
    };
    let body = &source[prefix_len..];
    let formatted =
        format_text(body, &config, |_tag, _code, _| Ok(None)).map_err(|e| e.to_string())?;
    Ok(formatted
        .map(|body| format!("{}{body}", &source[..prefix_len]))
        .filter(|text| text != source))
}

/// Apply a prepared format change only if both content and metadata still match.
pub fn format_file(
    root: &camino::Utf8Path,
    path: &crate::model::PathScope,
) -> crate::error::Result<bool> {
    use crate::error::AgentWikiError;
    use std::io::Write;
    crate::document::scope_path(root, &path.0)?;
    if path.0.as_str() == "AGENTWIKI.md" || path.0.extension() != Some("md") {
        return Err(AgentWikiError::Config(
            "expected an ordinary Markdown document".into(),
        ));
    }
    let file = root.join(&path.0);
    let io = |source| AgentWikiError::Io {
        path: file.clone(),
        source,
    };
    let metadata = std::fs::metadata(&file).map_err(io)?;
    let original = std::fs::read_to_string(&file).map_err(io)?;
    let Some(formatted) = format_markdown(&original).map_err(AgentWikiError::Other)? else {
        return Ok(false);
    };
    let mut temporary = tempfile::NamedTempFile::new_in(
        file.parent()
            .ok_or_else(|| AgentWikiError::Config("missing parent".into()))?,
    )
    .map_err(io)?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())
        .map_err(io)?;
    temporary.write_all(formatted.as_bytes()).map_err(io)?;
    temporary.as_file().sync_all().map_err(io)?;
    crate::document::scope_path(root, &path.0)?;
    let current = std::fs::metadata(&file).map_err(io)?;
    if current.len() != metadata.len()
        || current.modified().map_err(io)? != metadata.modified().map_err(io)?
        || std::fs::read_to_string(&file).map_err(io)? != original
    {
        return Err(AgentWikiError::Other(format!(
            "{}: formatting conflict; file changed",
            path.0
        )));
    }
    temporary.persist(&file).map_err(|e| io(e.error))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::format_markdown;

    #[test]
    fn formatting_is_available_and_idempotent() {
        let source = "# title\n\ntext\n";
        let formatted = format_markdown(source)
            .unwrap()
            .unwrap_or_else(|| source.into());
        let again = format_markdown(&formatted)
            .unwrap()
            .unwrap_or_else(|| formatted.clone());
        assert_eq!(formatted, again);
    }
}
