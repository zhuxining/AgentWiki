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
    score: float


class RelatedDocument(BaseModel):
    """A directly related document in the derived Wiki graph."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    path: str
    title: str
    relation_type: str
    direction: Literal["outgoing", "incoming"]
    resolution_status: Literal["resolved", "unresolved"]
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
    model_config = ConfigDict(frozen=True, extra="forbid")

    path: str
    title: str
    section: str
    snippet: str
    score: float
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


def timestamp_from_ns(value: int) -> datetime:
    return datetime.fromtimestamp(value / 1_000_000_000, tz=UTC)
