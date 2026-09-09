"""FastMCP protocol adapter for the shared document services."""

from collections.abc import Iterator
from contextlib import contextmanager
from typing import cast

from fastmcp import FastMCP

from agentwiki.config import Settings
from agentwiki.domain.models import Frontmatter, SearchMode
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.services.notes import NoteService, create_service

mcp = FastMCP("agentwiki")


@contextmanager
def _service() -> Iterator[NoteService]:
    settings = Settings.from_env()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    service = create_service(
        settings.document_root,
        settings.index_path,
        embedding_provider=provider,
    )
    try:
        yield service
    finally:
        service.index.close()


@mcp.tool
def write_note(
    content: str,
    path: str | None = None,
    title: str | None = None,
    directory: str = "",
    tags: list[str] | None = None,
    note_type: str = "note",
    metadata: Frontmatter | None = None,
    overwrite: bool = False,
) -> dict[str, object]:
    """Create a Markdown document and add it to the local index.

    ``path`` remains supported for callers that already use an explicit path;
    otherwise ``title`` and ``directory`` determine the Markdown filename.
    """
    with _service() as service:
        note = service.write(
            path,
            content,
            metadata,
            title=title,
            directory=directory,
            tags=tags,
            note_type=note_type,
            overwrite=overwrite,
        )
    return {"path": note.path.value, "title": note.title}


@mcp.tool
def read_note(identifier: str) -> dict[str, object]:
    """Read one Markdown document by path, permalink, or unique title."""
    with _service() as service:
        note = service.read(identifier)
    return {"path": note.path.value, "content": note.content, "frontmatter": note.frontmatter}


@mcp.tool
def update_note(
    path: str,
    content: str | None = None,
    frontmatter: Frontmatter | None = None,
) -> dict[str, object]:
    """Update a Markdown document and refresh its local index row."""
    with _service() as service:
        note = service.update(path, content=content, frontmatter=frontmatter)
    return {"path": note.path.value, "title": note.title}


@mcp.tool
def edit_note(
    identifier: str,
    operation: str,
    content: str,
    find_text: str | None = None,
    section: str | None = None,
    expected_replacements: int = 1,
    replace_subsections: bool = True,
    metadata: Frontmatter | None = None,
) -> dict[str, object]:
    """Apply an append, prepend, replacement, or section edit to a note."""
    with _service() as service:
        note = service.edit(
            identifier,
            operation=operation,
            content=content,
            find_text=find_text,
            section=section,
            expected_replacements=expected_replacements,
            replace_subsections=replace_subsections,
            metadata=metadata,
        )
    return {"path": note.path.value, "title": note.title}


@mcp.tool
def delete_note(identifier: str, is_directory: bool = False) -> dict[str, str]:
    """Delete one Markdown document and its local index row."""
    with _service() as service:
        service.delete(identifier, is_directory=is_directory)
    return {"path": identifier, "status": "deleted"}


@mcp.tool
def move_note(source: str, target: str, destination_folder: bool = False) -> dict[str, str]:
    """Move one Markdown document within the local document library."""
    with _service() as service:
        landing = service.move(source, target, destination_folder=destination_folder)
    return {"source": source, "target": landing, "status": "moved"}


@mcp.tool
def search_notes(
    query: str = "",
    mode: str = "keyword",
    limit: int = 20,
    page: int = 1,
    tags: list[str] | None = None,
    note_types: list[str] | None = None,
    metadata_filters: Frontmatter | None = None,
) -> list[dict[str, object]]:
    """Search indexed documents with keyword, semantic, or hybrid mode."""
    with _service() as service:
        results = service.search(
            query,
            mode=cast(SearchMode, mode),
            limit=limit,
            page=page,
            tags=tags,
            note_types=note_types,
            metadata_filters=metadata_filters,
        )
    return [
        {
            "path": result.path.value,
            "title": result.title,
            "score": result.score,
            "frontmatter": result.frontmatter,
            "snippet": result.snippet,
        }
        for result in results
    ]


@mcp.tool
def rebuild_index() -> dict[str, int]:
    """Rebuild the SQLite index from all Markdown files."""
    with _service() as service:
        count = service.rebuild_index()
    return {"indexed": count}


def main() -> None:
    """Run the MCP server over the default transport."""
    mcp.run()
