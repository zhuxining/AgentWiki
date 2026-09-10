import math

import pytest

from agentwiki.domain.retrieval import Evidence
from benchmarks.metrics import (
    mean_reciprocal_rank,
    ndcg_at_k,
    percentile,
    recall_at_k,
    section_precision_at_k,
    summarize_latency,
    summarize_quality,
)
from benchmarks.runner import (
    inspect_corpus,
    load_queries,
    main,
    render_markdown,
    run_benchmark,
)
from benchmarks.schema import BenchmarkQuery


def _evidence(path: str, section: str, *, modified: int = 1) -> Evidence:
    from datetime import UTC, datetime

    return Evidence(
        path=path,
        title=path,
        section=section,
        snippet="evidence",
        score=1.0,
        match_sources=("keyword",),
        modified_at=datetime.fromtimestamp(modified, tz=UTC),
        frontmatter={},
    )


def _query(**updates: object) -> BenchmarkQuery:
    values: dict[str, object] = {
        "id": "q1",
        "query": "authentication",
        "category": "keyword",
        "relevance": ({"path": "auth.md", "section": "Tokens", "grade": 3},),
    }
    values.update(updates)
    return BenchmarkQuery.model_validate(values)


def test_graded_retrieval_metrics_use_document_and_section() -> None:
    query = _query()
    results = [_evidence("other.md", "Overview"), _evidence("auth.md", "Tokens")]
    assert recall_at_k(results, query, 1) == pytest.approx(0.0)
    assert recall_at_k(results, query, 2) == pytest.approx(1.0)
    assert mean_reciprocal_rank(results, query, 2) == pytest.approx(0.5)
    assert section_precision_at_k(results, query, 2) == pytest.approx(0.5)
    assert ndcg_at_k(results, query, 2) == pytest.approx(1 / math.log2(3))


def test_quality_summary_reports_no_answer_false_positives() -> None:
    query = _query(id="no-answer", relevance=(), expected_no_answer=True)
    summary = summarize_quality([(query, [_evidence("wrong.md", "Other")])], ks=(1,))
    assert summary["no_answer_false_positive_rate"] == pytest.approx(1.0)


def test_ndcg_does_not_double_count_duplicate_document_evidence() -> None:
    query = _query(relevance=({"path": "auth.md", "grade": 3},))
    results = [_evidence("auth.md", "Tokens"), _evidence("auth.md", "Other")]
    assert ndcg_at_k(results, query, 2) == pytest.approx(1.0)


def test_latency_percentiles_are_interpolated() -> None:
    assert percentile([1, 2, 3, 4], 50) == pytest.approx(2.5)
    assert summarize_latency([1, 2, 3, 4])["p95"] == pytest.approx(3.85)


def test_benchmark_query_jsonl_and_corpus_manifest(tmp_path) -> None:
    query_path = tmp_path / "queries.jsonl"
    query_path.write_text(
        '{"id":"q1","query":"SQLite","category":"keyword","relevance":[]}\n',
        encoding="utf-8",
    )
    root = tmp_path / "wiki"
    root.mkdir()
    (root / "guide.md").write_text("SQLite\n", encoding="utf-8")
    (root / "AGENTWIKI.md").write_text("Reserved\n", encoding="utf-8")
    (root / "agentwiki").mkdir()
    (root / "agentwiki" / "guide.md").write_text("Reserved\n", encoding="utf-8")
    (root / "BENCHMARK_VERSION").write_text("v1\n", encoding="utf-8")
    queries = load_queries(query_path)
    manifest = inspect_corpus(root)
    assert queries[0].id == "q1"
    assert manifest.document_count == 1
    assert manifest.version == "v1"
    assert len(manifest.sha256) == 64


def test_quality_summary_excludes_no_answer_cases_from_main_metrics() -> None:
    answerable = _query()
    no_answer = _query(id="no-answer", relevance=(), expected_no_answer=True)
    summary = summarize_quality(
        [
            (answerable, [_evidence("auth.md", "Tokens")]),
            (no_answer, [_evidence("wrong.md", "Other")]),
        ],
        ks=(1,),
    )
    assert summary["recall@1"] == pytest.approx(1.0)
    assert summary["no_answer_false_positive_rate"] == pytest.approx(1.0)


async def test_run_benchmark_uses_real_runtime_and_writes_report_data(tmp_path) -> None:
    root = tmp_path / "wiki"
    root.mkdir()
    (root / "auth.md").write_text(
        "# Authentication\n\n## Tokens\n\nUse SQLite-backed tokens.\n",
        encoding="utf-8",
    )
    query = _query(
        query="SQLite",
        category="keyword",
        relevance=({"path": "auth.md", "section": "Authentication / Tokens", "grade": 3},),
    )
    report = await run_benchmark(root, (query,), mode="keyword", embedding_model=None)
    assert report.index.documents == 1
    assert report.index.chunks == 2
    assert report.quality["recall@1"] == pytest.approx(1.0)
    assert report.quality["category.keyword.recall@1"] == pytest.approx(1.0)
    assert "# AgentWiki benchmark" in render_markdown(report)


def test_benchmark_cli_writes_json_and_markdown_reports(tmp_path) -> None:
    root = tmp_path / "wiki"
    root.mkdir()
    (root / "guide.md").write_text("# Guide\n\nSQLite setup\n", encoding="utf-8")
    queries = tmp_path / "queries.jsonl"
    queries.write_text(
        '{"id":"q1","query":"SQLite","category":"keyword",'
        '"relevance":[{"path":"guide.md","grade":3}]}\n',
        encoding="utf-8",
    )
    output = tmp_path / "reports" / "keyword.json"
    main(
        [
            "--corpus",
            str(root),
            "--queries",
            str(queries),
            "--mode",
            "keyword",
            "--output",
            str(output),
        ]
    )
    assert output.is_file()
    assert output.with_suffix(".md").is_file()
