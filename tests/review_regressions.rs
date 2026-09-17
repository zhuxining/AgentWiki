use agentwiki::{AgentWiki, OpenOptions, PathScope, ValidationRequest, ValidationScope};
use camino::Utf8Path;

async fn open(wiki: &tempfile::TempDir, projection: &tempfile::TempDir) -> AgentWiki {
    AgentWiki::open(OpenOptions {
        wiki_root: camino::Utf8PathBuf::from_path_buf(wiki.path().to_path_buf()).unwrap(),
        projection_dir: camino::Utf8PathBuf::from_path_buf(projection.path().to_path_buf())
            .unwrap(),
        embedding_model: None,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn explicit_formatting_preserves_yaml_and_is_idempotent() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    let prefix = "---\n# comment\ntitle: 'Title'\ntags: [one,two]\n---\n";
    let code = "```rust\nlet   x=  1;\n```";
    let file = wiki.path().join("a.md");
    std::fs::write(&file, format!("{prefix}\n# Title\n\n{code}\n\n-  item\n")).unwrap();
    let app = open(&wiki, &projection).await;
    let request = || ValidationRequest {
        scope: ValidationScope::Document(PathScope("a.md".into())),
        fix_format: true,
    };
    app.validate(request()).await.unwrap();
    let result = std::fs::read_to_string(&file).unwrap();
    assert!(result.starts_with(prefix));
    assert!(result.contains(code));
    assert!(
        app.validate(request())
            .await
            .unwrap()
            .formatted_paths
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn validation_never_follows_external_symlinks() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("secret.md");
    std::fs::write(&file, "-  secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), wiki.path().join("escape")).unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .validate(ValidationRequest {
            scope: ValidationScope::Document(PathScope("escape/secret.md".into())),
            fix_format: true,
        })
        .await;
    assert!(result.is_err());
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
