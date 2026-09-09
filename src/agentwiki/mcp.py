"""FastMCP protocol adapter for the shared document services."""

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import cast

from fastmcp import Context, FastMCP
from fastmcp.server.lifespan import lifespan

from agentwiki.config import Settings
from agentwiki.domain.models import Frontmatter, SearchMode
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.services.notes import NoteService, create_service

_SERVICE_CONTEXT_KEY = "agentwiki.service"


@lifespan
async def _lifespan(_server: FastMCP) -> AsyncIterator[dict[str, NoteService]]:
    """Create one service and SQLite connection for the MCP server lifetime."""
    settings = Settings.from_env()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    service = await create_service(
        settings.document_root,
        settings.index_path,
        embedding_provider=provider,
    )
    try:
        yield {_SERVICE_CONTEXT_KEY: service}
    finally:
        await service.index.close()


mcp = FastMCP(
    "agentwiki",
    instructions=(
        "AgentWiki manages local Markdown documents. Use read/search/list tools to locate "
        "documents before editing; wiki:// resources expose read-only Markdown content."
    ),
    lifespan=_lifespan,
)


@asynccontextmanager
async def _service(ctx: Context | None = None) -> AsyncIterator[NoteService]:
    if ctx is not None and isinstance(ctx.lifespan_context, dict):
        service = ctx.lifespan_context.get(_SERVICE_CONTEXT_KEY)
        if isinstance(service, NoteService):
            yield service
            return

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


@mcp.resource(
    "wiki://{path*}",
    title="Markdown Note",
    description="Read a Markdown document from the local AgentWiki library.",
    mime_type="text/markdown",
    tags={"notes", "markdown"},
    annotations={
        "readOnlyHint": True,
        "idempotentHint": True,
    },
)
async def read_note_resource(path: str, ctx: Context) -> str:
    """Expose the raw Markdown content through a wiki:// resource URI."""
    identifier = f"wiki://{path}"
    async with _service(ctx) as service:
        _, content = await asyncio.to_thread(
            service.read_text,
            identifier,
            include_frontmatter=True,
        )
    return content


@mcp.tool(
    title="Write Note",
    description="Create a Markdown document and update its local SQLite index.",
    tags={"notes"},
    annotations={
        "title": "Write Note",
        "readOnlyHint": False,
        "destructiveHint": True,
        "idempotentHint": False,
        "openWorldHint": False,
    },
)
async def write_note(
    title: str,
    content: str,
    directory: str = "",
    tags: list[str] | None = None,
    note_type: str = "note",
    metadata: Frontmatter | None = None,
    overwrite: bool = False,
    path: str | None = None,
    *,
    ctx: Context,
) -> dict[str, object]:
    """Create a Markdown document and add it to the local index.

    ``path`` is an optional compatibility override; normally ``title`` and
    ``directory`` determine the Markdown filename.
    """
    async with _service(ctx) as service:
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


@mcp.tool(
    title="Read Note",
    description="Read a Markdown document by path, title, or wiki:// identifier.",
    tags={"notes", "navigation"},
    annotations={
        "title": "Read Note",
        "readOnlyHint": True,
        "destructiveHint": False,
        "openWorldHint": False,
    },
)
async def read_note(
    identifier: str,
    include_frontmatter: bool = False,
    start_line: int | None = None,
    end_line: int | None = None,
    *,
    ctx: Context,
) -> dict[str, object]:
    """Read one Markdown document by path, permalink, or unique title."""
    async with _service(ctx) as service:
        note, content = await asyncio.to_thread(
            service.read_text,
            identifier,
            include_frontmatter=include_frontmatter,
            start_line=start_line,
            end_line=end_line,
        )
    return {"path": note.path.value, "content": content, "frontmatter": note.frontmatter}


@mcp.tool(
    title="Update Note",
    description="Replace the content or frontmatter of an existing Markdown document.",
    tags={"notes"},
    annotations={
        "title": "Update Note",
        "readOnlyHint": False,
        "destructiveHint": True,
        "idempotentHint": True,
        "openWorldHint": False,
    },
)
async def update_note(
    path: str,
    content: str | None = None,
    frontmatter: Frontmatter | None = None,
    *,
    ctx: Context,
) -> dict[str, object]:
    """Update a Markdown document and refresh its local index row."""
    async with _service(ctx) as service:
        note = await service.update(path, content=content, frontmatter=frontmatter)
    return {"path": note.path.value, "title": note.title}


