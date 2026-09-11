use agentwiki::{PathScope, Runtime};
use camino::Utf8Path;

#[test]
fn parser_keeps_code_headings_inside_their_section() {
    let slices = agentwiki::document::chunk_document(
        "",
        "## Parent\n\n```sh\n# shell comment\n```\n\ntext\n\nSibling\n=======\n\nnext\n",
        &PathScope("a.md".into()),
    );
    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0].section, "Parent");
    assert!(slices[0].content.contains("# shell comment"));
    assert_eq!(slices[1].section, "Sibling");
}

#[test]
fn graph_resolves_dot_segments_and_rejects_external_targets() {
    let (edges, warnings) = agentwiki::graph::extract_edges(
        &PathScope("dir/a.md".into()),
        &Default::default(),
        "[safe](sub/../b.md) [root](/root.md) [external](https://example.com/a.md) [escape](../../secret.md)",
    );
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().any(|e| e.to.0 == "dir/b.md"));
    assert!(edges.iter().any(|e| e.to.0 == "root.md"));
    assert_eq!(warnings.len(), 1);
}

#[test]
fn explicit_formatting_preserves_yaml_and_is_idempotent() {
    let wiki = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let prefix = "---\n# comment\ntitle: 'Title'\ntags: [one,two]\n---\n";
    let code = "```rust\nlet   x=  1;\n```";
    let file = wiki.path().join("a.md");
    std::fs::write(&file, format!("{prefix}\n# Title\n\n{code}\n\n-  item\n")).unwrap();
    let runtime = Runtime::assemble(
        Utf8Path::from_path(wiki.path()).unwrap(),
        Utf8Path::from_path(index.path()).unwrap(),
    )
    .unwrap();
    assert!(runtime.validate_with_format(None, false, true).is_err());
    assert!(
        runtime
            .validate_with_format(Some(&PathScope("AGENTWIKI.md".into())), false, true)
            .is_err()
    );
    let path = PathScope("a.md".into());
    runtime
        .validate_with_format(Some(&path), false, true)
        .unwrap();
    let result = std::fs::read_to_string(&file).unwrap();
    assert!(result.starts_with(prefix));
    assert!(result.contains(code));
    assert!(
        runtime
            .validate_with_format(Some(&path), false, true)
            .unwrap()
            .formatted_paths
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn validation_and_formatting_never_follow_external_symlinks() {
    let wiki = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("secret.md");
    std::fs::write(&file, "-  secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), wiki.path().join("escape")).unwrap();
    let root = Utf8Path::from_path(wiki.path()).unwrap();
    assert!(agentwiki::document::scope_path(root, Utf8Path::new("escape/missing.md")).is_err());
    assert!(
        agentwiki::governance::format::format_file(root, &PathScope("escape/secret.md".into()))
            .is_err()
    );
    assert!(
        agentwiki::governance::validate::validate_wiki(
            root,
            Some(&PathScope("escape/secret.md".into()))
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "-  secret\n");
}

#[test]
fn projection_namespace_follows_the_effective_canonical_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let cfg = agentwiki::config::AppConfig::load_from(root).unwrap();
    let a = root.join("wiki-a");
    let b = root.join("wiki-b");
    assert_ne!(
        cfg.projection_for_root(&a).unwrap(),
        cfg.projection_for_root(&b).unwrap()
    );
    assert_eq!(
        cfg.projection_for_root(&a).unwrap(),
        cfg.projection_for_root(&a.join(".")).unwrap()
    );
}
