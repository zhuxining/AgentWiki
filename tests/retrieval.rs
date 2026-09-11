use agentwiki::model::{ContextQuery, PathScope, Slice};
use agentwiki::retrieval::LanceIndex;
use tempfile::tempdir;

#[test]
fn lancedb_indexes_and_queries_chunks() {
    let dir = tempdir().unwrap();
    let index = LanceIndex::open(camino::Utf8Path::from_path(dir.path()).unwrap(), None).unwrap();
    let path = PathScope("notes/auth.md".into());
    let slice = Slice {
        path: path.clone(),
        chunk_id: "chunk-1".into(),
        ordinal: 0,
        section: "Tokens".into(),
        content: "refresh token rotation policy".into(),
        source_hash: "hash".into(),
    };
    index.replace_slices(&path, &[slice]).unwrap();
    let result = index
        .search(
            &ContextQuery {
                query: "refresh token".into(),
                limit: 10,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].slice.path.0.as_str(), "notes/auth.md");
}

#[test]
fn lancedb_supports_optional_vector_projection() {
    let dir = tempfile::tempdir().unwrap();
    let index =
        LanceIndex::open(camino::Utf8Path::from_path(dir.path()).unwrap(), Some(2)).unwrap();
    let path = agentwiki::model::PathScope("notes/vector.md".into());
    let slices = vec![agentwiki::model::Slice {
        path: path.clone(),
        chunk_id: "v1".into(),
        ordinal: 0,
        section: "".into(),
        content: "semantic evidence".into(),
        source_hash: "hash".into(),
    }];
    index
        .replace_vectors(&path, &slices, &[vec![1.0, 0.0]])
        .unwrap();
    let query = agentwiki::model::ContextQuery {
        query: "semantic".into(),
        limit: 5,
        ..Default::default()
    };
    let hits = index.vector_search(&query, &[1.0, 0.0]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].sources, vec!["semantic"]);
}

#[test]
fn runtime_applies_frontmatter_filters() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(wiki.path().join("AGENTWIKI.md"), "---\nversion: 1\n---\n").unwrap();
    std::fs::write(
        wiki.path().join("guide.md"),
        "---\ntitle: Guide\ntags: [rust]\ntype: guide\n---\n\nRefresh token rotation\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(wiki.path()).unwrap();
    let index = camino::Utf8Path::from_path(projection.path()).unwrap();
    let mut runtime = agentwiki::Runtime::assemble(root, index).unwrap();
    runtime.ensure_fresh().unwrap();
    let query = agentwiki::model::ContextQuery {
        query: "refresh token".into(),
        tags: vec!["rust".into()],
        note_types: vec!["guide".into()],
        limit: 5,
        ..Default::default()
    };
    let result = runtime.query(&query).unwrap();
    assert_eq!(result.slices.len(), 1);
}

#[test]
fn runtime_returns_recent_documents_for_empty_query() {
    let wiki = tempfile::tempdir().unwrap();
    std::fs::write(wiki.path().join("AGENTWIKI.md"), "---\nversion: 1\n---\n").unwrap();
    std::fs::write(
        wiki.path().join("recent.md"),
        "---\ntitle: Recent\n---\n\nLatest note\n",
    )
    .unwrap();
    let projection = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(wiki.path()).unwrap();
    let index = camino::Utf8Path::from_path(projection.path()).unwrap();
    let mut runtime = agentwiki::Runtime::assemble(root, index).unwrap();
    runtime.ensure_fresh().unwrap();
    let result = runtime
        .query(&agentwiki::model::ContextQuery {
            limit: 5,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.slices[0].slice.path.0.as_str(), "recent.md");
    assert!(result.slices[0].sources.contains(&"recency".to_string()));
}
