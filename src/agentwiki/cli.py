"""Maintenance CLI for retrieval, indexing, rules, and validation."""

import asyncio
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
import json
from json import JSONDecodeError
from pathlib import Path
from typing import TypeVar

from pydantic import TypeAdapter
import typer

from agentwiki.config import Settings
from agentwiki.domain.documents import Frontmatter
from agentwiki.domain.retrieval import ContextQuery
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.runtime.context import AgentWikiRuntime, create_runtime
from agentwiki.runtime.watcher import watch_documents

app = typer.Typer(no_args_is_help=True, help="Maintain and search a local Markdown Wiki.")
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
    return TypeAdapter(Frontmatter).validate_python(parsed)


@asynccontextmanager
async def _runtime(root: Path | None, index: Path | None) -> AsyncIterator[AgentWikiRuntime]:
    settings = Settings.load()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    runtime = await create_runtime(
        root or settings.document_root,
        index or settings.index_path,
        provider,
    )
    try:
        yield runtime
    finally:
        await runtime.close()


def _execute[ResultT](
    root: Path | None,
    index: Path | None,
    operation: Callable[[AgentWikiRuntime], Awaitable[ResultT]],
) -> ResultT:
    async def run() -> ResultT:
        async with _runtime(root, index) as runtime:
            return await operation(runtime)

    return asyncio.run(run())


@app.command("query")
def query_wiki(
    text: str = typer.Argument("", help="Task query; omit to list recently modified documents."),
    scope: str = typer.Option("", help="Relative Wiki scope."),
    limit: int = typer.Option(10, min=1, max=20),
    tags: str | None = typer.Option(None, help="Comma-separated tags."),
    note_types: str | None = typer.Option(None, help="Comma-separated Frontmatter types."),
    metadata: str | None = typer.Option(None, help="Frontmatter filters as JSON."),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Retrieve a task-oriented Wiki evidence bundle."""
    request = ContextQuery(
        query=text,
        scope=scope,
        limit=limit,
        tags=tuple(item.strip() for item in tags.split(",") if item.strip()) if tags else (),
        note_types=(
            tuple(item.strip() for item in note_types.split(",") if item.strip())
            if note_types
            else ()
        ),
        metadata_filters=_metadata(metadata),
        min_similarity=Settings.load().min_similarity,
    )

    async def retrieve(runtime: AgentWikiRuntime) -> dict[str, object]:
        result = await runtime.retrieval.get_wiki_context(request)
        return result.model_dump(mode="json")

    typer.echo(json.dumps(_execute(root, index, retrieve), ensure_ascii=False, default=str))


@app.command("sync-index")
def sync_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Incrementally reconcile native Markdown changes."""

    async def synchronize(runtime: AgentWikiRuntime) -> dict[str, object]:
        return (await runtime.synchronizer.ensure_fresh()).model_dump(mode="json")

    typer.echo(json.dumps(_execute(root, index, synchronize), ensure_ascii=False))


@app.command("rebuild-index")
def rebuild_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Clear and rebuild the derived search projection."""

    async def rebuild(runtime: AgentWikiRuntime) -> dict[str, object]:
        return (await runtime.synchronizer.rebuild()).model_dump(mode="json")

    typer.echo(json.dumps(_execute(root, index, rebuild), ensure_ascii=False))


@app.command("rules")
def rules(
    scope: str = typer.Argument(""),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Show effective Wiki organization rules."""

    async def load(runtime: AgentWikiRuntime) -> dict[str, object]:
        await asyncio.sleep(0)
        return runtime.governance.get_wiki_rules(scope).model_dump(mode="json")

    typer.echo(json.dumps(_execute(root, index, load), ensure_ascii=False))


@app.command("validate-wiki")
def validate_wiki(
    path: str | None = typer.Argument(None),
    full: bool = typer.Option(False, "--full"),
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Validate native Markdown changes without rewriting them."""

    async def validate(runtime: AgentWikiRuntime) -> dict[str, object]:
        report = await asyncio.to_thread(runtime.governance.validate_wiki, path, full=full)
        return report.model_dump(mode="json")

    typer.echo(json.dumps(_execute(root, index, validate), ensure_ascii=False))


@app.command("watch-index")
def watch_index(
    root: Path | None = typer.Option(None),
    index: Path | None = typer.Option(None),
) -> None:
    """Watch Markdown changes and incrementally refresh the index."""

    async def run() -> None:
        async with _runtime(root, index) as runtime:
            await runtime.synchronizer.ensure_fresh()
            typer.echo("watching Markdown changes; press Ctrl-C to stop")
            await watch_documents(runtime.library.root, runtime.synchronizer)

    asyncio.run(run())


def main() -> None:
    app()
