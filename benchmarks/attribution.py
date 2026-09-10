"""Attribute retrieval loss to the pipeline layer that caused it.

A retrieval benchmark reports *how much* was missed. This module reports *where* it was
lost, because each loss layer has a different fix:

- ``qrels_stale`` - the judged path is not in the corpus; the query set is out of date.
- ``not_indexed`` - the file exists but never reached ``wiki_documents``.
- ``chunk_missing`` - the judged section was never emitted as a chunk.
- ``granularity_only`` - a *descendant* of the judged section was returned, so the judged
  content is present at finer granularity but the exact string differs. This is a
  *labeling* loss, not a retrieval loss: ``metrics.relevance_grade`` compares section
  strings for equality and scores these as grade 0.
- ``wrong_section`` - the document was returned, but only through unrelated sections.
- ``not_recalled`` - the document never entered the returned evidence at all.
- ``ranked_below_k`` / ``retrieved`` - the exact section was returned, below or within k.

The report is deliberately LLM-free. It compares the *retrieval ceiling* (judged evidence
that exists in the index and could in principle be returned) against what a real
``get_wiki_context`` call returned, which answers "if retrieval were perfect, would the
answer have been available?" without paying for a generation step.
"""

import argparse
import asyncio
from collections.abc import AsyncIterator, Callable, Mapping, Sequence
from contextlib import asynccontextmanager
from datetime import UTC, datetime
from enum import StrEnum
import json
from pathlib import Path
import subprocess
import tempfile

from pydantic import BaseModel, ConfigDict

from agentwiki.domain.retrieval import ContextQuery, ContextResult, Evidence
from agentwiki.domain.text import query_terms
from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.repository.embeddings import EmbeddingProvider, FastEmbedProvider
from agentwiki.runtime.context import AgentWikiRuntime, create_runtime
from benchmarks.runner import apply_fixture_mtimes, inspect_corpus, load_queries
from benchmarks.schema import BenchmarkQuery, RelevanceJudgment


class LossLayer(StrEnum):
    """The pipeline layer responsible for losing one relevance judgment."""

    RETRIEVED = "retrieved"
    RANKED_BELOW_K = "ranked_below_k"
    GRANULARITY_ONLY = "granularity_only"
    WRONG_SECTION = "wrong_section"
    NOT_RECALLED = "not_recalled"
    CHUNK_MISSING = "chunk_missing"
    NOT_INDEXED = "not_indexed"
    QRELS_STALE = "qrels_stale"


class SectionRelation(StrEnum):
    """How a judged section relates to a section present in the index or results."""

    EXACT = "exact"
    ANCESTOR = "ancestor"
    DESCENDANT = "descendant"
    UNRELATED = "unrelated"


class JudgmentAttribution(BaseModel):
    """Where one relevance judgment was lost."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    query_id: str
    path: str
    section: str
    grade: int
    layer: LossLayer
    rank: int | None = None
    match_sources: tuple[str, ...] = ()


class QueryAttribution(BaseModel):
    """All judgments for one query."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    query_id: str
    category: str
    strategy: str
    degraded: tuple[str, ...]
    judgments: tuple[JudgmentAttribution, ...]


