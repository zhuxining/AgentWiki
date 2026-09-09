"""Typer CLI composition root for local document operations."""

import asyncio
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
import json
from json import JSONDecodeError
from pathlib import Path
from typing import TypeVar, cast

from pydantic import TypeAdapter
import typer

from agentwiki.config import Settings
from agentwiki.domain.models import Frontmatter, Note, SearchMode
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.runtime.watcher import watch_documents
from agentwiki.services.notes import NoteService, create_service

app = typer.Typer(no_args_is_help=True, help="Operate on a local Markdown document library.")
ResultT = TypeVar("ResultT")


def _metadata(value: str | None) -> Frontmatter:
    if not value:
        return {}
    try:
        parsed: object = json.loads(value)
    except JSONDecodeError as exc:
        raise typer.BadParameter("metadata must be valid JSON") from exc
    if not isinstance(parsed, dict):
        raise typer.BadParameter("metadata must be a JSON object")
    try:
        return TypeAdapter(Frontmatter).validate_python(parsed)
    except ValueError as exc:
        raise typer.BadParameter("metadata must contain string keys") from exc


@asynccontextmanager
async def _service(root: Path | None, index: Path | None) -> AsyncIterator[NoteService]:
    settings = Settings.from_env()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    service = await create_service(
        root or settings.document_root,
        index or settings.index_path,
        embedding_provider=provider,
    )
    try:
        yield service
    finally:
        await service.index.close()


def _execute[ResultT](
    root: Path | None,
    index: Path | None,
    operation: Callable[[NoteService], Awaitable[ResultT]],
) -> ResultT:
    async def run() -> ResultT:
        async with _service(root, index) as service:
            return await operation(service)

    return asyncio.run(run())


async def _read_text(
    service: NoteService,
    identifier: str,
    *,
    include_frontmatter: bool,
    start_line: int | None,
    end_line: int | None,
) -> tuple[Note, str]:
    return await asyncio.to_thread(
        service.read_text,
        identifier,
        include_frontmatter=include_frontmatter,
        start_line=start_line,
        end_line=end_line,
    )


@app.command("write-note")
def write_note(
    path: str = typer.Argument("", help="Relative .md path; omit when using --title."),
    content: str = typer.Option("", help="Markdown body."),
    metadata: str | None = typer.Option(None, help="Frontmatter as a JSON object."),
    title: str | None = typer.Option(None, help="Title used when path is omitted."),
    directory: str = typer.Option("", help="Directory used with title."),
    tags: str | None = typer.Option(None, help="Comma-separated tags."),
    note_type: str = typer.Option("note", help="Frontmatter type."),
    overwrite: bool = typer.Option(False, help="Replace an existing document."),
    root: Path | None = typer.Option(None, help="Document library root."),
    index: Path | None = typer.Option(None, help="SQLite index path."),
) -> None:
    """Create a Markdown document and index it."""
    note = _execute(
        root,
        index,
        lambda service: service.write(
            path or None,
            content,
            _metadata(metadata),
            title=title,
            directory=directory,
            tags=(
                None if tags is None else [item.strip() for item in tags.split(",") if item.strip()]
            ),
            note_type=note_type,
            overwrite=overwrite,
        ),
    )
    typer.echo(note.path.value)


@app.command("read-note")
def read_note(
    path: str,
    include_frontmatter: bool = typer.Option(False, "--include-frontmatter"),
    start_line: int | None = typer.Option(None, min=1),
    end_line: int | None = typer.Option(None, min=1),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Read a Markdown document as JSON."""
    note, content = _execute(
        root,
        index,
        lambda service: _read_text(
            service,
            path,
            include_frontmatter=include_frontmatter,
            start_line=start_line,
            end_line=end_line,
        ),
    )
    typer.echo(
        json.dumps(
            {"path": note.path.value, "content": content, "frontmatter": note.frontmatter},
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
    _execute(
        root,
        index,
        lambda service: service.update(
            path,
            content=content,
            frontmatter=None if metadata is None else _metadata(metadata),
        ),
    )
    typer.echo(path)


@app.command("edit-note")
def edit_note(
    identifier: str,
    operation: str = typer.Option(..., help="append, prepend, find_replace, or section operation."),
    content: str = typer.Option(..., help="Replacement or inserted Markdown."),
    find_text: str | None = typer.Option(None),
    section: str | None = typer.Option(None),
    expected_replacements: int = typer.Option(1, min=0),
    replace_subsections: bool = typer.Option(True),
    metadata: str | None = typer.Option(None),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Apply one incremental Markdown edit and refresh its index row."""
    note = _execute(
        root,
        index,
        lambda service: service.edit(
            identifier,
            operation=operation,
            content=content,
            find_text=find_text,
            section=section,
            expected_replacements=expected_replacements,
            replace_subsections=replace_subsections,
            metadata=None if metadata is None else _metadata(metadata),
        ),
    )
    typer.echo(note.path.value)


@app.command("delete-note")
def delete_note(
    path: str,
    is_directory: bool = typer.Option(False, "--is-directory"),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Delete a Markdown document and its index row."""
    _execute(root, index, lambda service: service.delete(path, is_directory=is_directory))


@app.command("move-note")
def move_note(
    source: str,
    target: str,
    destination_folder: bool = typer.Option(False, "--destination-folder"),
    is_directory: bool = typer.Option(False, "--is-directory"),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Move a Markdown document within the document library."""
    _execute(
        root,
        index,
        lambda service: service.move(
            source,
            target,
            destination_folder=destination_folder,
            is_directory=is_directory,
        ),
    )
    typer.echo(target)


@app.command("search-notes")
def search_notes(
    text: str,
    mode: str = typer.Option("keyword", help="keyword, semantic, or hybrid."),
    search_type: str | None = typer.Option(None, help="text, title, permalink, vector, or hybrid."),
    limit: int = typer.Option(20, min=1, max=100),
    page: int = typer.Option(1, min=1),
    tags: str | None = typer.Option(None, help="Comma-separated tags."),
    note_types: str | None = typer.Option(None, help="Comma-separated frontmatter types."),
    metadata: str | None = typer.Option(None, help="Frontmatter filters as JSON."),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Search indexed Markdown documents."""
    results = _execute(
        root,
        index,
        lambda service: service.search(
            text,
            mode=cast(SearchMode, search_type or mode),
            limit=limit,
            page=page,
            tags=None if tags is None else [item.strip() for item in tags.split(",")],
            note_types=None
            if note_types is None
            else [item.strip() for item in note_types.split(",")],
            metadata_filters=None if metadata is None else _metadata(metadata),
        ),
    )
    typer.echo(
        json.dumps([result.model_dump() for result in results], ensure_ascii=False, default=str)
    )


@app.command("rebuild-index")
def rebuild_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Rebuild SQLite from all Markdown files."""
    count = _execute(root, index, lambda service: service.rebuild_index())
    typer.echo(f"indexed {count} notes")


@app.command("watch-index")
def watch_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Watch Markdown changes and rebuild the local index."""

    async def run_watch() -> None:
        async with _service(root, index) as service:
            await service.rebuild_index()
            typer.echo("watching Markdown document changes; press Ctrl-C to stop")
            await watch_documents(service)

    asyncio.run(run_watch())


def main() -> None:
    """Run the CLI program."""
    app()