@mcp.tool(
    title="List Directory",
    description="List Markdown files and directories with filtering and depth control.",
    tags={"navigation", "notes"},
    annotations={
        "title": "List Directory",
        "readOnlyHint": True,
        "destructiveHint": False,
        "openWorldHint": False,
    },
)
async def list_directory(
    directory: str = "",
    depth: int = 1,
    file_name_glob: str | None = None,
    page: int = 1,
    page_size: int = 20,
    *,
    ctx: Context,
) -> dict[str, object]:
    """List Markdown files and directories with bounded depth and pagination."""
    if page < 1:
        raise ValueError("page must be at least 1")
    if page_size < 1 or page_size > 200:
        raise ValueError("page_size must be between 1 and 200")
    async with _service(ctx) as service:
        entries = await service.list_directory(
            directory,
            depth=depth,
            file_name_glob=file_name_glob,
        )
    start = (page - 1) * page_size
    page_entries = entries[start : start + page_size]
    return {
        "directory": directory or ".",
        "entries": [entry.model_dump() for entry in page_entries],
        "page": page,
        "page_size": page_size,
        "total": len(entries),
        "has_more": start + page_size < len(entries),
    }


@mcp.tool(
    title="Edit Note",
    description="Apply an incremental append, replacement, or section edit to a Markdown document.",
    tags={"notes"},
    annotations={
        "title": "Edit Note",
        "readOnlyHint": False,
        "destructiveHint": True,
        "idempotentHint": False,
        "openWorldHint": False,
    },
)
async def edit_note(
    identifier: str,
    operation: str,
    content: str,
    find_text: str | None = None,
    section: str | None = None,
    expected_replacements: int = 1,
    replace_subsections: bool = True,
    metadata: Frontmatter | None = None,
    *,
    ctx: Context,
) -> dict[str, object]:
    """Apply an append, prepend, replacement, or section edit to a note."""
    async with _service(ctx) as service:
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


@mcp.tool(
    title="Delete Note",
    description="Delete a Markdown document or directory and remove its local index entries.",
    tags={"notes"},
    annotations={
        "title": "Delete Note",
        "readOnlyHint": False,
        "destructiveHint": True,
        "idempotentHint": False,
        "openWorldHint": False,
    },
)
async def delete_note(
    identifier: str,
    is_directory: bool = False,
    *,
    ctx: Context,
) -> dict[str, str]:
    """Delete one Markdown document and its local index row."""
    async with _service(ctx) as service:
        await service.delete(identifier, is_directory=is_directory)
    return {"path": identifier, "status": "deleted"}


@mcp.tool(
    title="Move Note",
    description="Move a Markdown document or directory within the local document library.",
    tags={"notes"},
    annotations={
        "title": "Move Note",
        "readOnlyHint": False,
        "destructiveHint": False,
        "idempotentHint": False,
        "openWorldHint": False,
    },
)
async def move_note(
    identifier: str,
    destination_path: str = "",
    destination_folder: str | None = None,
    is_directory: bool = False,
    *,
    ctx: Context,
) -> dict[str, str]:
    """Move one Markdown document within the local document library."""
    async with _service(ctx) as service:
        if destination_folder is not None and destination_path:
            raise ValueError("destination_path and destination_folder are mutually exclusive")
        landing = await service.move(
            identifier,
            destination_folder or destination_path,
            destination_folder=destination_folder is not None,
            is_directory=is_directory,
        )
    return {"source": identifier, "target": landing, "status": "moved"}


@mcp.tool(
    title="Search Notes",
    description="Search indexed Markdown documents by keyword, title, semantic, or hybrid mode.",
    tags={"search", "notes"},
    annotations={
        "title": "Search Notes",
        "readOnlyHint": True,
        "destructiveHint": False,
        "openWorldHint": False,
    },
)
async def search_notes(
    query: str = "",
    mode: str = "keyword",
    search_type: str | None = None,
    limit: int = 20,
    page: int = 1,
    tags: list[str] | None = None,
    note_types: list[str] | None = None,
    metadata_filters: Frontmatter | None = None,
    *,
    ctx: Context,
) -> list[dict[str, object]]:
    """Search indexed documents with keyword, semantic, or hybrid mode."""
    async with _service(ctx) as service:
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


@mcp.tool(
    title="Rebuild Index",
    description="Rebuild the local SQLite search index from Markdown documents.",
    tags={"indexing", "maintenance"},
    annotations={
        "title": "Rebuild Index",
        "readOnlyHint": False,
        "destructiveHint": False,
        "idempotentHint": True,
        "openWorldHint": False,
    },
)
async def rebuild_index(*, ctx: Context) -> dict[str, int]:
    """Rebuild the SQLite index from all Markdown files."""
    async with _service(ctx) as service:

        async def report_progress(current: int, total: int) -> None:
            await ctx.report_progress(
                current,
                total,
                message=f"Indexed {current}/{total} Markdown documents",
            )

        count = await service.rebuild_index(progress=report_progress)
        await ctx.info(f"Rebuilt Markdown index with {count} documents")
    return {"indexed": count}


def main() -> None:
    """Run the MCP server over the default transport."""
    mcp.run()
