"""End-to-end guarantees for the keyword projection.

SQLite FTS5's default tokenizer treats a run of CJK characters as one token, which
made every Chinese query return nothing from the body text. These tests pin the
bigram projection and the guarantees layered on top of it.
"""

import pytest

from agentwiki.domain.retrieval import ContextQuery
from agentwiki.domain.text import analyze, query_expression, run_grams
from agentwiki.runtime.context import create_runtime

AUTH_DOCUMENT = """---
title: 认证方案
type: decision
tags: [architecture]
---
# 认证方案

## 刷新令牌

认证方案使用刷新令牌，令牌过期后需要重新登录。
"""

DEPLOY_DOCUMENT = """---
title: 部署指南
type: guide
tags: [ops]
---
# 部署指南

## 回滚

部署失败时执行回滚脚本，并通知值班同学。
"""


async def _runtime(tmp_path, documents: dict[str, str]):
    root = tmp_path / "wiki"
    for relative, text in documents.items():
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return await create_runtime(root, tmp_path / "index.sqlite3", None)


@pytest.mark.parametrize(
    "query",
    ["令牌", "刷新令牌", "令牌过期", "重新登录", "部署失败", "回滚脚本", "通知值班"],
)
async def test_chinese_body_terms_are_retrievable(tmp_path, query: str) -> None:
    """A query that appears only in body text must match without hint from the title."""
    runtime = await _runtime(tmp_path, {"auth.md": AUTH_DOCUMENT, "deploy.md": DEPLOY_DOCUMENT})
    try:
        await runtime.synchronizer.rebuild()
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query=query))

        assert result.results, f"expected evidence for {query!r}"
        assert any("keyword" in item.match_sources for item in result.results)
    finally:
        await runtime.close()


async def test_chinese_query_does_not_require_the_title_to_match(tmp_path) -> None:
    """The matched section title must be irrelevant to the keyword match."""
    runtime = await _runtime(tmp_path, {"notes.md": "# 杂记\n\n统一使用 uv 管理依赖与虚拟环境。\n"})
    try:
        await runtime.synchronizer.rebuild()
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="虚拟环境"))

        assert [item.path for item in result.results] == ["notes.md"]
        assert "虚拟环境" in result.results[0].snippet
    finally:
        await runtime.close()


async def test_strict_phrases_require_every_part(tmp_path) -> None:
    """The strict pass needs every query part, so it stays precise by construction."""
    runtime = await _runtime(tmp_path, {"auth.md": AUTH_DOCUMENT, "deploy.md": DEPLOY_DOCUMENT})
    try:
        await runtime.synchronizer.rebuild()
        strict = await runtime.repository._keyword_rows(
            ContextQuery(query="令牌 部署指南"),
            query_expression("令牌 部署指南"),
            20,
        )

        # No single chunk carries both "令牌" and "部署指南".
        assert strict == []
    finally:
        await runtime.close()


async def test_loose_fallback_recovers_partial_matches(tmp_path) -> None:
    """A strict pass that finds nothing falls back to OR, ranked by shared terms.

    Returning a partial match beats returning nothing for question-shaped queries; the
    old behaviour ("different wording means no match") is what this replaced.
    """
    runtime = await _runtime(tmp_path, {"auth.md": AUTH_DOCUMENT, "deploy.md": DEPLOY_DOCUMENT})
    try:
        await runtime.synchronizer.rebuild()
        result = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="令牌 部署指南", limit=5)
        )

        assert {item.path for item in result.results} == {"auth.md", "deploy.md"}
        assert all("keyword" in item.match_sources for item in result.results)
    finally:
        await runtime.close()


async def test_english_and_mixed_queries_keep_working(tmp_path) -> None:
    runtime = await _runtime(
        tmp_path,
        {
            "tooling.md": "# Tooling\n\nSQLite FTS5 handles 索引维护 for local search.\n",
        },
    )
    try:
        await runtime.synchronizer.rebuild()
        english = await runtime.retrieval.get_wiki_context(ContextQuery(query="SQLite"))
        mixed = await runtime.retrieval.get_wiki_context(ContextQuery(query="索引维护 FTS5"))

        assert [item.path for item in english.results] == ["tooling.md"]
        assert [item.path for item in mixed.results] == ["tooling.md"]
    finally:
        await runtime.close()


