use std::fs::{File, FileTimes};
use std::time::{Duration, UNIX_EPOCH};

use agentwiki::{AgentWiki, ContextQuery, KeywordMode, OpenOptions, SearchOrder, SearchResult};

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

fn write(wiki: &tempfile::TempDir, path: &str, content: &str) {
    let path = wiki.path().join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn set_modified(wiki: &tempfile::TempDir, path: &str, seconds: u64) -> i64 {
    let path = wiki.path().join(path);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
        .unwrap();
    i64::try_from(
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn assert_only_path(result: &SearchResult, path: &str) {
    assert!(result.degraded.is_empty(), "{:?}", result.degraded);
    assert_eq!(result.documents.len(), 1, "{:?}", result.documents);
    assert_eq!(result.fragments.len(), 1, "{:?}", result.fragments);
    assert_eq!(result.documents[0].slice.path.0.as_str(), path);
    assert_eq!(result.fragments[0].slice.path.0.as_str(), path);
}

async fn assert_literal_scope(scope: &str, outside: &str) {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    let wanted = format!("{scope}/wanted.md");
    for path in [&wanted, &format!("{outside}/outside.md")] {
        write(&wiki, path, "---\nsummary: scopeprobe\n---\n\nscopeprobe\n");
    }
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "scopeprobe".into(),
            scope: scope.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_only_path(&result, &wanted);
}

#[tokio::test]
async fn scope_underscore_is_a_literal_directory_character() {
    assert_literal_scope("proj_one", "projXone").await;
}

#[tokio::test]
async fn scope_percent_is_a_literal_directory_character() {
    assert_literal_scope("proj%one", "projExpandedone").await;
}

#[tokio::test]
async fn numeric_yaml_metadata_matches_numeric_json_filter() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    for (path, count) in [("wanted.md", 2), ("outside.md", 3)] {
        write(
            &wiki,
            path,
            &format!("---\nsummary: countprobe\ncount: {count}\n---\n\ncountprobe\n"),
        );
    }
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "countprobe".into(),
            metadata_filters: serde_json::from_value(serde_json::json!({ "count": 2 })).unwrap(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_only_path(&result, "wanted.md");
}

#[tokio::test]
async fn mtime_only_change_refreshes_document_and_fragment_dates_and_filters() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    let content = "---\nsummary: clockprobe\n---\n\nclockprobe\n";
    write(&wiki, "clock.md", content);
    let old = set_modified(&wiki, "clock.md", 1_700_000_000);
    let app = open(&wiki, &projection).await;
    let before = app
        .query(ContextQuery {
            query: "clockprobe".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_only_path(&before, "clock.md");
    for hit in before.documents.iter().chain(&before.fragments) {
        assert_eq!(hit.modified_at_ns, old);
        assert_eq!(hit.slice.modified_at_ns, old);
    }

    let new = set_modified(&wiki, "clock.md", 1_700_000_100);
    assert!(new > old);
    assert_eq!(
        std::fs::read_to_string(wiki.path().join("clock.md")).unwrap(),
        content
    );
    for modified_after_ns in [None, Some(old + (new - old) / 2)] {
        let result = app
            .query(ContextQuery {
                query: "clockprobe".into(),
                modified_after_ns,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_only_path(&result, "clock.md");
        for hit in result.documents.iter().chain(&result.fragments) {
            assert_eq!(hit.modified_at_ns, new);
            assert_eq!(hit.slice.modified_at_ns, new);
        }
    }
}

#[tokio::test]
async fn all_keywords_cannot_be_bypassed_by_exact_or_natural_query_matches() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    for (path, text) in [
        ("wanted.md", "bypass required second"),
        ("bypass.md", "bypass required"),
        ("natural.md", "bypass"),
        ("partial.md", "bypass second"),
    ] {
        write(
            &wiki,
            path,
            &format!("---\nsummary: {text}\n---\n\n{text}\n"),
        );
    }
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "bypass".into(),
            keywords: vec!["required".into(), "second".into()],
            keyword_mode: KeywordMode::All,
            document_limit: 20,
            fragment_limit: 20,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_only_path(&result, "wanted.md");
}

#[tokio::test]
async fn modified_desc_selects_latest_match_before_relevance_candidate_truncation() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    for index in 0..30 {
        let path = format!("old-{index}.md");
        let text = "chronoprobe ".repeat(12);
        write(
            &wiki,
            &path,
            &format!("---\nsummary: {text}\n---\n\n{text}\n"),
        );
        set_modified(&wiki, &path, 1_700_000_000 + index);
    }
    // One occurrence in a longer document makes the latest lexical match less relevant.
    let text = format!("chronoprobe {}", "background ".repeat(100));
    write(
        &wiki,
        "latest.md",
        &format!("---\nsummary: {text}\n---\n\n{text}\n"),
    );
    set_modified(&wiki, "latest.md", 1_700_001_000);
    write(
        &wiki,
        "unrelated.md",
        "---\nsummary: unrelated\n---\n\nunrelated\n",
    );
    set_modified(&wiki, "unrelated.md", 1_700_002_000);

    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "chronoprobe".into(),
            document_limit: 1,
            order: SearchOrder::ModifiedDesc,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(result.degraded.is_empty(), "{:?}", result.degraded);
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].slice.path.0.as_str(), "latest.md");
    assert!(
        result.documents[0]
            .sources
            .iter()
            .all(|source| source != "semantic")
    );
}

#[tokio::test]
async fn document_search_includes_earlier_sibling_headings() {
    let wiki = tempfile::tempdir().unwrap();
    let projection = tempfile::tempdir().unwrap();
    write(
        &wiki,
        "outline.md",
        "---\nsummary: Outline\n---\n\n# Earlierneedle\n\nFirst evidence.\n\n# Later\n\nLast evidence.\n",
    );
    let app = open(&wiki, &projection).await;
    let result = app
        .query(ContextQuery {
            query: "Earlierneedle".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_only_path(&result, "outline.md");
}
