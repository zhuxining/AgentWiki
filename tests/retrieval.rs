use agentwiki::{AgentWiki, ContextQuery, KeywordMode, OpenOptions};

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
    std::fs::write(wiki.path().join("AGENTWIKI.md"), "---\n---\n").unwrap();
    std::fs::write(
        wiki.path().join("guide.md"),
        "---\ntags: [rust]\ntype: guide\nsummary: Refresh token rotation guide.\n---\n\nRefresh token rotation\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "refresh token".into(),
            tags: vec!["rust".into()],
            note_types: vec!["guide".into()],
            fragment_limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.fragments.len(), 1);
    assert_eq!(result.fragments[0].filename, "guide");
    assert_eq!(result.documents.len(), 1);
}

#[tokio::test]
async fn empty_query_returns_recent_documents() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(
        wiki.path().join("recent.md"),
        "---\ntype: note\ntags: [recent]\nsummary: Latest note.\n---\n\nLatest note\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            document_limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.documents[0].slice.path.0.as_str(), "recent.md");
    assert!(result.documents[0].sources.contains(&"recency".to_string()));
}

#[tokio::test]
async fn filters_are_applied_before_the_candidate_limit() {
    let wiki = tempfile::tempdir().unwrap();
    for index in 0..60 {
        std::fs::write(
            wiki.path().join(format!("noise-{index}.md")),
            format!(
                "---\ntype: note\ntags: [noise]\nsummary: needle noise {index}\n---\n\nneedle noise\n"
            ),
        )
        .unwrap();
    }
    std::fs::write(
        wiki.path().join("wanted.md"),
        "---\ntype: guide\ntags: [rust]\nsummary: needle wanted\n---\n\nneedle wanted\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "needle".into(),
            note_types: vec!["guide".into()],
            tags: vec!["rust".into()],
            document_limit: 1,
            fragment_limit: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.documents[0].slice.path.0.as_str(), "wanted.md");
    assert_eq!(result.fragments[0].slice.path.0.as_str(), "wanted.md");
}

#[tokio::test]
async fn explicit_keywords_support_all_mode_and_alias_exact_match() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(
        wiki.path().join("filter.md"),
        "---\ntype: case\ntags: [storage]\nsummary: Lance filtering\naliases: [filter incident]\n---\n\nprefilter preserves top-k candidates\n",
    )
    .unwrap();
    std::fs::write(
        wiki.path().join("other.md"),
        "---\ntype: case\ntags: [storage]\nsummary: Other filtering\n---\n\nprefilter only\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let keyword_result = app
        .query(ContextQuery {
            keywords: vec!["prefilter".into(), "candidates".into()],
            keyword_mode: KeywordMode::All,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(keyword_result.fragments.len(), 1);
    assert_eq!(
        keyword_result.fragments[0].slice.path.0.as_str(),
        "filter.md"
    );

    let any_result = app
        .query(ContextQuery {
            keywords: vec!["candidates".into(), "only".into()],
            keyword_mode: KeywordMode::Any,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(any_result.fragments.len(), 2);

    let exact_result = app
        .query(ContextQuery {
            query: "filter incident".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        exact_result.documents[0]
            .sources
            .contains(&"exact".to_string())
    );
}

#[tokio::test]
async fn empty_query_honors_structured_filters() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(
        wiki.path().join("note.md"),
        "---\ntype: note\ntags: [general]\nsummary: General\n---\n\nGeneral\n",
    )
    .unwrap();
    std::fs::write(
        wiki.path().join("case.md"),
        "---\ntype: case\ntags: [storage]\nsummary: Storage case\n---\n\nCase\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            note_types: vec!["case".into()],
            tags: vec!["storage".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].slice.path.0.as_str(), "case.md");
}

#[tokio::test]
async fn relations_are_queried_from_lance() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(
        wiki.path().join("source.md"),
        "---\ntype: note\ntags: [graph]\nsummary: Source\n---\n\n# Links\n\nSee [[target.md]].\n",
    )
    .unwrap();
    std::fs::write(
        wiki.path().join("target.md"),
        "---\ntype: note\ntags: [graph]\nsummary: Target\n---\n\nTarget body.\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "Source".into(),
            include_relations: true,
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.related.len(), 1);
    assert_eq!(result.related[0].path.0.as_str(), "target.md");
    assert_eq!(format!("{:?}", result.related[0].status), "Resolved");
}
