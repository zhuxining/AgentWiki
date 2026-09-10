"""FastMCP adapter for task retrieval, Wiki rules, and validation."""

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager

from fastmcp import Context, FastMCP
from fastmcp.server.lifespan import lifespan

from agentwiki.config import Settings
from agentwiki.domain.documents import Frontmatter
from agentwiki.domain.retrieval import ContextQuery
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.runtime.context import AgentWikiRuntime, create_runtime

_RUNTIME_CONTEXT_KEY = "agentwiki.runtime"


@lifespan
async def _lifespan(_server: FastMCP) -> AsyncIterator[dict[str, AgentWikiRuntime]]:
    settings = Settings.load()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    runtime = await create_runtime(settings.document_root, settings.index_path, provider)
    try:
        yield {_RUNTIME_CONTEXT_KEY: runtime}
    finally:
        await runtime.close()


mcp = FastMCP(
    "agentwiki",
    instructions=(
        "AgentWiki retrieves evidence from a local Markdown Wiki. You MUST call "
        "get_wiki_context when a task depends on Wiki history, conventions, prior decisions, "
        "cross-document relationships, recent changes, or content whose path is unknown. "
        "The configured Wiki root is returned as `wiki_root` by retrieval and rules tools "
        "(normally `~/AgentWiki`); result paths are relative to that root. "
        "If an exact path is already known and no other Wiki knowledge is needed, use native "
        "file tools directly. Search snippets are candidate evidence: read important source "
        "files with native tools before quoting them, deciding, or editing. Call "
        "get_wiki_rules before creating, moving, or first editing in an unfamiliar scope; "
        "reuse rules for consecutive edits in the same scope. Use native tools for Markdown "
        "changes, then call validate_wiki for changed paths."
    ),
    lifespan=_lifespan,
)


@asynccontextmanager
async def _runtime(ctx: Context) -> AsyncIterator[AgentWikiRuntime]:
    runtime = ctx.lifespan_context.get(_RUNTIME_CONTEXT_KEY)
    if isinstance(runtime, AgentWikiRuntime):
        yield runtime
        return
    settings = Settings.load()
    provider = FastEmbedProvider(settings.embedding_model) if settings.embedding_model else None
    runtime = await create_runtime(settings.document_root, settings.index_path, provider)
    try:
        yield runtime
    finally:
        await runtime.close()


@mcp.resource("agentwiki://rules", title="Wiki Rules", mime_type="application/json")
async def wiki_rules_resource(ctx: Context) -> str:
    async with _runtime(ctx) as runtime:
        return runtime.governance.get_wiki_rules().model_dump_json()


@mcp.resource("agentwiki://guide", title="Wiki Guide", mime_type="text/markdown")
async def wiki_guide_resource(ctx: Context) -> str:
    async with _runtime(ctx) as runtime:
        return runtime.governance.get_wiki_rules().guide_content


@mcp.tool(
    title="Get Wiki Context",
    description=(
        "Retrieve task-relevant evidence when Wiki content, history, decisions, relationships, "
        "or recent changes must be discovered. Omit query for recent documents. Results are "
        "candidate snippets and paths; read important source files before relying on them. "
        "Do not use this tool merely to read an already-known exact path."
    ),
    tags={"context", "retrieval", "search"},
    annotations={
        "title": "Get Wiki Context",
        "readOnlyHint": True,
        "destructiveHint": False,
        "openWorldHint": False,
    },
)
async def get_wiki_context(
    query: str = "",
    scope: str = "",
    limit: int = 10,
    tags: list[str] | None = None,
    note_types: list[str] | None = None,
    metadata_filters: Frontmatter | None = None,
    *,
    ctx: Context,
) -> dict[str, object]:
    """Retrieve a bounded, automatically ranked Wiki evidence bundle."""
    request = ContextQuery(
        query=query,
        scope=scope,
        limit=limit,
        tags=tuple(tags or ()),
        note_types=tuple(note_types or ()),
        metadata_filters=metadata_filters or {},
    )
    async with _runtime(ctx) as runtime:
        result = await runtime.retrieval.get_wiki_context(request)
        response = result.model_dump(mode="json")
        response["wiki_root"] = str(runtime.library.root)
    return response


@mcp.tool(
    title="Get Wiki Rules",
    description=(
        "Return effective organization and Frontmatter rules for a target path. Call before "
        "creating, moving, or first editing in an unfamiliar Wiki scope."
    ),
    tags={"governance", "rules"},
    annotations={
        "title": "Get Wiki Rules",
        "readOnlyHint": True,
        "destructiveHint": False,
        "openWorldHint": False,
    },
)
async def get_wiki_rules(scope: str = "", *, ctx: Context) -> dict[str, object]:
    async with _runtime(ctx) as runtime:
        response = runtime.governance.get_wiki_rules(scope).model_dump(mode="json")
        response["wiki_root"] = str(runtime.library.root)
        return response


@mcp.tool(
    title="Validate Wiki",
    description=(
        "Validate Markdown structure, formatting, Frontmatter, and internal links after native "
        "file changes. Use full only for an explicit whole-Wiki acceptance check."
    ),
    tags={"validation", "maintenance"},
    annotations={
        "title": "Validate Wiki",
        "readOnlyHint": True,
        "destructiveHint": False,
        "openWorldHint": False,
    },
)
async def validate_wiki(
    path: str | None = None,
    full: bool = False,
    *,
    ctx: Context,
) -> dict[str, object]:
    async with _runtime(ctx) as runtime:
        report = await asyncio.to_thread(runtime.governance.validate_wiki, path, full=full)
    return report.model_dump(mode="json")


def main() -> None:
    mcp.run()
