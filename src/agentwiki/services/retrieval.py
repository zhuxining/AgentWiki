"""Task-oriented Wiki evidence retrieval."""

import asyncio
from collections.abc import Awaitable, Callable
import operator
import re
import sqlite3

from agentwiki.domain.retrieval import (
    ContextQuery,
    ContextResult,
    Evidence,
    MatchSource,
    RetrievalStrategy,
    SearchCandidate,
    timestamp_from_ns,
)
from agentwiki.domain.scope import normalize_scope
from agentwiki.services.ports import FreshnessSynchronizer, SearchRepository, TagRulesProvider

_PURE_RECENT_QUERY = re.compile(
    r"^(?:最近|近期|新近|最新|刚刚|recent|recently|latest|newest|new)$",
    re.IGNORECASE,
)
_RECENT_SIGNAL = re.compile(
    r"(?:最近|近期|新近|最新|刚刚|更新|变化|变更|活动|recent|recently|latest|newest|"
    r"changed|updated)",
    re.IGNORECASE,
)
_SOURCE_ORDER: tuple[MatchSource, ...] = (
    "exact",
    "keyword",
    "semantic",
    "graph",
    "recency",
)
# Source weights for reciprocal rank fusion. Content sources outrank recency, and
# exact path/title matches outrank broad keyword recall.
_SOURCE_WEIGHTS: dict[MatchSource, float] = {
    "exact": 1.2,
    "keyword": 1.0,
    "semantic": 1.0,
    "graph": 0.4,
    "recency": 0.3,
}
_RRF_K = 60
# The graph source scans every edge, so it only runs when the query actually asks for
# a relationship rather than for content.
_GRAPH_INTENT = re.compile(
    r"(?:依赖|引用|参见|相关|关联|关系|上游|下游|depends?_?on|links?_?to|related|relations?|references?)",
    re.IGNORECASE,
)
# Guarded sources degrade to a warning instead of failing the whole retrieval.
_DEGRADABLE_ERRORS = (ImportError, OSError, RuntimeError, TypeError, ValueError, sqlite3.Error)
# Evidence snippets must stay well inside a model context window per document.
_SNIPPET_CHARS = 700


