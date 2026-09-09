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


class AlternateEmbeddingProvider(FakeEmbeddingProvider):
    model_name = "alternate"


class FailingIndex:
    async def upsert(self, note: Note, *, updated_at: float) -> None:
        raise RuntimeError("index unavailable")


@pytest.fixture
async def service(tmp_path):
    index = SQLiteIndex(tmp_path / "index.sqlite3")
    await index.initialize()
    service = NoteService(MarkdownStore(tmp_path / "documents"), index)
    try:
        yield service
    finally:
        await index.close()


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


async def test_document_lifecycle_updates_index(service: NoteService) -> None:
    note = await service.write(
        "notes/today.md",
        "SQLite makes local search fast.",
        {"title": "Today", "tags": ["local", "sqlite"]},
    )
    assert note.title == "Today"
    assert service.read("notes/today.md").content == "SQLite makes local search fast."
    assert (await service.search("local"))[0].path.value == "notes/today.md"

    await service.update("notes/today.md", content="Markdown remains the source of truth.")
    assert service.read("notes/today.md").content == "Markdown remains the source of truth."
    assert (await service.search("source truth"))[0].path.value == "notes/today.md"

    await service.move("notes/today.md", "archive/today.md")
    assert service.read("archive/today.md").path.value == "archive/today.md"
    assert (await service.search("source truth"))[0].path.value == "archive/today.md"

    await service.delete("archive/today.md")
    assert await service.search("source truth") == []


async def test_markdown_remains_source_of_truth_when_index_update_fails(tmp_path) -> None:
    store = MarkdownStore(tmp_path / "documents")
    service = NoteService(store, cast(SQLiteIndex, FailingIndex()))

    with pytest.raises(RuntimeError, match="index unavailable"):
        await service.write("kept.md", "Markdown survives index failure")

    assert store.read(NotePath(value="kept.md")).content == "Markdown survives index failure"


async def test_rebuild_index_reconciles_external_markdown_changes(service: NoteService) -> None:
    await service.write("one.md", "alpha document")
    service.store.path_for(NotePath(value="two.md")).write_text(
        "---\ntitle: Two\n---\n\nbeta document\n"
    )

    assert await service.search("beta") == []
    assert await service.rebuild_index() == 2
    results = await service.search("beta")
    assert len(results) == 1
    assert results[0].path.value == "two.md"
    assert results[0].frontmatter == {"title": "Two"}


async def test_sync_index_updates_and_removes_only_changed_projection_rows(
    service: NoteService,
) -> None:
    await service.write("old.md", "old document")
    service.store.path_for(NotePath(value="new.md")).write_text(
        "new document",
        encoding="utf-8",
    )
    service.store.path_for(NotePath(value="old.md")).unlink()

    assert await service.sync_index() == 1
    assert await service.search("old") == []
    assert (await service.search("new"))[0].path.value == "new.md"


async def test_list_directory_supports_depth_and_file_glob(service: NoteService) -> None:
    await service.write("guides/one.md", "one")
    await service.write("guides/nested/two.md", "two")

    immediate = await service.list_directory("guides", depth=1)
    assert [entry.path for entry in immediate] == ["guides/nested", "guides/one.md"]
    markdown_files = await service.list_directory("guides", depth=3, file_name_glob="two.md")
    assert [entry.path for entry in markdown_files] == [
        "guides/nested",
        "guides/nested/two.md",
    ]


async def test_rebuild_index_does_not_index_invalid_or_external_symlink_documents(
    service: NoteService,
    tmp_path,
) -> None:
    await service.write("valid.md", "valid document")
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

    assert await service.rebuild_index() == 1
    assert (await service.search("valid"))[0].path.value == "valid.md"
    assert await service.search("outside") == []


async def test_semantic_and_hybrid_search_use_configured_provider(tmp_path) -> None:
    index = SQLiteIndex(tmp_path / "index.sqlite3", embedding_provider=FakeEmbeddingProvider())
    await index.initialize()
    service = NoteService(MarkdownStore(tmp_path / "documents"), index)
    try:
        await service.write("python.md", "Python local tooling")
        await service.write("sqlite.md", "SQLite keyword indexing")

        semantic = await service.search("python", mode="semantic")
        assert semantic[0].path.value == "python.md"
        hybrid = await service.search("python", mode="hybrid")
        assert hybrid[0].path.value == "python.md"
    finally:
        await index.close()


async def test_semantic_search_requires_query_text(service: NoteService) -> None:
    with pytest.raises(ValueError, match="non-empty text"):
        await service.search("", mode="semantic")


async def test_reference_style_identifiers_incremental_edits_and_filters(
    service: NoteService,
) -> None:
    note = await service.write(
        None,
        "intro\n\n## Details\n\nold",
        title="Reference Note",
        directory="guides",
        tags=["python", "local"],
        note_type="guide",
    )
    assert note.path.value == "guides/Reference-Note.md"
    assert service.read("Reference Note").path.value == note.path.value
    assert service.read("wiki://Reference Note").path.value == note.path.value

    await service.edit("Reference Note", operation="find_replace", content="new", find_text="old")
    await service.edit("Reference Note", operation="append", content="tail")
    assert "new" in service.read("guides/Reference-Note.md").content
    assert (await service.search("", tags=["python"], note_types=["guide"]))[
        0
    ].title == "Reference Note"


