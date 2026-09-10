from sqlite3 import OperationalError
from typing import cast

from agentwiki.domain.documents import DocumentPath, SyncReport
from agentwiki.domain.retrieval import ContextQuery, SearchCandidate
from agentwiki.services.ports import FreshnessSynchronizer, SearchRepository
from agentwiki.services.retrieval import RetrievalService


def _candidate(path: str, content: str, *, modified: int = 1) -> SearchCandidate:
    return SearchCandidate(
        chunk_id=f"{path}:{content}",
        path=DocumentPath(value=path),
        title=path.removesuffix(".md"),
        section="Section",
        content=content,
        frontmatter={},
        modified_at_ns=modified,
    )


class FakeSynchronizer:
    async def ensure_fresh(self) -> SyncReport:
        return SyncReport()

    async def rebuild(self) -> SyncReport:
        return SyncReport()


class FakeRepository:
    def __init__(
        self,
        *,
        semantic: bool = True,
        fail_semantic: bool = False,
        fail_keyword: bool = False,
    ) -> None:
        self.semantic_available = semantic
        self.fail_semantic = fail_semantic
        self.fail_keyword = fail_keyword
        self.items = [
            _candidate("old.md", "SQLite evidence", modified=1),
            _candidate("new.md", "Authentication evidence", modified=3),
        ]

    async def wait_for_initial_vector_sync(self) -> None:
        return None

    async def wait_for_vector_sync(self) -> None:
        return None

    async def close(self) -> None:
        return None

    async def related_documents(self, paths, *, limit=5):
        return {}

    async def exact_candidates(self, query, *, candidate_limit):
        return []

    async def keyword_candidates(self, query, *, candidate_limit):
        if self.fail_keyword:
            raise OperationalError("database is locked")
        return list(reversed(self.items))

    async def graph_candidates(self, query, *, candidate_limit):
        return []

    async def semantic_candidates(self, query, *, candidate_limit):
        if self.fail_semantic:
            raise RuntimeError("model failed")
        return list(reversed(self.items))

    async def recent_candidates(self, query, *, candidate_limit):
        return list(reversed(self.items))[:candidate_limit]


async def test_retrieval_automatically_uses_hybrid_and_returns_evidence() -> None:
    service = RetrievalService(
        cast(SearchRepository, FakeRepository()),
        cast(FreshnessSynchronizer, FakeSynchronizer()),
    )
    result = await service.get_wiki_context(ContextQuery(query="authentication"))
    assert result.strategy == "hybrid"
    assert result.results[0].path == "new.md"
    assert set(result.results[0].match_sources) == {"keyword", "semantic"}


async def test_empty_query_returns_recent_documents() -> None:
    service = RetrievalService(
        cast(SearchRepository, FakeRepository()),
        cast(FreshnessSynchronizer, FakeSynchronizer()),
    )
    result = await service.get_wiki_context(ContextQuery())
    assert result.strategy == "recent"
    assert [item.path for item in result.results] == ["new.md", "old.md"]


async def test_recent_topic_uses_relevance_and_recency() -> None:
    service = RetrievalService(
        cast(SearchRepository, FakeRepository()),
        cast(FreshnessSynchronizer, FakeSynchronizer()),
    )
    result = await service.get_wiki_context(ContextQuery(query="最近 authentication 的变化"))
    assert result.strategy == "recent_hybrid"
    assert "recency" in result.results[0].match_sources


async def test_semantic_failure_degrades_to_keyword() -> None:
    service = RetrievalService(
        cast(SearchRepository, FakeRepository(fail_semantic=True)),
        cast(FreshnessSynchronizer, FakeSynchronizer()),
    )
    result = await service.get_wiki_context(ContextQuery(query="SQLite"))
    assert result.strategy == "keyword"
    assert result.results
    assert result.degraded[0].startswith("semantic_unavailable")


async def test_keyword_failure_degrades_instead_of_aborting_retrieval() -> None:
    """A transient failure in one source must lower the strategy, not raise."""
    service = RetrievalService(
        cast(SearchRepository, FakeRepository(fail_keyword=True)),
        cast(FreshnessSynchronizer, FakeSynchronizer()),
    )
    result = await service.get_wiki_context(ContextQuery(query="SQLite"))
    assert result.strategy == "keyword"
    assert result.results
    assert any(entry.startswith("keyword_unavailable") for entry in result.degraded)
