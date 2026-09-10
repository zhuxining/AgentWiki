"""Tests for loss attribution: which pipeline layer lost a relevance judgment."""

from datetime import UTC, datetime

import pytest

from agentwiki.domain.retrieval import Evidence, MatchSource
from benchmarks.attribution import (
    LossLayer,
    SectionRelation,
    attribute_judgment,
    detect_no_answer_contamination,
    run_attribution,
    section_relation,
    summarize,
)
from benchmarks.schema import BenchmarkQuery, RelevanceJudgment


def _judgment(
    path: str = "a.md",
    section: str = "A / B",
    grade: int = 3,
) -> RelevanceJudgment:
    return RelevanceJudgment(path=path, section=section, grade=grade)


def _evidence(
    path: str,
    section: str,
    *,
    sources: tuple[MatchSource, ...] = ("keyword",),
) -> Evidence:
    return Evidence(
        path=path,
        title=path,
        section=section,
        snippet="evidence",
        rank_score=1.0,
        match_sources=sources,
        modified_at=datetime.fromtimestamp(1, tz=UTC),
        frontmatter={},
    )


def _layer(
    judgment: RelevanceJudgment,
    *,
    corpus_paths: frozenset[str] = frozenset({"a.md"}),
    index_sections: dict[str, tuple[str, ...]] | None = None,
    results: tuple[Evidence, ...] = (),
    k: int = 5,
) -> LossLayer:
    return attribute_judgment(
        judgment,
        query_id="q1",
        corpus_paths=corpus_paths,
        index_sections=index_sections if index_sections is not None else {"a.md": ("A / B",)},
        results=results,
        k=k,
    ).layer


def test_section_relation_distinguishes_ancestry_from_equality() -> None:
    assert section_relation("A / B", "A / B") is SectionRelation.EXACT
    assert section_relation("A / B", "A / B / C") is SectionRelation.DESCENDANT
    assert section_relation("A / B / C", "A / B") is SectionRelation.ANCESTOR
    assert section_relation("A / B", "A / C") is SectionRelation.UNRELATED
    assert section_relation("A / B", "A / BC") is SectionRelation.UNRELATED


def test_judgment_missing_from_corpus_is_reported_as_stale_qrels() -> None:
    assert _layer(_judgment(), corpus_paths=frozenset()) is LossLayer.QRELS_STALE


def test_judgment_whose_file_never_reached_the_index_is_not_indexed() -> None:
    assert _layer(_judgment(), index_sections={}) is LossLayer.NOT_INDEXED


def test_judgment_whose_section_was_never_chunked_is_chunk_missing() -> None:
    assert _layer(_judgment(), index_sections={"a.md": ("A / Z",)}) is LossLayer.CHUNK_MISSING


def test_exact_section_within_k_is_retrieved() -> None:
    results = (_evidence("a.md", "A / B"),)
    assert _layer(_judgment(), results=results) is LossLayer.RETRIEVED


def test_exact_section_beyond_k_is_ranked_below_k() -> None:
    filler = tuple(_evidence("other.md", "O") for _ in range(4))
    results = (*filler, _evidence("a.md", "A / B"))
    assert _layer(_judgment(), results=results, k=4) is LossLayer.RANKED_BELOW_K


def test_child_section_is_a_granularity_loss_not_a_retrieval_loss() -> None:
    """A judgment on a parent heading, answered by its child chunk, is a labeling loss."""
    results = (_evidence("a.md", "A / B / C"),)
    assert _layer(_judgment(), results=results) is LossLayer.GRANULARITY_ONLY


def test_child_judgment_absent_from_index_is_chunk_missing() -> None:
    """Only a parent chunk exists, and a parent chunk never carries its child's body."""
    results = (_evidence("a.md", "A"),)
    attribution = _layer(
        _judgment(),
        results=results,
        index_sections={"a.md": ("A",)},
    )
    assert attribution is LossLayer.CHUNK_MISSING