async def test_markdown_is_formatted_on_write(service: NoteService) -> None:
    await service.write("format.md", "# Heading\n\n-   item")
    assert service.read("format.md").content == "# Heading\n\n- item"


async def test_edit_append_creates_and_search_supports_reference_modes(
    service: NoteService,
) -> None:
    created = await service.edit("new note", operation="append", content="created")
    assert created.path.value == "new-note.md"
    await service.write("docs/guide.md", "SQLite guide", {"title": "Guide"})

    assert (await service.search("Guide", mode="title"))[0].path.value == "docs/guide.md"
    assert (await service.search("docs/guide", mode="permalink"))[0].path.value == "docs/guide.md"
    assert await service.search("tag:local", tags=["local"]) == []


async def test_title_search_treats_like_wildcards_as_literal(service: NoteService) -> None:
    await service.write("percent.md", "one", {"title": "100% Guide"})
    await service.write("underscore.md", "two", {"title": "100X Guide"})

    results = await service.search("100%", mode="title")

    assert [result.path.value for result in results] == ["percent.md"]


async def test_directory_move_updates_index_and_read_ranges(service: NoteService) -> None:
    await service.write("drafts/a.md", "line one\nline two", {"title": "A"})
    await service.move("drafts", "archive", is_directory=True)
    assert (await service.search("line two"))[0].path.value == "archive/a.md"
    note, content = service.read_text("A", include_frontmatter=True, start_line=1, end_line=3)
    assert note.path.value == "archive/a.md"
    assert "title: A" in content


async def test_directory_move_rejects_a_target_inside_the_source(
    service: NoteService,
) -> None:
    await service.write("drafts/a.md", "content")

    with pytest.raises(ValueError, match="inside the source"):
        await service.move("drafts", "drafts/archive", is_directory=True)


async def test_directory_root_cannot_be_deleted_or_moved(service: NoteService) -> None:
    await service.write("root.md", "content")

    with pytest.raises(ValueError, match="document root"):
        await service.delete("", is_directory=True)
    with pytest.raises(ValueError, match="document root"):
        await service.move("", "archive", is_directory=True)


async def test_write_merges_frontmatter_supplied_inside_content(service: NoteService) -> None:
    note = await service.write(
        "embedded.md",
        "---\ntitle: Embedded\ntype: guide\n---\n\n# Body",
        title="Ignored by content",
        note_type="note",
    )
    assert note.title == "Embedded"
    assert note.frontmatter["type"] == "guide"
    assert note.content == "# Body"


async def test_write_strips_an_empty_frontmatter_block(service: NoteService) -> None:
    note = await service.write("empty-frontmatter.md", "---\n---\n\n# Body")

    assert note.content == "# Body"
    assert note.frontmatter == {"type": "note"}


async def test_section_edit_supports_nested_paths_and_duplicate_headers(
    service: NoteService,
) -> None:
    await service.write(
        "sections.md",
        "# Root\n\n## Details\n\nfirst\n\n### Child\n\nkeep\n\n## Details\n\nsecond",
    )
    await service.edit(
        "sections.md",
        operation="replace_section",
        section="Root/Details[1]",
        content="replacement",
    )
    body = service.read("sections.md").content
    assert "first" in body
    assert "replacement" in body
    assert "second" not in body


async def test_sqlite_metadata_filters_support_nested_values_and_comparisons(
    service: NoteService,
) -> None:
    await service.write(
        "priority.md",
        "priority document",
        {"state": "active", "priority": 3, "owner": {"team": "local"}},
    )
    assert await service.search("", metadata_filters={"owner.team": "local"})
    assert await service.search("", metadata_filters={"priority": {"$gte": 3}})
    assert await service.search("", metadata_filters={"priority": {"$between": [1, 2]}}) == []


async def test_sqlite_invalidates_vectors_when_embedding_model_changes(tmp_path) -> None:
    db_path = tmp_path / "index.sqlite3"
    documents = MarkdownStore(tmp_path / "documents")
    first = SQLiteIndex(db_path, embedding_provider=FakeEmbeddingProvider())
    await first.initialize()
    try:
        await NoteService(documents, first).write("python.md", "Python")
        cursor = await first.connection.execute("SELECT COUNT(*) FROM document_vectors")
        row = await cursor.fetchone()
        assert row is not None
        assert row[0] == 1
    finally:
        await first.close()

    second = SQLiteIndex(db_path, embedding_provider=AlternateEmbeddingProvider())
    await second.initialize()
    try:
        cursor = await second.connection.execute("SELECT COUNT(*) FROM document_vectors")
        row = await cursor.fetchone()
        assert row is not None
        assert row[0] == 0
    finally:
        await second.close()