def test_analyze_splits_script_streams_and_words() -> None:
    """Script columns carry characters and bigrams; latin tokens go to the word column."""
    assert analyze("增量索引") == ("增 量 索 引", "增量 量索 索引", "")
    assert analyze("认证") == ("认 证", "认证", "")
    assert analyze("配置 FTS5") == ("配 置", "配置", "FTS5")


def test_query_expression_uses_column_scoped_phrases() -> None:
    """The character phrase pins order, the bigram phrase pins adjacency."""
    expression = query_expression("刷新令牌")

    assert expression == (
        '(search_chars: "刷 新 令 牌" AND search_bigrams: "刷新 新令 令牌")'
    )


def test_query_expression_and_joins_runs_and_word_tokens() -> None:
    assert query_expression("配置 FTS5") == (
        '(search_chars: "配 置" AND search_bigrams: "配置")'
        ' AND search_words: "FTS5"'
    )


def test_single_character_run_needs_no_bigram_clause() -> None:
    assert query_expression("生") == 'search_chars: "生"'


def test_run_grams_are_overlapping_bigrams() -> None:
    assert run_grams("适者生存") == ("适者", "者生", "生存")
    assert run_grams("生") == ("生",)


async def test_subject_words_are_not_stripped_as_recency_intent(tmp_path) -> None:
    """更新/变更 are subject matter in Chinese docs, not only recency words.

    Stripping them turned "更新流程" into a pure recency query and could return
    unfiltered recent documents instead of the matching section.
    """
    runtime = await _runtime(
        tmp_path,
        {
            "config.md": (
                "---\ntitle: 配置管理规范\ntype: reference\ntags: [config]\n---\n"
                "# 配置管理规范\n\n## 更新流程\n\n更新配置前先备份，变更需要评审。\n"
            ),
            "other.md": (
                "---\ntitle: 无关文档\ntype: note\ntags: [x]\n---\n# 无关文档\n\n别的正文。\n"
            ),
        },
    )
    try:
        await runtime.synchronizer.rebuild()
        for query in ("更新流程", "更新配置", "变更"):
            result = await runtime.retrieval.get_wiki_context(ContextQuery(query=query, limit=5))
            assert result.results, f"expected evidence for {query!r}"
            assert result.results[0].path == "config.md", f"{query!r} -> {result.strategy}"
            assert "keyword" in result.results[0].match_sources

        # A query that is nothing but a recency word still means "what changed lately".
        recent_only = await runtime.retrieval.get_wiki_context(ContextQuery(query="最近"))
        assert recent_only.strategy == "recent"
        assert recent_only.results
    finally:
        await runtime.close()


async def test_topic_absent_from_the_wiki_still_misses(tmp_path) -> None:
    """The boundary is now "no shared term", not "not verbatim".

    A query that shares no term with any document still returns nothing, even after the
    loose fallback; a query that shares some terms returns partial matches.
    """
    runtime = await _runtime(
        tmp_path,
        {
            "config.md": (
                "---\ntitle: 配置管理规范\ntype: reference\ntags: [config]\n---\n"
                "# 配置管理规范\n\n## 更新流程\n\n更新配置前先备份，变更需要评审。\n"
            ),
        },
    )
    try:
        await runtime.synchronizer.rebuild()
        present = await runtime.retrieval.get_wiki_context(ContextQuery(query="变更"))
        partial = await runtime.retrieval.get_wiki_context(ContextQuery(query="变更管理"))
        absent = await runtime.retrieval.get_wiki_context(ContextQuery(query="量子纠缠实验"))

        assert present.results
        # Shared term 变更 keeps a partial match alive after the loose fallback.
        assert partial.results
        assert partial.results[0].path == "config.md"
        assert not absent.results
    finally:
        await runtime.close()