def test_returned_parent_section_does_not_count_as_a_granularity_match() -> None:
    """The judged child section is indexed, but the returned parent does not cover it."""
    results = (_evidence("a.md", "A"),)
    assert _layer(_judgment(), results=results) is LossLayer.WRONG_SECTION


def test_unrelated_section_of_the_right_document_is_wrong_section() -> None:
    results = (_evidence("a.md", "A / Z"),)
    assert _layer(_judgment(), results=results) is LossLayer.WRONG_SECTION


def test_document_absent_from_results_is_not_recalled() -> None:
    results = (_evidence("other.md", "O"),)
    assert _layer(_judgment(), results=results) is LossLayer.NOT_RECALLED


def test_document_level_judgment_matches_on_path_alone() -> None:
    judgment = _judgment(section="")
    assert _layer(judgment, results=(_evidence("a.md", "A / Z"),)) is LossLayer.RETRIEVED
    assert _layer(judgment, results=()) is LossLayer.NOT_RECALLED


def test_attribution_records_rank_and_match_sources() -> None:
    results = (_evidence("a.md", "A / B", sources=("keyword", "semantic")),)
    attribution = attribute_judgment(
        _judgment(),
        query_id="q1",
        corpus_paths=frozenset({"a.md"}),
        index_sections={"a.md": ("A / B",)},
        results=results,
        k=5,
    )
    assert attribution.rank == 1
    assert attribution.match_sources == ("keyword", "semantic")


def test_no_answer_contamination_flags_terms_that_occur_in_the_corpus() -> None:
    """A no-answer case only tests abstention when its topic is genuinely absent."""
    query = BenchmarkQuery.model_validate(
        {
            "id": "q-na",
            "query": "etcd 备份",
            "category": "no_answer",
            "expected_no_answer": True,
            "relevance": [],
        }
    )
    assert detect_no_answer_contamination((query,), "本文讨论备份策略") == {"q-na": ("备份",)}
    assert detect_no_answer_contamination((query,), "完全无关的内容") == {}


def test_no_answer_contamination_ignores_answerable_queries() -> None:
    query = BenchmarkQuery.model_validate(
        {
            "id": "q",
            "query": "SQLite",
            "category": "keyword",
            "relevance": [{"path": "a.md", "grade": 3}],
        }
    )
    assert detect_no_answer_contamination((query,), "SQLite is used here") == {}


async def test_run_attribution_separates_chunking_from_ranking_losses(tmp_path) -> None:
    """A judged parent heading answered by its child chunk must not read as a miss."""
    root = tmp_path / "wiki"
    root.mkdir()
    (root / "a.md").write_text(
        "# A\n\nintro\n\n## B\n\nSQLite tokens live here.\n",
        encoding="utf-8",
    )
    queries = (
        BenchmarkQuery.model_validate(
            {
                "id": "q1",
                "query": "SQLite",
                "category": "keyword",
                "relevance": [{"path": "a.md", "section": "A / B", "grade": 3}],
            }
        ),
    )
    report = await run_attribution(root, queries, mode="keyword", embedding_model=None, k=5)
    assert report.judgments == 1
    assert report.retrieved_rate == pytest.approx(1.0)
    assert report.index_ceiling_rate == pytest.approx(1.0)
    assert report.no_answer_contamination == {}
    assert "# Retrieval loss attribution" in summarize(report)


async def test_run_attribution_records_a_missing_section_as_chunk_missing(tmp_path) -> None:
    root = tmp_path / "wiki"
    root.mkdir()
    (root / "a.md").write_text("# A\n\nSQLite tokens\n", encoding="utf-8")
    queries = (
        BenchmarkQuery.model_validate(
            {
                "id": "q1",
                "query": "SQLite",
                "category": "keyword",
                "relevance": [{"path": "a.md", "section": "A / Nowhere", "grade": 3}],
            }
        ),
    )
    report = await run_attribution(root, queries, mode="keyword", embedding_model=None, k=5)
    assert report.layer_counts["chunk_missing"] == 1
    assert report.index_ceiling_rate == pytest.approx(0.0)
