use camino::{Utf8Path, Utf8PathBuf};

use super::types::PathScope;
use crate::error::{AgentWikiError, Result};

pub fn scope_path(root: &Utf8Path, path: &Utf8Path) -> Result<PathScope> {
    if !root.is_absolute() {
        return Err(AgentWikiError::Config(
            "wiki root must be an absolute path".into(),
        ));
    }
    let joined = root.join(path);
    if path
        .components()
        .any(|component| matches!(component, camino::Utf8Component::ParentDir))
        || path.is_absolute()
    {
        return Err(AgentWikiError::PathOutsideRoot(path.to_path_buf()));
    }
    match (root.canonicalize(), joined.canonicalize()) {
        (Ok(root), Ok(joined)) if joined.starts_with(&root) => {}
        (Ok(_), Ok(_)) => return Err(AgentWikiError::PathOutsideRoot(path.to_path_buf())),
        _ => {}
    }
    for ancestor in joined
        .ancestors()
        .skip(1)
        .take_while(|ancestor| root.exists() && ancestor.starts_with(root))
    {
        if let Ok(actual) = ancestor.canonicalize() {
            let canonical_root = root.canonicalize().map_err(|source| AgentWikiError::Io {
                path: root.to_path_buf(),
                source,
            })?;
            if !actual.starts_with(canonical_root) {
                return Err(AgentWikiError::PathOutsideRoot(path.to_path_buf()));
            }
            break;
        }
    }
    Ok(PathScope(path.to_path_buf()))
}

pub fn snapshot(root: &Utf8Path) -> Result<Vec<PathScope>> {
    let mut paths = Vec::new();
    collect_markdown(root, root, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn collect_markdown(
    root: &Utf8Path,
    directory: &Utf8Path,
    paths: &mut Vec<PathScope>,
) -> Result<()> {
    for entry in std::fs::read_dir(directory).map_err(|source| AgentWikiError::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| AgentWikiError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());
        let Some(relative) = Utf8PathBuf::from_path_buf(relative.to_path_buf()).ok() else {
            continue;
        };
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            if relative
                .as_str()
                .split(std::path::MAIN_SEPARATOR)
                .any(|component| component.starts_with('.') && component != ".")
            {
                continue;
            }
            collect_markdown(root, Utf8Path::from_path(&path).unwrap(), paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == std::ffi::OsStr::new("md"))
            && relative.as_str() != "AGENTWIKI.md"
        {
            paths.push(PathScope(relative));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_escape() {
        let root = Utf8PathBuf::from("/wiki");
        assert!(scope_path(&root, Utf8Path::new("../etc/passwd")).is_err());
        assert!(scope_path(&root, Utf8Path::new("a/../../../x.md")).is_err());
    }

    #[test]
    fn snapshot_excludes_hidden_dirs_and_rule_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".obsidian/plugins")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("AGENTWIKI.md"), "---\n---").unwrap();
        std::fs::write(root.join("a.md"), "# a").unwrap();
        std::fs::write(root.join("notes/b.md"), "# b").unwrap();
        std::fs::write(root.join(".obsidian/plugins/c.md"), "# c").unwrap();
        let paths = snapshot(&root).unwrap();
        assert_eq!(
            paths.iter().map(|path| path.0.as_str()).collect::<Vec<_>>(),
            vec!["a.md", "notes/b.md"]
        );
    }
}
