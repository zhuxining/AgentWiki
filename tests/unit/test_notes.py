from collections.abc import Sequence
from typing import cast

import pytest

from agentwiki.domain.models import Note, NotePath, SearchMode, SearchQuery
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


class FailingIndex:
    def upsert(self, note: Note, *, updated_at: float) -> None:
        raise RuntimeError("index unavailable")


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


def test_search_query_validates_mode_and_limit() -> None:
    with pytest.raises(ValueError):
        SearchQuery(text="query", mode=cast(SearchMode, "unknown"))
    with pytest.raises(ValueError):
        SearchQuery(text="query", limit=0)
    with pytest.raises(ValueError):
        SearchQuery(text="query", limit=101)


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


def test_markdown_remains_source_of_truth_when_index_update_fails(tmp_path) -> None:
    store = MarkdownStore(tmp_path / "documents")
    service = NoteService(store, cast(SQLiteIndex, FailingIndex()))

    with pytest.raises(RuntimeError, match="index unavailable"):
        service.write("kept.md", "Markdown survives index failure")

    assert store.read(NotePath(value="kept.md")).content == "Markdown survives index failure"


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


def test_rebuild_index_does_not_index_invalid_or_external_symlink_documents(
    service: NoteService,
    tmp_path,
) -> None:
    service.write("valid.md", "valid document")
    service.store.path_for(NotePath(value="broken.md")).write_text(
        "---\n- frontmatter must be a mapping\n---\n\nbroken document\n"
    )
    outside = tmp_path / "outside.md"
    outside.write_text("outside document")
    external_link = service.store.root / "external.md"
    try:
        external_link.symlink_to(outside)
    except OSError as exc:
        pytest.skip(f"symbolic links are unavailable: {exc}")

    assert service.rebuild_index() == 1
    assert service.search("valid")[0].path.value == "valid.md"
    assert service.search("outside") == []


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


def test_reference_style_identifiers_incremental_edits_and_filters(service: NoteService) -> None:
    note = service.write(
        None,
        "intro\n\n## Details\n\nold",
        title="Reference Note",
        directory="guides",
        tags=["python", "local"],
        note_type="guide",
    )
    assert note.path.value == "guides/Reference-Note.md"
    assert service.read("Reference Note").path.value == note.path.value

    service.edit("Reference Note", operation="find_replace", content="new", find_text="old")
    service.edit("Reference Note", operation="append", content="tail")
    assert "new" in service.read("guides/Reference-Note.md").content
    assert service.search("", tags=["python"], note_types=["guide"])[0].title == "Reference Note"


def test_markdown_is_formatted_on_write(service: NoteService) -> None:
    service.write("format.md", "# Heading\n\n-   item")
    assert service.read("format.md").content == "# Heading\n\n- item"


def test_edit_append_creates_and_search_supports_reference_modes(service: NoteService) -> None:
    created = service.edit("new note", operation="append", content="created")
    assert created.path.value == "new-note.md"
    service.write("docs/guide.md", "SQLite guide", {"title": "Guide"})

    assert service.search("Guide", mode="title")[0].path.value == "docs/guide.md"
    assert service.search("docs/guide", mode="permalink")[0].path.value == "docs/guide.md"
    assert service.search("tag:local", tags=["local"]) == []


def test_directory_move_updates_index_and_read_ranges(service: NoteService) -> None:
    service.write("drafts/a.md", "line one\nline two", {"title": "A"})
    service.move("drafts", "archive", is_directory=True)
    assert service.search("line two")[0].path.value == "archive/a.md"
    note, content = service.read_text(
        "A", include_frontmatter=True, start_line=1, end_line=3
    )
    assert note.path.value == "archive/a.md"
    assert "title: A" in content


def test_write_merges_frontmatter_supplied_inside_content(service: NoteService) -> None:
    note = service.write(
        "embedded.md",
        "---\ntitle: Embedded\ntype: guide\n---\n\n# Body",
        title="Ignored by content",
        note_type="note",
    )
    assert note.title == "Embedded"
    assert note.frontmatter["type"] == "guide"
    assert note.content == "# Body"


def test_section_edit_supports_nested_paths_and_duplicate_headers(service: NoteService) -> None:
    service.write(
        "sections.md",
        "# Root\n\n## Details\n\nfirst\n\n### Child\n\nkeep\n\n## Details\n\nsecond",
    )
    service.edit(
        "sections.md",
        operation="replace_section",
        section="Root/Details[1]",
        content="replacement",
    )
    body = service.read("sections.md").content
    assert "first" in body
    assert "replacement" in body
    assert "second" not in body
