use agentwiki::{ContextQuery, Runtime};
use camino::Utf8Path;

#[test]
fn rebuild_repopulates_an_unchanged_archive() {
    let wiki = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    std::fs::write(wiki.path().join("a.md"), "# Title\n\nrebuild evidence\n").unwrap();
    let mut runtime = Runtime::assemble(
        Utf8Path::from_path(wiki.path()).unwrap(),
        Utf8Path::from_path(index.path()).unwrap(),
    )
    .unwrap();
    assert_eq!(runtime.ensure_fresh().unwrap().indexed, 1);
    assert_eq!(runtime.rebuild().unwrap().indexed, 1);
    assert_eq!(
        runtime
            .query(&ContextQuery::default())
            .unwrap()
            .slices
            .len(),
        1
    );
}

#[test]
fn failed_document_preserves_evidence_and_retries_without_other_changes() {
    let wiki = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let file = wiki.path().join("a.md");
    std::fs::write(&file, "# Title\n\nretained evidence\n").unwrap();
    let mut runtime = Runtime::assemble(
        Utf8Path::from_path(wiki.path()).unwrap(),
        Utf8Path::from_path(index.path()).unwrap(),
    )
    .unwrap();
    runtime.ensure_fresh().unwrap();
    std::fs::write(&file, "---\ntitle: [broken\n---\n").unwrap();
    for _ in 0..2 {
        let report = runtime.ensure_fresh().unwrap();
        assert_eq!(report.removed, 0);
        assert!(report.degraded.iter().any(|e| e.contains("a.md")));
        let result = runtime.query(&ContextQuery::default()).unwrap();
        assert_eq!(result.slices.len(), 1);
        assert!(!result.degraded.is_empty());
    }
    std::fs::write(&file, "# Title\n\nrecovered evidence\n").unwrap();
    assert_eq!(runtime.ensure_fresh().unwrap().indexed, 1);
}

#[test]
fn no_answer_is_not_a_failure_and_queries_validate_scope() {
    let wiki = tempfile::tempdir().unwrap();
    let index = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::assemble(
        Utf8Path::from_path(wiki.path()).unwrap(),
        Utf8Path::from_path(index.path()).unwrap(),
    )
    .unwrap();
    let result = runtime
        .query(&ContextQuery {
            query: "absent".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.slices.is_empty());
    assert!(result.degraded.is_empty(), "{:?}", result.degraded);
    assert!(
        runtime
            .query(&ContextQuery {
                scope: "../outside".into(),
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn nested_heading_without_parent_does_not_panic() {
    let slices = agentwiki::document::chunk_document(
        "",
        "### Nested\n\nevidence\n",
        &agentwiki::PathScope("a.md".into()),
    );
    assert_eq!(slices[0].section, "Nested");
}
