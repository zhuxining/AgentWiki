import pytest

from agentwiki.domain.documents import DocumentPath, WikiDocument
from agentwiki.indexing.chunking import _MAX_CHARS, _split_oversized, chunk_document


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


@pytest.mark.parametrize(
    ("first", "second"),
    [(600, 1150), (150, 1200), (600, 1049), (1199, 1199), (1, _MAX_CHARS)],
)
def test_oversized_splitting_never_exceeds_the_bound(first: int, second: int) -> None:
    """The overlap budget must come out of the limit, not on top of it."""
    parts = _split_oversized(f"{'x' * first}\n\n{'y' * second}")

    assert all(len(part) <= _MAX_CHARS for part in parts)


def test_heading_without_body_does_not_emit_an_empty_chunk() -> None:
    chunks = chunk_document(_document("# Alpha\n\n## Beta\n\nbody text"))

    assert [(item.section, item.content) for item in chunks] == [
        ("Alpha / Beta", "body text")
    ]


def test_document_without_content_still_produces_one_chunk() -> None:
    chunks = chunk_document(_document("# A\n\n## B\n\n### C"))

    assert len(chunks) == 1
    assert chunks[0].content == ""
