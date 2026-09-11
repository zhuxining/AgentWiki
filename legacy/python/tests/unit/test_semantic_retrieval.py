"""Semantic retrieval with the default Chinese embedding model.

These tests load the real model (cached after the first run) because the bugs they guard
against - a wrong similarity sign, a rank key that was never projected, and a threshold
calibrated for a different model - are all invisible to fake-vector tests.

They are also the calibration corpus for `DEFAULT_MIN_SIMILARITY`: the four paraphrases
below must score above the threshold and `UNRELATED` below it. Changing the model or the
threshold means re-running this file and updating the numbers recorded in `config.py`.
"""

import pytest

from agentwiki.domain.retrieval import ContextQuery
from agentwiki.repository.embeddings import FastEmbedProvider
from agentwiki.runtime.context import create_runtime

DOCUMENTS = {
    "perf.md": "# 性能优化\n\n用本地缓存和批量写入减少磁盘往返。",
    "auth.md": "# 认证设计\n\n登录后签发短期令牌，过期需要重新认证。",
    "deploy.md": "# 发布流程\n\n先灰度一小部分流量，失败就回滚。",
    "report.md": "# 季度报表\n\n本季度营收同比增长。",
}
# Paraphrases with no verbatim term overlap with the target document.
PARAPHRASES = [
    ("怎么让系统跑得更快", "perf.md"),
    ("用户登录以后怎么保持状态", "auth.md"),
    ("上线出问题怎么恢复", "deploy.md"),
    ("这个季度赚了多少钱", "report.md"),
]
UNRELATED = "完全不相关的量子纠缠实验"

# Loading a real ONNX model inside pytest leaves runtime state that does not survive a
# sibling test spawning another interpreter. These tests own their own process.
pytestmark = pytest.mark.integration


@pytest.fixture
def provider() -> FastEmbedProvider:
    return FastEmbedProvider()


async def _runtime(tmp_path, provider):
    root = tmp_path / "wiki"
    root.mkdir()
    for name, body in DOCUMENTS.items():
        (root / name).write_text(body, encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3", provider)
    await runtime.synchronizer.rebuild()
    await runtime.repository.wait_for_vector_sync()
    return runtime


@pytest.mark.parametrize(("query", "expected"), PARAPHRASES)
async def test_chinese_paraphrase_finds_the_right_document(
    tmp_path, provider, query: str, expected: str
) -> None:
    runtime = await _runtime(tmp_path, provider)
    try:
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query=query, limit=3))

        assert result.results, f"no evidence for {query!r}"
        assert result.results[0].path == expected
    finally:
        await runtime.close()


async def test_similarity_is_a_comparable_score(tmp_path, provider) -> None:
    """The vector leg must expose cosine similarity, not a raw L2 distance."""
    runtime = await _runtime(tmp_path, provider)
    try:
        candidates = await runtime.repository.semantic_candidates(
            ContextQuery(query="怎么让系统跑得更快", min_similarity=0.0), candidate_limit=4
        )

        assert candidates, "vector leg returned nothing"
        similarities = [-item.rank_score for item in candidates]
        assert all(0.0 <= value <= 1.0 for value in similarities), similarities
        assert similarities == sorted(similarities, reverse=True), similarities
        assert similarities[0] > 0.3, similarities
    finally:
        await runtime.close()


async def test_unrelated_query_is_refused_by_the_threshold(tmp_path, provider) -> None:
    """An unrelated topic must produce no evidence rather than the nearest neighbour."""
    runtime = await _runtime(tmp_path, provider)
    try:
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query=UNRELATED, limit=3))

        assert result.results == ()
        assert result.matched is False
    finally:
        await runtime.close()


async def test_threshold_can_be_relaxed_per_query(tmp_path, provider) -> None:
    runtime = await _runtime(tmp_path, provider)
    try:
        relaxed = await runtime.retrieval.get_wiki_context(
            ContextQuery(query=UNRELATED, limit=3, min_similarity=0.0)
        )

        assert relaxed.results, "with the threshold off the nearest neighbour should surface"
    finally:
        await runtime.close()
