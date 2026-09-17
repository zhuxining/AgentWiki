use agentwiki::{AgentWiki, ContextQuery, OpenOptions};

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
async fn query_applies_frontmatter_filters() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(wiki.path().join("AGENTWIKI.md"), "---\nversion: 1\n---\n").unwrap();
    std::fs::write(
        wiki.path().join("guide.md"),
        "---\ntitle: Guide\ntags: [rust]\ntype: guide\n---\n\nRefresh token rotation\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "refresh token".into(),
            tags: vec!["rust".into()],
            note_types: vec!["guide".into()],
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.slices.len(), 1);
    assert_eq!(result.slices[0].title, "Guide");
}

#[tokio::test]
async fn empty_query_returns_recent_documents() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(
        wiki.path().join("recent.md"),
        "---\ntitle: Recent\n---\n\nLatest note\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.slices[0].slice.path.0.as_str(), "recent.md");
    assert!(result.slices[0].sources.contains(&"recency".to_string()));
}
