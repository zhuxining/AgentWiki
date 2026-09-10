"""Run the AgentWiki retrieval benchmark against a frozen Markdown corpus."""

import argparse
import asyncio
from collections.abc import AsyncIterator, Sequence
from contextlib import asynccontextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from agentwiki.domain.retrieval import ContextQuery, ContextResult, Evidence
from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.repository.embeddings import EmbeddingProvider, FastEmbedProvider
from agentwiki.runtime.context import AgentWikiRuntime, create_runtime
from benchmarks.metrics import relevance_grade, summarize_latency, summarize_quality
from benchmarks.schema import (
    BenchmarkEnvironment,
    BenchmarkQuery,
    BenchmarkReport,
    DegradationStats,
    IndexStats,
    QueryBenchmarkResult,
)


@dataclass(frozen=True)
class CorpusManifest:
    """Stable facts about a benchmark corpus."""

    document_count: int
    total_bytes: int
    sha256: str
    version: str | None


def load_queries(path: Path) -> tuple[BenchmarkQuery, ...]:
    """Load and validate one JSON object per line."""
    queries: list[BenchmarkQuery] = []
    seen_ids: set[str] = set()
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        try:
            raw: object = json.loads(line)
            query = BenchmarkQuery.model_validate(raw)
        except (json.JSONDecodeError, ValueError) as exc:
            raise ValueError(f"invalid benchmark query at {path}:{line_number}: {exc}") from exc
        if query.id in seen_ids:
            raise ValueError(f"duplicate benchmark query id: {query.id}")
        seen_ids.add(query.id)
        queries.append(query)
    if not queries:
        raise ValueError(f"benchmark query file is empty: {path}")
    return tuple(queries)


def inspect_corpus(root: Path) -> CorpusManifest:
    """Create a deterministic manifest from Markdown files below a corpus root."""
    if not root.is_dir():
        raise ValueError(f"corpus root does not exist or is not a directory: {root}")
    library = MarkdownLibrary(root)
    descriptors = library.descriptors()
    digest = hashlib.sha256()
    total_bytes = 0
    for descriptor in descriptors:
        relative = descriptor.path.value
        path = library.path_for(descriptor.path)
        content = path.read_bytes()
        total_bytes += len(content)
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(content)
        digest.update(b"\0")
    version_path = root / "BENCHMARK_VERSION"
    version = version_path.read_text(encoding="utf-8").strip() if version_path.is_file() else None
    return CorpusManifest(len(descriptors), total_bytes, digest.hexdigest(), version)


async def run_benchmark(
    corpus: Path,
    queries: Sequence[BenchmarkQuery],
    *,
    mode: str,
    embedding_model: str | None,
    repeats: int = 1,
) -> BenchmarkReport:
    """Build a temporary index and execute a fixed benchmark query set."""
    if mode not in {"keyword", "hybrid"}:
        raise ValueError("mode must be keyword or hybrid")
    if mode == "hybrid" and not embedding_model:
        raise ValueError("hybrid mode requires --embedding-model")
    if repeats < 1:
        raise ValueError("repeats must be at least 1")

    manifest = inspect_corpus(corpus)
    provider: EmbeddingProvider | None = None
    if mode == "hybrid":
        assert embedding_model is not None
        provider = FastEmbedProvider(embedding_model)
    with tempfile.TemporaryDirectory(prefix="agentwiki-benchmark-") as temporary:
        index_path = Path(temporary) / "index.sqlite3"
        started = time.perf_counter()
        async with _runtime(corpus, index_path, provider) as runtime:
            rebuild = await runtime.synchronizer.rebuild()
            rebuild_seconds = time.perf_counter() - started
            cases: list[tuple[BenchmarkQuery, Sequence[Evidence]]] = []
            raw_cases: list[QueryBenchmarkResult] = []
            latencies: list[float] = []
            query_failures = 0
            for query in queries:
                try:
                    result, latency_ms = await _run_query(runtime, query, repeats=repeats)
                except (OSError, RuntimeError, ValueError) as exc:
                    query_failures += 1
                    cases.append((query, ()))
                    raw_cases.append(_failed_result(query, str(exc)))
                    continue
                cases.append((query, result.results))
                latencies.append(latency_ms)
                raw_cases.append(_raw_result(query, result, latency_ms))
            chunk_count = await _count_index_rows(runtime, "wiki_chunks")
            vector_count = await _count_index_rows(runtime, "wiki_vectors")

        degraded_count = sum(bool(case.degraded) for case in raw_cases)
        semantic_unavailable = sum(
            any("semantic_unavailable" in reason for reason in case.degraded)
            for case in raw_cases
        )
        environment = BenchmarkEnvironment(
            corpus=str(corpus.resolve()),
            corpus_version=manifest.version or manifest.sha256,
            git_revision=_git_revision(),
            python_version=platform.python_version(),
            platform=platform.platform(),
            mode=mode,
            embedding_model=embedding_model if mode == "hybrid" else None,
        )
        return BenchmarkReport(
            run_id=datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ"),
            environment=environment,
            index=IndexStats(
                documents=manifest.document_count,
                bytes=manifest.total_bytes,
                chunks=chunk_count,
                vectors=vector_count,
                rebuild_seconds=rebuild_seconds,
                indexed=rebuild.indexed,
                degraded=len(rebuild.degraded),
            ),
            quality=summarize_quality(cases),
            latency_ms=summarize_latency(latencies),
            degradation=DegradationStats(
                query_degraded_rate=degraded_count / len(raw_cases),
                semantic_unavailable_rate=semantic_unavailable / len(raw_cases),
                query_failure_rate=query_failures / len(raw_cases),
            ),
            queries=tuple(raw_cases),
        )