def _snippet(candidate: SearchCandidate, query: str) -> str:
    """Return a bounded snippet that keeps the matched wording visible.

    A chunk whose match sits past the cut-off would otherwise be returned as evidence
    that does not contain the query at all, so the window is centred on the first
    match when one is found.
    """
    normalized = " ".join(candidate.content.split())
    if not normalized or not query:
        return _truncate(normalized)
    needle = query.strip().casefold()
    position = normalized.casefold().find(needle) if needle else -1
    if position < 0 or position + len(needle) <= _SNIPPET_CHARS:
        return _truncate(normalized)
    start = max(0, position - (_SNIPPET_CHARS - len(needle)) // 2)
    window = normalized[start : start + _SNIPPET_CHARS]
    prefix = "…" if start > 0 else ""
    suffix = "…" if start + _SNIPPET_CHARS < len(normalized) else ""
    return f"{prefix}{window.strip()}{suffix}"


def _truncate(text: str) -> str:
    return text if len(text) <= _SNIPPET_CHARS else text[:_SNIPPET_CHARS]


class RetrievalService:
    def __init__(
        self,
        repository: SearchRepository,
        synchronizer: FreshnessSynchronizer,
        tag_rules: TagRulesProvider | None = None,
    ) -> None:
        self.repository = repository
        self.synchronizer = synchronizer
        self.tag_rules = tag_rules

    async def get_wiki_context(self, query: ContextQuery) -> ContextResult:
        normalized = query.model_copy(
            update={
                "scope": normalize_scope(query.scope),
                "tag_aliases": self.tag_rules.get_tag_aliases() if self.tag_rules else {},
            }
        )
        sync = await self.synchronizer.ensure_fresh()
        await self.repository.wait_for_initial_vector_sync()
        degraded = list(sync.degraded)
        query_text = normalized.query.strip()
        # Only a query that is nothing but a recency word asks for "what changed lately".
        # Words like 更新/变更 also carry subject matter ("更新流程", "变更管理"), so they
        # bias ranking towards recency but stay in the topic instead of being stripped.
        wants_recent_only = bool(_PURE_RECENT_QUERY.fullmatch(query_text))
        has_recent_intent = bool(_RECENT_SIGNAL.search(query_text))
        topic = " ".join(
            _PURE_RECENT_QUERY.sub(" ", query_text).split()
        ) if wants_recent_only else query_text

        if not query_text or wants_recent_only or not topic:
            # One row per document (newest chunk), so ask for one extra to detect
            # truncation without letting a single long document fill the answer.
            candidates = await self.repository.recent_candidates(
                normalized, candidate_limit=normalized.limit + 1
            )
            evidence = self._recent_evidence(candidates[: normalized.limit])
            evidence = await self._attach_related(evidence)
            return ContextResult(
                query=query.query,
                scope=normalized.scope,
                strategy="recent",
                degraded=tuple(degraded),
                results=tuple(evidence),
                truncated=len(candidates) > normalized.limit,
                matched=bool(evidence),
            )

        exact_query = normalized.model_copy(update={"query": topic})
        # Tag aliases are not spliced into the query text: keyword matching requires every
        # query token to be present, so adding alias spellings only narrows recall. Aliases
        # still apply to tag *filtering* through `tag_aliases` in SQL.
        search_query = normalized.model_copy(update={"query": topic})
        candidate_limit = max(search_query.limit * 4, 20)
        repository = self.repository
        async with asyncio.TaskGroup() as group:
            exact_task = group.create_task(
                self._guard(
                    "exact",
                    lambda q, limit: repository.exact_candidates(q, candidate_limit=limit),
                    exact_query,
                    candidate_limit,
                )
            )
            keyword_task = group.create_task(
                self._guard(
                    "keyword",
                    lambda q, limit: repository.keyword_candidates(q, candidate_limit=limit),
                    search_query,
                    candidate_limit,
                )
            )
            graph_task = (
                group.create_task(
                    self._guard(
                        "graph",
                        lambda q, limit: repository.graph_candidates(q, candidate_limit=limit),
                        search_query,
                        candidate_limit,
                    )
                )
                if _GRAPH_INTENT.search(topic)
                else None
            )
            semantic_task = (
                group.create_task(
                    self._guard(
                        "semantic",
                        lambda q, limit: repository.semantic_candidates(q, candidate_limit=limit),
                        search_query,
                        candidate_limit,
                    )
                )
                if repository.semantic_available
                else None
            )

        sources: dict[MatchSource, list[SearchCandidate]] = {}
        for source, task in (
            ("exact", exact_task),
            ("keyword", keyword_task),
            ("graph", graph_task),
        ):
            if task is None:
                sources[source] = []
                continue
            values, error = task.result()
            sources[source] = values
            if error:
                degraded.append(error)
        semantic: list[SearchCandidate] = []
        if semantic_task is not None:
            semantic, semantic_error = semantic_task.result()
            sources["semantic"] = semantic
            if semantic_error:
                degraded.append(semantic_error)
        else:
            degraded.append("semantic_unavailable")

        has_content = any(sources.values())
        if not has_content:
            # Distinguish "nothing cleared the bar" from "a dependency was unavailable";
            # `matched=False` on the result is the machine-readable form of this.
            degraded.append("no_content_match")

        ranked = self._fuse(sources, include_recency=has_recent_intent)
        selected, truncated = self._select(ranked, search_query.limit, query=topic)
        selected = await self._attach_related(selected)
        return ContextResult(
            query=query.query,
            scope=normalized.scope,
            strategy=self._strategy(sources, has_recent_intent),
            degraded=tuple(dict.fromkeys(degraded)),
            results=tuple(selected),
            truncated=truncated,
            matched=bool(selected),
        )

    @staticmethod
    def _strategy(
        sources: dict[MatchSource, list[SearchCandidate]], has_recent_intent: bool
    ) -> RetrievalStrategy:
        """Name the strategy from the sources that actually contributed results."""
        content = {source for source, values in sources.items() if values}
        if has_recent_intent:
            return "recent_hybrid" if content else "recent"
        if "semantic" in content and len(content) > 1:
            return "hybrid"
        return "keyword"

    async def _guard(
        self,
        source: str,
        call: Callable[[ContextQuery, int], Awaitable[list[SearchCandidate]]],
        query: ContextQuery,
        candidate_limit: int,
    ) -> tuple[list[SearchCandidate], str | None]:
        """Run one retrieval source, degrading to a warning instead of raising.

        Any single source failing (a transient ``database is locked``, a missing
        extension, a malformed query) must lower the strategy, never abort the call.
        """
        try:
            return await call(query, candidate_limit), None
        except _DEGRADABLE_ERRORS as exc:
            return [], f"{source}_unavailable: {exc}"

    async def _attach_related(self, evidence: list[Evidence]) -> list[Evidence]:
        if not evidence:
            return evidence
        related = await self.repository.related_documents(
            tuple(item.path for item in evidence), limit=5
        )
        return [
            item.model_copy(update={"related": related.get(item.path, ())})
            for item in evidence
        ]

    @staticmethod
    def _fuse(
        sources_by_name: dict[MatchSource, list[SearchCandidate]],
        *,
        include_recency: bool,
    ) -> list[tuple[SearchCandidate, float, set[MatchSource]]]:
        """Weighted reciprocal rank fusion.

        Rank position is the only signal every source can produce, but the sources are
        not equally trustworthy, so each contributes with its own weight. Recency is
        additionally ranked per document: every chunk of a document shares
        ``modified_at_ns``, and ranking chunks would give a long document many times
        the recency weight of a short one.
        """
        scores: dict[str, float] = {}
        matched: dict[str, set[MatchSource]] = {}
        candidates: dict[str, SearchCandidate] = {}
        for source, values in sources_by_name.items():
            weight = _SOURCE_WEIGHTS[source]
            for rank, candidate in enumerate(values, start=1):
                candidates.setdefault(candidate.chunk_id, candidate)
                scores[candidate.chunk_id] = scores.get(candidate.chunk_id, 0.0) + weight / (
                    _RRF_K + rank
                )
                matched.setdefault(candidate.chunk_id, set()).add(source)
        if include_recency:
            latest_by_path: dict[str, int] = {}
            for candidate in candidates.values():
                path = candidate.path.value
                latest_by_path[path] = max(
                    latest_by_path.get(path, 0), candidate.modified_at_ns
                )
            by_recency = sorted(
                latest_by_path.items(), key=operator.itemgetter(1), reverse=True
            )
            rank_by_path = {path: rank for rank, (path, _) in enumerate(by_recency, start=1)}
            for chunk_id, candidate in candidates.items():
                rank = rank_by_path[candidate.path.value]
                scores[chunk_id] += _SOURCE_WEIGHTS["recency"] / (_RRF_K + rank)
                matched[chunk_id].add("recency")
        return sorted(
            (
                (candidate, scores[chunk_id], matched[chunk_id])
                for chunk_id, candidate in candidates.items()
            ),
            key=operator.itemgetter(1),
            reverse=True,
        )

    @classmethod
    def _select(
        cls,
        ranked: list[tuple[SearchCandidate, float, set[MatchSource]]],
        limit: int,
        *,
        query: str = "",
    ) -> tuple[list[Evidence], bool]:
        selected: list[Evidence] = []
        per_document: dict[str, int] = {}
        eligible = 0
        for candidate, score, sources in ranked:
            path = candidate.path.value
            if per_document.get(path, 0) >= 2:
                continue
            per_document[path] = per_document.get(path, 0) + 1
            eligible += 1
            if len(selected) < limit:
                selected.append(cls._evidence(candidate, score, sources, query=query))
        return selected, eligible > len(selected)

    @classmethod
    def _recent_evidence(cls, candidates: list[SearchCandidate]) -> list[Evidence]:
        return [cls._evidence(item, 0.0, {"recency"}) for item in candidates]

    @staticmethod
    def _evidence(
        candidate: SearchCandidate,
        rank_score: float,
        sources: set[MatchSource],
        *,
        query: str = "",
    ) -> Evidence:
        return Evidence(
            path=candidate.path.value,
            title=candidate.title,
            section=candidate.section,
            snippet=_snippet(candidate, query),
            rank_score=round(rank_score, 6),
            match_sources=tuple(source for source in _SOURCE_ORDER if source in sources),
            modified_at=timestamp_from_ns(candidate.modified_at_ns),
            frontmatter=candidate.frontmatter,
        )
