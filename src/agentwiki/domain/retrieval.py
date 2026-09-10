"""Domain contracts for task-oriented Wiki retrieval."""

from datetime import UTC, datetime
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from agentwiki.domain.documents import DocumentPath, Frontmatter
from agentwiki.domain.tags import TagAliases

RetrievalStrategy = Literal["recent", "keyword", "hybrid", "recent_hybrid"]
MatchSource = Literal["exact", "keyword", "semantic", "graph", "recency"]


class ContextQuery(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    query: str = ""
    scope: str = ""
    limit: int = Field(default=10, ge=1, le=20)
    tags: tuple[str, ...] = ()
    note_types: tuple[str, ...] = ()
    metadata_filters: Frontmatter = Field(default_factory=dict)
    tag_aliases: TagAliases = Field(default_factory=dict)
    min_similarity: float = Field(default=0.30, ge=0.0, le=1.0)


class SearchCandidate(BaseModel):
    """One ranked chunk returned by a concrete retrieval source."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    chunk_id: str
    path: DocumentPath
    title: str
    section: str
    content: str
    frontmatter: Frontmatter
    modified_at_ns: int
    # The source's own rank key. For the vector leg this is the negated cosine
    # similarity (ascending order); other legs key on bm25 or a timestamp.
    rank_score: float = 0.0


class RelatedDocument(BaseModel):
    """A directly related document in the derived Wiki graph."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    path: str
    title: str
    relation_type: str
    direction: Literal["outgoing", "incoming"]
    resolution_status: Literal["resolved", "unresolved"]
    anchor: str | None = None
    source_section: str | None = None
    context: str | None = None


class IndexedChunk(BaseModel):
    """A deterministic Markdown fragment persisted in the derived index."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    chunk_id: str
    ordinal: int
    section: str
    content: str
    source_hash: str
    embedding_hash: str = ""


class Evidence(BaseModel):
    """One piece of candidate evidence returned to the caller.

    ``rank_score`` is the reciprocal-rank-fusion score that produced the ordering. It is
    a ranking artefact, not a similarity: it is bounded by the number of matching
    sources (roughly ``sources / 61``) and is not comparable across queries. Use
    ``match_sources`` to judge how many independent paths found this evidence.
    """

    model_config = ConfigDict(frozen=True, extra="forbid")

    path: str
    title: str
    section: str
    snippet: str
    rank_score: float
    match_sources: tuple[MatchSource, ...]
    modified_at: datetime
    frontmatter: Frontmatter
    related: tuple[RelatedDocument, ...] = ()


class ContextResult(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    query: str
    scope: str
    strategy: RetrievalStrategy
    degraded: tuple[str, ...] = ()
    results: tuple[Evidence, ...] = ()
    truncated: bool = False
    # False means nothing cleared the relevance bar (or nothing matched at all). Callers
    # should report "not found" rather than treating an empty list as a weak answer.
    matched: bool = False


def timestamp_from_ns(value: int) -> datetime:
    return datetime.fromtimestamp(value / 1_000_000_000, tz=UTC)