@asynccontextmanager
async def _runtime(
    corpus: Path,
    index_path: Path,
    provider: EmbeddingProvider | None,
) -> AsyncIterator[AgentWikiRuntime]:
    runtime = await create_runtime(corpus, index_path, provider)
    try:
        yield runtime
    finally:
        await runtime.close()


async def _run_query(
    runtime: AgentWikiRuntime,
    benchmark_query: BenchmarkQuery,
    *,
    repeats: int,
) -> tuple[ContextResult, float]:
    request = ContextQuery(
        query=benchmark_query.query,
        scope=benchmark_query.scope,
        limit=benchmark_query.limit,
        tags=benchmark_query.tags,
        note_types=benchmark_query.note_types,
        metadata_filters=benchmark_query.metadata_filters,
    )
    result: ContextResult | None = None
    samples: list[float] = []
    for _ in range(repeats):
        started = time.perf_counter()
        result = await runtime.retrieval.get_wiki_context(request)
        samples.append((time.perf_counter() - started) * 1_000)
    if result is None:
        raise RuntimeError("benchmark query did not produce a result")
    return result, sum(samples) / len(samples)


def _raw_result(
    query: BenchmarkQuery,
    result: ContextResult,
    latency_ms: float,
) -> QueryBenchmarkResult:
    return QueryBenchmarkResult(
        query_id=query.id,
        category=query.category,
        difficulty=query.difficulty,
        latency_ms=latency_ms,
        strategy=result.strategy,
        degraded=result.degraded,
        result_paths=tuple(item.path for item in result.results),
        result_sections=tuple(item.section for item in result.results),
        grades=tuple(relevance_grade(item, query) for item in result.results),
        context_result=result.model_dump(mode="json"),
    )


def _failed_result(query: BenchmarkQuery, error: str) -> QueryBenchmarkResult:
    return QueryBenchmarkResult(
        query_id=query.id,
        category=query.category,
        difficulty=query.difficulty,
        latency_ms=0.0,
        strategy="error",
        degraded=(),
        result_paths=(),
        result_sections=(),
        grades=(),
        context_result={},
        error=error,
    )


async def _count_index_rows(runtime: AgentWikiRuntime, table: str) -> int:
    """Read a count through the runtime's asynchronous SQLite connection."""
    if table not in {"wiki_chunks", "wiki_vectors"}:
        raise ValueError(f"unsupported benchmark table: {table}")
    cursor = await runtime.database.connection.execute(f"SELECT COUNT(*) FROM {table}")
    row = await cursor.fetchone()
    return int(row[0]) if row is not None else 0


def _git_revision() -> str | None:
    try:
        return subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True,
            check=True,
            text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--queries", type=Path, required=True)
    parser.add_argument("--mode", choices=("keyword", "hybrid"), default="keyword")
    parser.add_argument("--embedding-model", default=None)
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--markdown-output",
        type=Path,
        default=None,
        help="Markdown summary path; defaults to the JSON output path with a .md suffix.",
    )
    return parser


def render_markdown(report: BenchmarkReport) -> str:
    """Render the stable summary portion of a benchmark report."""
    quality_rows = "\n".join(
        f"| `{name}` | {value:.6f} |" for name, value in sorted(report.quality.items())
    )
    latency_rows = "\n".join(
        f"| `{name}` | {value:.3f} |" for name, value in report.latency_ms.items()
    )
    return (
        f"# AgentWiki benchmark {report.run_id}\n\n"
        f"- Corpus: `{report.environment.corpus}`\n"
        f"- Corpus version: `{report.environment.corpus_version or 'unknown'}`\n"
        f"- Git revision: `{report.environment.git_revision or 'unknown'}`\n"
        f"- Mode: `{report.environment.mode}`\n"
        f"- Embedding model: `{report.environment.embedding_model or 'none'}`\n\n"
        "## Quality\n\n"
        "| Metric | Value |\n| --- | ---: |\n"
        f"{quality_rows}\n\n"
        "## Latency (ms)\n\n"
        "| Metric | Value |\n| --- | ---: |\n"
        f"{latency_rows}\n\n"
        "## Index\n\n"
        f"- Documents: `{report.index.documents}`\n"
        f"- Chunks: `{report.index.chunks}`\n"
        f"- Vectors: `{report.index.vectors}`\n"
        f"- Rebuild seconds: `{report.index.rebuild_seconds:.3f}`\n"
        f"- Query degraded rate: `{report.degradation.query_degraded_rate:.6f}`\n"
        f"- Semantic unavailable rate: `{report.degradation.semantic_unavailable_rate:.6f}`\n"
        f"- Query failure rate: `{report.degradation.query_failure_rate:.6f}`\n"
    )


def main(argv: Sequence[str] | None = None) -> None:
    """Run the benchmark CLI and write a JSON report."""
    args = _parser().parse_args(argv)
    queries = load_queries(args.queries)
    report = asyncio.run(
        run_benchmark(
            args.corpus,
            queries,
            mode=args.mode,
            embedding_model=args.embedding_model,
            repeats=args.repeats,
        )
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report.model_dump(mode="json"), ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    markdown_output = args.markdown_output or args.output.with_suffix(".md")
    markdown_output.parent.mkdir(parents=True, exist_ok=True)
    markdown_output.write_text(render_markdown(report), encoding="utf-8")
    print(json.dumps(report.quality, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
