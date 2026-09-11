"""Explicitly owned runtime resources shared by composition roots."""

from dataclasses import dataclass
from pathlib import Path

from agentwiki.indexing.sync import IndexSynchronizer
from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.repository.embeddings import EmbeddingProvider
from agentwiki.repository.search import SQLiteSearchRepository
from agentwiki.repository.sqlite import SQLiteDatabase
from agentwiki.services.governance import GovernanceService
from agentwiki.services.retrieval import RetrievalService


@dataclass(frozen=True)
class AgentWikiRuntime:
    library: MarkdownLibrary
    database: SQLiteDatabase
    repository: SQLiteSearchRepository
    synchronizer: IndexSynchronizer
    retrieval: RetrievalService
    governance: GovernanceService

    async def close(self) -> None:
        await self.repository.close()
        await self.database.close()


async def create_runtime(
    document_root: Path,
    index_path: Path,
    embedding_provider: EmbeddingProvider | None = None,
) -> AgentWikiRuntime:
    """Assemble the runtime and make sure the Wiki has a control file.

    A Wiki without ``AGENTWIKI.md`` is unusable for agents (no rules, no guidance), so
    startup seeds the packaged default. An existing file is never overwritten.
    """
    library = MarkdownLibrary(document_root)
    library.ensure_control_file()
    database = SQLiteDatabase(index_path)
    await database.initialize()
    repository = SQLiteSearchRepository(database, embedding_provider)
    await repository.initialize()
    synchronizer = IndexSynchronizer(library, repository, database.path)
    governance = GovernanceService(library)
    return AgentWikiRuntime(
        library=library,
        database=database,
        repository=repository,
        synchronizer=synchronizer,
        retrieval=RetrievalService(repository, synchronizer, governance),
        governance=governance,
    )
