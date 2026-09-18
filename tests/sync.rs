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
async fn rebuild_repopulates_an_unchanged_archive() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    std::fs::write(wiki.path().join("a.md"), "# Title\n\nrebuild evidence\n").unwrap();
    let app = open(&wiki, &projection).await;
    assert_eq!(app.sync().await.unwrap().indexed, 1);
    assert_eq!(app.rebuild().await.unwrap().indexed, 1);
    assert_eq!(
        app.query(ContextQuery::default())
            .await
            .unwrap()
            .documents
            .len(),
        1
    );
}

#[tokio::test]
async fn failed_document_preserves_evidence_and_retries() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    let file = wiki.path().join("a.md");
    std::fs::write(&file, "# Title\n\nretained evidence\n").unwrap();
    let app = open(&wiki, &projection).await;
    app.sync().await.unwrap();
    std::fs::write(&file, "---\ntitle: [broken\n---\n").unwrap();
    for _ in 0..2 {
        let report = app.sync().await.unwrap();
        assert_eq!(report.removed, 0);
        assert!(report.degraded.iter().any(|error| error.contains("a.md")));
        assert_eq!(
            app.query(ContextQuery::default())
                .await
                .unwrap()
                .documents
                .len(),
            1
        );
    }
    std::fs::write(&file, "# Title\n\nrecovered evidence\n").unwrap();
    assert_eq!(app.sync().await.unwrap().indexed, 1);
}

#[tokio::test]
async fn no_answer_is_not_a_failure_and_scope_is_validated() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "absent".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(result.documents.is_empty());
    assert!(result.fragments.is_empty());
    assert!(result.degraded.is_empty());
    assert!(
        app.query(ContextQuery {
            scope: "../outside".into(),
            ..Default::default()
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn rename_is_reported_as_a_move() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    std::fs::write(
        wiki.path().join("old.md"),
        "# Title\n\nmove evidence text\n",
    )
    .unwrap();
    let app = open(&wiki, &projection).await;
    assert_eq!(app.sync().await.unwrap().indexed, 1);
    std::fs::rename(wiki.path().join("old.md"), wiki.path().join("new.md")).unwrap();
    let report = app.sync().await.unwrap();
    assert_eq!(report.moved, 1);
    assert_eq!(report.removed, 1);
    let result = app
        .query(ContextQuery {
            query: "move evidence".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.fragments[0].slice.path.0.as_str(), "new.md");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_use_cases_are_serialized_without_deadlock() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    std::fs::write(wiki.path().join("a.md"), "# Title\n\nconcurrent evidence\n").unwrap();
    let app = std::sync::Arc::new(open(&wiki, &projection).await);
    let syncing = {
        let app = app.clone();
        tokio::spawn(async move { app.sync().await })
    };
    let querying = {
        let app = app.clone();
        tokio::spawn(async move {
            app.query(ContextQuery {
                query: "concurrent evidence".into(),
                ..Default::default()
            })
            .await
        })
    };
    syncing.await.unwrap().unwrap();
    assert_eq!(querying.await.unwrap().unwrap().fragments.len(), 1);
}
