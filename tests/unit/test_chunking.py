from agentwiki.domain.documents import DocumentPath, WikiDocument
from agentwiki.indexing.chunking import chunk_document


def _document(content: str) -> WikiDocument:
    return WikiDocument(
        path=DocumentPath(value="guide.md"),
        content=content,
        modified_at_ns=1,
        size=len(content),
    )


def test_chunking_preserves_heading_hierarchy_and_stable_ids() -> None:
    document = _document("# Root\n\nIntro\n\n## Details\n\nEvidence")

    first = chunk_document(document)
    second = chunk_document(document)

    assert [item.section for item in first] == ["Root", "Root / Details"]
    assert [item.content for item in first] == ["Intro", "Evidence"]
    assert [item.chunk_id for item in first] == [item.chunk_id for item in second]


def test_chunking_bounds_oversized_unicode_sections() -> None:
    chunks = chunk_document(_document("# 长文\n\n" + "知" * 2_600))

    assert len(chunks) == 3
    assert all(len(item.content) <= 1_200 for item in chunks)
