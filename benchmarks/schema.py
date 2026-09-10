"""Input and output models for the offline retrieval benchmark."""

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from agentwiki.domain.documents import Frontmatter

QueryCategory = Literal[
    "exact",
    "keyword",
    "semantic",
    "cross_document",
    "recent",
    "filter",
    "long_document",
    "no_answer",
    "robustness",
]
Difficulty = Literal["easy", "medium", "hard"]


class RelevanceJudgment(BaseModel):
    """A graded document or section judgment for one benchmark query."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    path: str
    section: str = ""
    grade: int = Field(ge=0, le=3)


class BenchmarkQuery(BaseModel):
    """One query case and its human-maintained relevance judgments."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    id: str = Field(min_length=1)
    query: str = ""
    scope: str = ""
    limit: int = Field(default=10, ge=1, le=20)
    tags: tuple[str, ...] = ()
    note_types: tuple[str, ...] = ()
    metadata_filters: Frontmatter = Field(default_factory=dict)
    category: QueryCategory
    difficulty: Difficulty = "medium"
    relevance: tuple[RelevanceJudgment, ...] = ()
    expected_no_answer: bool = False


class BenchmarkEnvironment(BaseModel):
    """Reproducibility metadata stored alongside benchmark results."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    corpus: str
    corpus_version: str | None = None
    git_revision: str | None = None
    python_version: str
    platform: str
    mode: Literal["keyword", "hybrid"]
    embedding_model: str | None = None


class QueryBenchmarkResult(BaseModel):
    """Raw result and scoring details for one executed query."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    query_id: str
    category: QueryCategory
    difficulty: Difficulty
    latency_ms: float
    strategy: str
    degraded: tuple[str, ...]
    result_paths: tuple[str, ...]
    result_sections: tuple[str, ...]
    grades: tuple[int, ...]
    context_result: dict[str, object]
    error: str | None = None


class IndexStats(BaseModel):
    """Typed index build statistics."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    documents: int
    bytes: int
    chunks: int
    vectors: int
    rebuild_seconds: float
    indexed: int
    degraded: int


class DegradationStats(BaseModel):
    """Typed runtime degradation rates."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    query_degraded_rate: float
    semantic_unavailable_rate: float
    query_failure_rate: float


class BenchmarkReport(BaseModel):
    """Complete machine-readable benchmark report."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    run_id: str
    environment: BenchmarkEnvironment
    index: IndexStats
    quality: dict[str, float]
    latency_ms: dict[str, float]
    degradation: DegradationStats
    queries: tuple[QueryBenchmarkResult, ...]
