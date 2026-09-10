from agentwiki.domain.documents import DocumentPath, WikiDocument
from agentwiki.indexing.graph import extract_edges, extract_graph


def test_graph_extraction_normalizes_relative_links_and_ignores_external_links() -> None:
    document = WikiDocument(
        path=DocumentPath(value="guides/source.md"),
        content=(
            "[[../decisions/plan]] [target](../notes/target.md#section) "
            "[external](https://example.com/doc.md)"
        ),
        modified_at_ns=1,
        size=1,
    )

    edges = extract_edges(document)

    assert [(edge.target_path, edge.source_kind) for edge in edges] == [
        ("decisions/plan.md", "wikilink"),
        ("notes/target.md", "markdown_link"),
    ]


def test_graph_extraction_reports_invalid_frontmatter_relations() -> None:
    document = WikiDocument(
        path=DocumentPath(value="source.md"),
        content="Source",
        frontmatter={
            "relations": [
                {"type": "depends_on", "target": "target.md"},
                {"type": "", "target": "other.md"},
                "invalid",
            ]
        },
        modified_at_ns=1,
        size=1,
    )

    extraction = extract_graph(document)

    assert len(extraction.edges) == 1
    assert extraction.warnings == (
        "relations[1] type must not be empty",
        "relations[2] must be a mapping",
    )
