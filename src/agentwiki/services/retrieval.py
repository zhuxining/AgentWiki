"""Task-oriented Wiki evidence retrieval."""

import asyncio
import operator
from pathlib import PurePosixPath
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
from agentwiki.services.ports import FreshnessSynchronizer, SearchRepository, TagRulesProvider

_RECENT_INTENT = re.compile(
    r"(?:最近|近期|新近|更新|变化|变更|活动|recent|latest|changed|updated)",
    re.IGNORECASE,
)
_SOURCE_ORDER: tuple[MatchSource, ...] = (
    "exact",
    "keyword",
    "semantic",
    "graph",
    "recency",
)


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
                "scope": self._scope(query.scope),
                "tag_aliases": self.tag_rules.get_tag_aliases() if self.tag_rules else {},
            }
        )
        sync = await self.synchronizer.ensure_fresh()
        wait_for_vectors = getattr(self.repository, "wait_for_initial_vector_sync", None)
        if wait_for_vectors is not None:
            await wait_for_vectors()
        degraded = list(sync.degraded)
        has_recent_intent = bool(_RECENT_INTENT.search(normalized.query))
        topic = _RECENT_INTENT.sub(" ", normalized.query)
        topic = " ".join(topic.split())

        if not normalized.query.strip() or (has_recent_intent and not topic):
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
            )

        exact_query = normalized.model_copy(update={"query": topic})
        search_query = normalized.model_copy(
            update={"query": self._expand_tag_terms(topic, normalized.tag_aliases)}
        )
        candidate_limit = max(search_query.limit * 4, 20)
        async with asyncio.TaskGroup() as group:
            exact_task = group.create_task(
                self.repository.exact_candidates(exact_query, candidate_limit=candidate_limit)
            )
            keyword_task = group.create_task(
                self.repository.keyword_candidates(search_query, candidate_limit=candidate_limit)
            )
            graph_task = group.create_task(
                self._graph(search_query, candidate_limit)
            )
            semantic_task = (
                group.create_task(self._semantic(search_query, candidate_limit))
                if self.repository.semantic_available
                else None
            )

        semantic: list[SearchCandidate] = []
        semantic_error: str | None = None
        if semantic_task is not None:
            semantic, semantic_error = semantic_task.result()
            if semantic_error:
                degraded.append(semantic_error)
        else:
            degraded.append("semantic_unavailable")

        ranked = self._fuse(
            exact_task.result(),
            keyword_task.result(),
            semantic,
            graph_task.result(),
            include_recency=has_recent_intent,
        )
        selected, truncated = self._select(ranked, search_query.limit)
        selected = await self._attach_related(selected)
        strategy: RetrievalStrategy
        if has_recent_intent:
            strategy = "recent_hybrid"
        else:
            strategy = "hybrid" if semantic and semantic_error is None else "keyword"
        return ContextResult(
            query=query.query,
            scope=normalized.scope,
            strategy=strategy,
            degraded=tuple(dict.fromkeys(degraded)),
            results=tuple(selected),
            truncated=truncated,
        )

    async def _attach_related(self, evidence: list[Evidence]) -> list[Evidence]:
        related_query = getattr(self.repository, "related_documents", None)
        if related_query is None or not evidence:
            return evidence
        related = await related_query(tuple(item.path for item in evidence), limit=5)
        return [
            item.model_copy(update={"related": related.get(item.path, ())})
            for item in evidence
        ]

    async def _graph(self, query: ContextQuery, candidate_limit: int) -> list[SearchCandidate]:
        graph_query = getattr(self.repository, "graph_candidates", None)
        if graph_query is None:
            return []
        return await graph_query(query, candidate_limit=candidate_limit)

    @staticmethod
    def _expand_tag_terms(query: str, aliases: dict[str, tuple[str, ...]]) -> str:
        terms = [query]
        lowered = query.casefold()
        for canonical, values in aliases.items():
            spellings = (canonical, *values)
            if any(spelling.casefold() in lowered for spelling in spellings):
                terms.extend(spellings)
        return " ".join(dict.fromkeys(term for term in terms if term))

    async def _semantic(
        self, query: ContextQuery, candidate_limit: int
    ) -> tuple[list[SearchCandidate], str | None]:
        try:
            return (
                await self.repository.semantic_candidates(
                    query, candidate_limit=candidate_limit
                ),
                None,
            )
        except (ImportError, OSError, RuntimeError, TypeError, ValueError, sqlite3.Error) as exc:
            return [], f"semantic_unavailable: {exc}"

    @staticmethod
    def _fuse(
        exact: list[SearchCandidate],
        keyword: list[SearchCandidate],
        semantic: list[SearchCandidate],
        graph: list[SearchCandidate],
        *,
        include_recency: bool,
    ) -> list[tuple[SearchCandidate, float, set[MatchSource]]]:
        scores: dict[str, float] = {}
        sources: dict[str, set[MatchSource]] = {}
        candidates: dict[str, SearchCandidate] = {}
        for source, values in (
            ("exact", exact),
            ("keyword", keyword),
            ("semantic", semantic),
            ("graph", graph),
        ):
            for rank, candidate in enumerate(values, start=1):
                candidates[candidate.chunk_id] = candidate
                scores[candidate.chunk_id] = scores.get(candidate.chunk_id, 0.0) + 1 / (60 + rank)
                sources.setdefault(candidate.chunk_id, set()).add(source)
        if include_recency:
            newest = sorted(candidates.values(), key=lambda item: item.modified_at_ns, reverse=True)
            for rank, candidate in enumerate(newest, start=1):
                scores[candidate.chunk_id] += 1 / (60 + rank)
                sources.setdefault(candidate.chunk_id, set()).add("recency")
        return sorted(
            (
                (candidate, scores[chunk_id], sources[chunk_id])
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
    ) -> tuple[list[Evidence], bool]:
        selected: list[Evidence] = []
        per_document: dict[str, int] = {}
        for candidate, score, sources in ranked:
            path = candidate.path.value
            if per_document.get(path, 0) >= 2:
                continue
            per_document[path] = per_document.get(path, 0) + 1
            selected.append(cls._evidence(candidate, score, sources))
            if len(selected) == limit:
                break
        eligible = sum(
            1
            for index, (candidate, _, _) in enumerate(ranked)
            if sum(
                1
                for previous, _, _ in ranked[:index]
                if previous.path.value == candidate.path.value
            )
            < 2
        )
        return selected, eligible > len(selected)

    @classmethod
    def _recent_evidence(cls, candidates: list[SearchCandidate]) -> list[Evidence]:
        return [cls._evidence(item, 0.0, {"recency"}) for item in candidates]

    @staticmethod
    def _evidence(
        candidate: SearchCandidate,
        score: float,
        sources: set[MatchSource],
    ) -> Evidence:
        snippet = " ".join(candidate.content.split())
        if len(snippet) > 700:
            snippet = f"{snippet[:697].rstrip()}…"
        return Evidence(
            path=candidate.path.value,
            title=candidate.title,
            section=candidate.section,
            snippet=snippet,
            score=round(score, 6),
            match_sources=tuple(source for source in _SOURCE_ORDER if source in sources),
            modified_at=timestamp_from_ns(candidate.modified_at_ns),
            frontmatter=candidate.frontmatter,
        )

    @staticmethod
    def _scope(value: str) -> str:
        if value.startswith(("/", "\\")):
            raise ValueError("scope must stay inside the Wiki root")
        normalized = value.replace("\\", "/").strip("/")
        if not normalized or normalized == ".":
            return ""
        path = PurePosixPath(normalized)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("scope must stay inside the Wiki root")
        return path.as_posix()