class AttributionReport(BaseModel):
    """Machine-readable loss attribution for one query set."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    run_id: str
    corpus: str
    corpus_version: str
    git_revision: str | None
    mode: str
    k: int
    judgments: int
    layer_counts: dict[str, int]
    index_ceiling_rate: float
    document_recall_rate: float
    retrieved_rate: float
    granularity_loss_rate: float
    no_answer_contamination: dict[str, tuple[str, ...]]
    queries: tuple[QueryAttribution, ...]


def section_relation(judged: str, candidate: str) -> SectionRelation:
    """Classify a judged section against a section actually present somewhere.

    Sections are ``" / "``-joined heading paths, so ancestry is a prefix relation on the
    split parts. ``judged=""`` is a document-level judgment and is never related here;
    callers handle that case before calling.
    """
    if judged == candidate:
        return SectionRelation.EXACT
    judged_parts = tuple(judged.split(" / "))
    candidate_parts = tuple(candidate.split(" / "))
    if candidate_parts[: len(judged_parts)] == judged_parts:
        return SectionRelation.DESCENDANT
    if judged_parts[: len(candidate_parts)] == candidate_parts:
        return SectionRelation.ANCESTOR
    return SectionRelation.UNRELATED


def attribute_judgment(
    judgment: RelevanceJudgment,
    *,
    query_id: str,
    corpus_paths: frozenset[str],
    index_sections: Mapping[str, tuple[str, ...]],
    results: Sequence[Evidence],
    k: int,
) -> JudgmentAttribution:
    """Locate the pipeline layer that lost one relevance judgment.

    Pure function: every input is passed in, so the decision table can be tested without
    a database or a runtime.
    """
    path = judgment.path
    if path not in corpus_paths:
        return _attribute(query_id, judgment, LossLayer.QRELS_STALE, None, results)
    if path not in index_sections:
        return _attribute(query_id, judgment, LossLayer.NOT_INDEXED, None, results)

    if not judgment.section:
        rank = _first_rank(results, path=path)
        layer = _rank_layer(rank, k)
        return _attribute(query_id, judgment, layer, rank, results)

    sections = index_sections[path]
    # Chunking emits a parent section's own body only up to its first subheading, so a
    # parent chunk does not cover its children. A judged section therefore still counts
    # as indexed when a *descendant* chunk exists (the label was coarser than the
    # chunking), but not when only an ancestor exists (the judged body is genuinely gone).
    descendant_chunked = any(
        section_relation(judgment.section, section) is SectionRelation.DESCENDANT
        for section in sections
    )
    if judgment.section not in sections and not descendant_chunked:
        return _attribute(query_id, judgment, LossLayer.CHUNK_MISSING, None, results)

    exact_rank = _first_rank(results, path=path, section=judgment.section)
    if exact_rank is not None:
        return _attribute(
            query_id, judgment, _rank_layer(exact_rank, k), exact_rank, results
        )

    descendant_rank = _first_descendant_rank(
        results, path=path, judged_section=judgment.section
    )
    if descendant_rank is not None:
        layer = (
            LossLayer.GRANULARITY_ONLY
            if descendant_rank <= k
            else LossLayer.RANKED_BELOW_K
        )
        return _attribute(query_id, judgment, layer, descendant_rank, results)

    document_rank = _first_rank(results, path=path)
    if document_rank is not None:
        return _attribute(query_id, judgment, LossLayer.WRONG_SECTION, document_rank, results)

    return _attribute(query_id, judgment, LossLayer.NOT_RECALLED, None, results)


def detect_no_answer_contamination(
    queries: Sequence[BenchmarkQuery],
    corpus_text: str,
) -> dict[str, tuple[str, ...]]:
    """Report no-answer queries whose own terms literally occur in the corpus.

    A ``no_answer`` case only tests abstention if the corpus genuinely lacks the topic. If
    its terms appear, the expected label is wrong and the false-positive rate it feeds is
    meaningless, so this is checked before any result is interpreted.
    """
    contaminated: dict[str, tuple[str, ...]] = {}
    for query in queries:
        if not query.expected_no_answer:
            continue
        present = tuple(term for term in query_terms(query.query) if term in corpus_text)
        if present:
            contaminated[query.id] = present
    return contaminated


def summarize(report: AttributionReport) -> str:
    """Render a human-readable attribution summary."""
    lines = [
        f"# Retrieval loss attribution {report.run_id}",
        "",
        f"- Corpus: `{report.corpus}` (version `{report.corpus_version}`)",
        f"- Mode: `{report.mode}`, k = {report.k}",
        f"- Judgments: {report.judgments}",
        "",
        "## Loss layers",
        "",
        "| Layer | Count |",
        "| --- | ---: |",
    ]
    for layer in LossLayer:
        count = report.layer_counts.get(layer.value, 0)
        if count:
            lines.append(f"| `{layer.value}` | {count} |")
    lines += [
        "",
        "## Rates",
        "",
        f"- Index ceiling (judged section exists in the index): {report.index_ceiling_rate:.4f}",
        f"- Document recall (judged file appears in results): {report.document_recall_rate:.4f}",
        f"- Retrieved (exact section within k): {report.retrieved_rate:.4f}",
        f"- Granularity loss (labeling, not retrieval): {report.granularity_loss_rate:.4f}",
    ]
    if report.no_answer_contamination:
        lines += ["", "## No-answer contamination", ""]
        for query_id, terms in sorted(report.no_answer_contamination.items()):
            lines.append(f"- `{query_id}`: {', '.join(terms)}")
    return "\n".join(lines) + "\n"


async def run_attribution(
    corpus: Path,
    queries: Sequence[BenchmarkQuery],
    *,
    mode: str,
    embedding_model: str | None,
    k: int,
) -> AttributionReport:
    """Build a temporary index and attribute every judgment of a query set."""
    if mode not in {"keyword", "hybrid"}:
        raise ValueError("mode must be keyword or hybrid")
    if mode == "hybrid" and not embedding_model:
        raise ValueError("hybrid mode requires --embedding-model")
    if k < 1:
        raise ValueError("k must be at least 1")

    apply_fixture_mtimes(corpus)
    manifest = inspect_corpus(corpus)
    corpus_paths = frozenset(descriptor.path.value for descriptor in _library(corpus).descriptors())
    corpus_text = _corpus_text(corpus)
    provider: EmbeddingProvider | None = None
    if mode == "hybrid":
        assert embedding_model is not None
        provider = FastEmbedProvider(embedding_model)

    with tempfile.TemporaryDirectory(prefix="agentwiki-attribution-") as temporary:
        index_path = Path(temporary) / "index.sqlite3"
        async with _runtime(corpus, index_path, provider) as runtime:
            await runtime.synchronizer.rebuild()
            index_sections = await _index_sections(runtime)
            attributions: list[QueryAttribution] = []
            for query in queries:
                result = await _query(runtime, query)
                attributions.append(
                    QueryAttribution(
                        query_id=query.id,
                        category=str(query.category),
                        strategy=result.strategy,
                        degraded=result.degraded,
                        judgments=tuple(
                            attribute_judgment(
                                judgment,
                                query_id=query.id,
                                corpus_paths=corpus_paths,
                                index_sections=index_sections,
                                results=result.results,
                                k=k,
                            )
                            for judgment in query.relevance
                        ),
                    )
                )

    flat = [judgment for attribution in attributions for judgment in attribution.judgments]
    return AttributionReport(
        run_id=datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ"),
        corpus=str(corpus.resolve()),
        corpus_version=manifest.version or manifest.sha256,
        git_revision=_git_revision(),
        mode=mode,
        k=k,
        judgments=len(flat),
        layer_counts=_layer_counts(flat),
        index_ceiling_rate=_rate(flat, _is_at_index_ceiling),
        document_recall_rate=_rate(flat, _is_document_recalled),
        retrieved_rate=_rate(flat, lambda item: item.layer is LossLayer.RETRIEVED),
        granularity_loss_rate=_rate(
            flat, lambda item: item.layer is LossLayer.GRANULARITY_ONLY
        ),
        no_answer_contamination=detect_no_answer_contamination(queries, corpus_text),
        queries=tuple(attributions),
    )


def _library(corpus: Path) -> MarkdownLibrary:
    return MarkdownLibrary(corpus)


def _corpus_text(corpus: Path) -> str:
    library = _library(corpus)
    return "\n".join(
        library.path_for(descriptor.path).read_text(encoding="utf-8")
        for descriptor in library.descriptors()
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


async def _query(runtime: AgentWikiRuntime, benchmark_query: BenchmarkQuery) -> ContextResult:
    return await runtime.retrieval.get_wiki_context(
        ContextQuery(
            query=benchmark_query.query,
            scope=benchmark_query.scope,
            limit=benchmark_query.limit,
            tags=benchmark_query.tags,
            note_types=benchmark_query.note_types,
            metadata_filters=benchmark_query.metadata_filters,
        )
    )


async def _index_sections(runtime: AgentWikiRuntime) -> dict[str, tuple[str, ...]]:
    """Read the sections that actually reached the chunk table."""
    cursor = await runtime.database.connection.execute(
        "SELECT path, section FROM wiki_chunks ORDER BY path, ordinal"
    )
    rows = await cursor.fetchall()
    collected: dict[str, list[str]] = {}
    for path, section in rows:
        collected.setdefault(str(path), []).append(str(section))
    return {path: tuple(dict.fromkeys(sections)) for path, sections in collected.items()}


def _attribute(
    query_id: str,
    judgment: RelevanceJudgment,
    layer: LossLayer,
    rank: int | None,
    results: Sequence[Evidence],
) -> JudgmentAttribution:
    return JudgmentAttribution(
        query_id=query_id,
        path=judgment.path,
        section=judgment.section,
        grade=judgment.grade,
        layer=layer,
        rank=rank,
        match_sources=_document_sources(results, judgment.path),
    )


def _document_sources(results: Sequence[Evidence], path: str) -> tuple[str, ...]:
    for item in results:
        if item.path == path:
            return tuple(str(source) for source in item.match_sources)
    return ()


def _first_rank(
    results: Sequence[Evidence],
    *,
    path: str,
    section: str | None = None,
) -> int | None:
    for rank, item in enumerate(results, start=1):
        if item.path != path:
            continue
        if section is None or item.section == section:
            return rank
    return None


def _first_descendant_rank(
    results: Sequence[Evidence],
    *,
    path: str,
    judged_section: str,
) -> int | None:
    """Return the first rank whose section is a *descendant* of the judged section.

    Only descendants qualify. A parent chunk holds just the body above its first
    subheading, so matching a parent would claim coverage the chunk does not have.
    """
    for rank, item in enumerate(results, start=1):
        if item.path != path:
            continue
        if section_relation(judged_section, item.section) is SectionRelation.DESCENDANT:
            return rank
    return None


def _rank_layer(rank: int | None, k: int) -> LossLayer:
    if rank is None:
        return LossLayer.NOT_RECALLED
    return LossLayer.RETRIEVED if rank <= k else LossLayer.RANKED_BELOW_K


def _layer_counts(judgments: Sequence[JudgmentAttribution]) -> dict[str, int]:
    counts = {layer.value: 0 for layer in LossLayer}
    for judgment in judgments:
        counts[judgment.layer.value] += 1
    return counts


def _rate(
    judgments: Sequence[JudgmentAttribution],
    predicate: Callable[[JudgmentAttribution], bool],
) -> float:
    if not judgments:
        return 0.0
    matched = sum(1 for judgment in judgments if predicate(judgment))
    return matched / len(judgments)


def _is_at_index_ceiling(judgment: JudgmentAttribution) -> bool:
    """Whether the judged content exists in the index and could in principle be returned."""
    return judgment.layer not in {
        LossLayer.QRELS_STALE,
        LossLayer.NOT_INDEXED,
        LossLayer.CHUNK_MISSING,
    }


def _is_document_recalled(judgment: JudgmentAttribution) -> bool:
    """Whether the judged file appeared in the returned evidence at all."""
    return judgment.layer not in {
        LossLayer.QRELS_STALE,
        LossLayer.NOT_INDEXED,
        LossLayer.CHUNK_MISSING,
        LossLayer.NOT_RECALLED,
    }


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
    parser.add_argument("--k", type=int, default=5)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--markdown-output", type=Path, default=None)
    return parser


def main(argv: Sequence[str] | None = None) -> None:
    """Run loss attribution and write a machine-readable report."""
    args = _parser().parse_args(argv)
    queries = load_queries(args.queries)
    report = asyncio.run(
        run_attribution(
            args.corpus,
            queries,
            mode=args.mode,
            embedding_model=args.embedding_model,
            k=args.k,
        )
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report.model_dump(mode="json"), ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    markdown_output = args.markdown_output or args.output.with_suffix(".md")
    markdown_output.parent.mkdir(parents=True, exist_ok=True)
    markdown_output.write_text(summarize(report), encoding="utf-8")
    print(summarize(report))


if __name__ == "__main__":
    main()
