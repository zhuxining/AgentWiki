"""Typer CLI composition root for local document operations."""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import asdict
import json
from pathlib import Path
from typing import Any, cast

import typer

from agentwiki.config import Settings
from agentwiki.domain.models import SearchMode
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.runtime.watcher import watch_documents
from agentwiki.services.notes import NoteService, create_service

app = typer.Typer(no_args_is_help=True, help="Operate on a local Markdown document library.")


def _metadata(value: str | None) -> dict[str, Any]:
    if not value:
        return {}
    parsed = json.loads(value)
    if not isinstance(parsed, dict):
        raise typer.BadParameter("metadata must be a JSON object")
    return parsed


@contextmanager
def _service(root: Path | None, index: Path | None) -> Iterator[NoteService]:
    settings = Settings.from_env()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    service = create_service(
        root or settings.document_root,
        index or settings.index_path,
        embedding_provider=provider,
    )
    try:
        yield service
    finally:
        service.index.close()


@app.command("write-note")
def write_note(
    path: str,
    content: str = typer.Option("", help="Markdown body."),
    metadata: str | None = typer.Option(None, help="Frontmatter as a JSON object."),
    root: Path | None = typer.Option(None, help="Document library root."),
    index: Path | None = typer.Option(None, help="SQLite index path."),
) -> None:
    """Create a Markdown document and index it."""
    with _service(root, index) as service:
        note = service.write(path, content, _metadata(metadata))
    typer.echo(note.path.value)


@app.command("read-note")
def read_note(
    path: str,
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Read a Markdown document as JSON."""
    with _service(root, index) as service:
        note = service.read(path)
    typer.echo(
        json.dumps(
            {"path": note.path.value, "content": note.content, "frontmatter": note.frontmatter},
            ensure_ascii=False,
        )
    )


@app.command("update-note")
def update_note(
    path: str,
    content: str | None = typer.Option(None),
    metadata: str | None = typer.Option(None),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Update a Markdown document and index it."""
    with _service(root, index) as service:
        service.update(
            path,
            content=content,
            frontmatter=None if metadata is None else _metadata(metadata),
        )
    typer.echo(path)


@app.command("delete-note")
def delete_note(
    path: str,
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Delete a Markdown document and its index row."""
    with _service(root, index) as service:
        service.delete(path)


@app.command("move-note")
def move_note(
    source: str,
    target: str,
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Move a Markdown document within the document library."""
    with _service(root, index) as service:
        service.move(source, target)
    typer.echo(target)


@app.command("search-notes")
def search_notes(
    text: str,
    mode: str = typer.Option("keyword", help="keyword, semantic, or hybrid."),
    limit: int = typer.Option(20, min=1, max=100),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Search indexed Markdown documents."""
    with _service(root, index) as service:
        results = service.search(text, mode=cast(SearchMode, mode), limit=limit)
    typer.echo(json.dumps([asdict(result) for result in results], ensure_ascii=False, default=str))


@app.command("rebuild-index")
def rebuild_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Rebuild SQLite from all Markdown files."""
    with _service(root, index) as service:
        count = service.rebuild_index()
    typer.echo(f"indexed {count} notes")


@app.command("watch-index")
def watch_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Watch Markdown changes and rebuild the local index."""
    with _service(root, index) as service:
        service.rebuild_index()
        typer.echo("watching Markdown document changes; press Ctrl-C to stop")
        watch_documents(service)


def main() -> None:
    """Run the CLI program."""
    app()
