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
    path: str,
    content: str,
    frontmatter: Frontmatter | None = None,
) -> dict[str, object]:
    """Create a Markdown document and add it to the local index."""
    with _service() as service:
        note = service.write(path, content, frontmatter)
    return {"path": note.path.value, "title": note.title}


@mcp.tool
def read_note(path: str) -> dict[str, object]:
    """Read one Markdown document by its relative path."""
    with _service() as service:
        note = service.read(path)
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
def delete_note(path: str) -> dict[str, str]:
    """Delete one Markdown document and its local index row."""
    with _service() as service:
        service.delete(path)
    return {"path": path, "status": "deleted"}


@mcp.tool
def move_note(source: str, target: str) -> dict[str, str]:
    """Move one Markdown document within the local document library."""
    with _service() as service:
        service.move(source, target)
    return {"source": source, "target": target, "status": "moved"}


@mcp.tool
def search_notes(text: str, mode: str = "keyword", limit: int = 20) -> list[dict[str, object]]:
    """Search indexed documents with keyword, semantic, or hybrid mode."""
    with _service() as service:
        results = service.search(text, mode=cast(SearchMode, mode), limit=limit)
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
