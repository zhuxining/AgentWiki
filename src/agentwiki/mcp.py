"""FastMCP protocol adapter for the shared document services."""

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import cast

from fastmcp import FastMCP

from agentwiki.config import Settings
from agentwiki.domain.models import Frontmatter, SearchMode
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.services.notes import NoteService, create_service

mcp = FastMCP("agentwiki")


@asynccontextmanager
async def _service() -> AsyncIterator[NoteService]:
    settings = Settings.from_env()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    service = await create_service(
        settings.document_root,
        settings.index_path,
        embedding_provider=provider,
    )
    try:
        yield service
    finally:
        await service.index.close()


@mcp.tool
async def write_note(
    title: str,
    content: str,
    directory: str = "",
    tags: list[str] | None = None,
    note_type: str = "note",
    metadata: Frontmatter | None = None,
    overwrite: bool = False,
    path: str | None = None,
) -> dict[str, object]:
    """Create a Markdown document and add it to the local index.

    ``path`` is an optional compatibility override; normally ``title`` and
    ``directory`` determine the Markdown filename.
    """
    async with _service() as service:
        note = await service.write(
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
async def read_note(
    identifier: str,
    include_frontmatter: bool = False,
    start_line: int | None = None,
    end_line: int | None = None,
) -> dict[str, object]:
    """Read one Markdown document by path, permalink, or unique title."""
    async with _service() as service:
        note, content = service.read_text(
            identifier,
            include_frontmatter=include_frontmatter,
            start_line=start_line,
            end_line=end_line,
        )
    return {"path": note.path.value, "content": content, "frontmatter": note.frontmatter}


@mcp.tool
async def update_note(
    path: str,
    content: str | None = None,
    frontmatter: Frontmatter | None = None,
) -> dict[str, object]:
    """Update a Markdown document and refresh its local index row."""
    async with _service() as service:
        note = await service.update(path, content=content, frontmatter=frontmatter)
    return {"path": note.path.value, "title": note.title}


@mcp.tool
async def edit_note(
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
    async with _service() as service:
        note = await service.edit(
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
async def delete_note(identifier: str, is_directory: bool = False) -> dict[str, str]:
    """Delete one Markdown document and its local index row."""
    async with _service() as service:
        await service.delete(identifier, is_directory=is_directory)
    return {"path": identifier, "status": "deleted"}


@mcp.tool
async def move_note(
    identifier: str,
    destination_path: str = "",
    destination_folder: str | None = None,
    is_directory: bool = False,
) -> dict[str, str]:
    """Move one Markdown document within the local document library."""
    async with _service() as service:
        if destination_folder is not None and destination_path:
            raise ValueError("destination_path and destination_folder are mutually exclusive")
        landing = await service.move(
            identifier,
            destination_folder or destination_path,
            destination_folder=destination_folder is not None,
            is_directory=is_directory,
        )
    return {"source": identifier, "target": landing, "status": "moved"}


@mcp.tool
async def search_notes(
    query: str = "",
    mode: str = "keyword",
    search_type: str | None = None,
    limit: int = 20,
    page: int = 1,
    tags: list[str] | None = None,
    note_types: list[str] | None = None,
    metadata_filters: Frontmatter | None = None,
) -> list[dict[str, object]]:
    """Search indexed documents with keyword, semantic, or hybrid mode."""
    async with _service() as service:
        results = await service.search(
            query,
            mode=cast(SearchMode, search_type or mode),
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
async def rebuild_index() -> dict[str, int]:
    """Rebuild the SQLite index from all Markdown files."""
    async with _service() as service:
        count = await service.rebuild_index()
    return {"indexed": count}


def main() -> None:
    """Run the MCP server over the default transport."""
    mcp.run()
