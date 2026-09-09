from __future__ import annotations

from collections.abc import Sequence

import pytest

from agentwiki.domain.models import NotePath
from agentwiki.markdown.store import MarkdownStore
from agentwiki.repository.sqlite_index import SQLiteIndex
from agentwiki.services.notes import NoteService


class FakeEmbeddingProvider:
    model_name = "fake"

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        return [self._vector(text) for text in texts]

    def embed_query(self, text: str) -> list[float]:
        return self._vector(text)

    @staticmethod
    def _vector(text: str) -> list[float]:
        lowered = text.lower()
        return [float(lowered.count("python")), float(lowered.count("sqlite"))]


@pytest.fixture
def service(tmp_path):
    index = SQLiteIndex(tmp_path / "index.sqlite3")
    service = NoteService(MarkdownStore(tmp_path / "documents"), index)
    try:
        yield service
    finally:
        index.close()


def test_note_path_rejects_absolute_and_parent_paths() -> None:
    with pytest.raises(ValueError):
        NotePath(value="/outside.md")
    with pytest.raises(ValueError):
        NotePath(value="../outside.md")
    with pytest.raises(ValueError):
        NotePath(value="notes/today.txt")


def test_document_lifecycle_updates_index(service: NoteService) -> None:
    note = service.write(
        "notes/today.md",
        "SQLite makes local search fast.",
        {"title": "Today", "tags": ["local", "sqlite"]},
    )
    assert note.title == "Today"
    assert service.read("notes/today.md").content == "SQLite makes local search fast."
    assert service.search("local")[0].path.value == "notes/today.md"

    service.update("notes/today.md", content="Markdown remains the source of truth.")
    assert service.read("notes/today.md").content == "Markdown remains the source of truth."
    assert service.search("source truth")[0].path.value == "notes/today.md"

    service.move("notes/today.md", "archive/today.md")
    assert service.read("archive/today.md").path.value == "archive/today.md"
    assert service.search("source truth")[0].path.value == "archive/today.md"

    service.delete("archive/today.md")
    assert service.search("source truth") == []


def test_rebuild_index_reconciles_external_markdown_changes(service: NoteService) -> None:
    service.write("one.md", "alpha document")
    service.store.path_for(NotePath(value="two.md")).write_text(
        "---\ntitle: Two\n---\n\nbeta document\n"
    )

    assert service.search("beta") == []
    assert service.rebuild_index() == 2
    results = service.search("beta")
    assert len(results) == 1
    assert results[0].path.value == "two.md"
    assert results[0].frontmatter == {"title": "Two"}


def test_semantic_and_hybrid_search_use_configured_provider(tmp_path) -> None:
    index = SQLiteIndex(tmp_path / "index.sqlite3", embedding_provider=FakeEmbeddingProvider())
    service = NoteService(MarkdownStore(tmp_path / "documents"), index)
    try:
        service.write("python.md", "Python local tooling")
        service.write("sqlite.md", "SQLite keyword indexing")

        semantic = service.search("python", mode="semantic")
        assert semantic[0].path.value == "python.md"
        hybrid = service.search("python", mode="hybrid")
        assert hybrid[0].path.value == "python.md"
    finally:
        index.close()
