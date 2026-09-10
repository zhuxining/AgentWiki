"""Pure quality and latency metrics for benchmark results."""

from collections.abc import Iterable, Sequence
import math

from agentwiki.domain.retrieval import Evidence
from benchmarks.schema import BenchmarkQuery


def relevance_grade(result: Evidence, query: BenchmarkQuery) -> int:
    """Return the strongest judgment matching one returned evidence item."""
    grades = [
        judgment.grade
        for judgment in query.relevance
        if judgment.path == result.path
        and (not judgment.section or judgment.section == result.section)
    ]
    return max(grades, default=0)


def recall_at_k(results: Sequence[Evidence], query: BenchmarkQuery, k: int) -> float:
    """Measure whether a grade-2-or-better evidence item was retrieved."""
    if not query.relevance:
        return 0.0
    return float(any(relevance_grade(item, query) >= 2 for item in results[:k]))


def strict_recall_at_k(results: Sequence[Evidence], query: BenchmarkQuery, k: int) -> float:
    """Measure whether a highly relevant grade-3 item was retrieved."""
    if not query.relevance:
        return 0.0
    return float(any(relevance_grade(item, query) == 3 for item in results[:k]))


def document_recall_at_k(results: Sequence[Evidence], query: BenchmarkQuery, k: int) -> float:
    """Measure document recall while ignoring section-level differences."""
    relevant_paths = {judgment.path for judgment in query.relevance if judgment.grade >= 2}
    if not relevant_paths:
        return 0.0
    return float(any(item.path in relevant_paths for item in results[:k]))


def mean_reciprocal_rank(results: Sequence[Evidence], query: BenchmarkQuery, k: int) -> float:
    """Return the reciprocal rank of the first relevant evidence item."""
    for rank, item in enumerate(results[:k], start=1):
        if relevance_grade(item, query) >= 2:
            return 1.0 / rank
    return 0.0


def ndcg_at_k(results: Sequence[Evidence], query: BenchmarkQuery, k: int) -> float:
    """Compute graded normalized discounted cumulative gain."""
    actual: list[int] = []
    seen_judgments: set[tuple[str, str]] = set()
    for item in results[:k]:
        matching = [
            judgment
            for judgment in query.relevance
            if judgment.path == item.path
            and (not judgment.section or judgment.section == item.section)
        ]
        if not matching:
            actual.append(0)
            continue
        judgment = max(matching, key=lambda value: value.grade)
        judgment_key = (judgment.path, judgment.section)
        if judgment_key in seen_judgments:
            actual.append(0)
            continue
        seen_judgments.add(judgment_key)
        actual.append(judgment.grade)
    ideal = sorted((judgment.grade for judgment in query.relevance), reverse=True)[:k]
    if not ideal:
        return 0.0

    def dcg(grades: Iterable[int]) -> float:
        return sum(
            (2**grade - 1) / math.log2(rank + 1)
            for rank, grade in enumerate(grades, start=1)
        )

    ideal_gain = dcg(ideal)
    return dcg(actual) / ideal_gain if ideal_gain else 0.0


def section_precision_at_k(results: Sequence[Evidence], query: BenchmarkQuery, k: int) -> float:
    """Return the fraction of returned evidence items graded as relevant."""
    selected = results[:k]
    if not selected:
        return 0.0
    return sum(relevance_grade(item, query) >= 2 for item in selected) / len(selected)


def percentile(values: Sequence[float], percentile_value: float) -> float:
    """Compute a linearly interpolated percentile without external dependencies."""
    if not values:
        return 0.0
    if not 0 <= percentile_value <= 100:
        raise ValueError("percentile must be between 0 and 100")
    ordered = sorted(values)
    position = (len(ordered) - 1) * percentile_value / 100
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    fraction = position - lower
    return ordered[lower] + (ordered[upper] - ordered[lower]) * fraction


def summarize_quality(
    cases: Sequence[tuple[BenchmarkQuery, Sequence[Evidence]]],
    *,
    ks: Sequence[int] = (1, 3, 5, 10),
) -> dict[str, float]:
    """Aggregate retrieval metrics across a query set."""
    if not cases:
        return {}
    summary: dict[str, float] = {}
    answerable_cases = [
        (query, results) for query, results in cases if not query.expected_no_answer
    ]
    summary.update(_quality_values(answerable_cases, ks))

    groups: list[tuple[str, str]] = [
        ("category", str(query.category)) for query, _ in cases
    ]
    groups.extend(("difficulty", str(query.difficulty)) for query, _ in cases)
    for group_name, group_value in sorted(set(groups)):
        summary.update(
            {
                f"{group_name}.{group_value}.{key}": value
                for key, value in _quality_values(
                    [
                        (query, results)
                        for query, results in cases
                        if getattr(query, group_name) == group_value
                    ],
                    ks,
                ).items()
            }
        )

    no_answer = [
        query for query, _ in cases if query.expected_no_answer
    ]
    if no_answer:
        false_positives = sum(
            bool(results) for query, results in cases if query.expected_no_answer
        )
        summary["no_answer_false_positive_rate"] = false_positives / len(no_answer)
    else:
        summary["no_answer_false_positive_rate"] = 0.0
    return summary


def _quality_values(
    cases: Sequence[tuple[BenchmarkQuery, Sequence[Evidence]]],
    ks: Sequence[int],
) -> dict[str, float]:
    values: dict[str, float] = {}
    for k in ks:
        values[f"recall@{k}"] = _mean(
            recall_at_k(results, query, k) for query, results in cases
        )
        values[f"recall_strict@{k}"] = _mean(
            strict_recall_at_k(results, query, k) for query, results in cases
        )
        values[f"document_recall@{k}"] = _mean(
            document_recall_at_k(results, query, k) for query, results in cases
        )
        values[f"mrr@{k}"] = _mean(
            mean_reciprocal_rank(results, query, k) for query, results in cases
        )
        values[f"ndcg@{k}"] = _mean(ndcg_at_k(results, query, k) for query, results in cases)
        values[f"section_precision@{k}"] = _mean(
            section_precision_at_k(results, query, k) for query, results in cases
        )
    return values


def summarize_latency(latencies_ms: Sequence[float]) -> dict[str, float]:
    """Summarize query latency in milliseconds."""
    if not latencies_ms:
        return {"p50": 0.0, "p95": 0.0, "p99": 0.0, "min": 0.0, "max": 0.0}
    return {
        "p50": percentile(latencies_ms, 50),
        "p95": percentile(latencies_ms, 95),
        "p99": percentile(latencies_ms, 99),
        "min": min(latencies_ms),
        "max": max(latencies_ms),
    }


def _mean(values: Iterable[float]) -> float:
    collected = list(values)
    return sum(collected) / len(collected) if collected else 0.0
