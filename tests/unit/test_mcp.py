"""MCP tool coverage: every tool, plus the fallback runtime path.

`_runtime` reuses the lifespan runtime and only builds its own when the lifespan context
has none. That fallback builds a runtime per call, so it needs a test to stay honest.
"""

from dataclasses import dataclass
import json
from pathlib import Path
from typing import Any, cast

from fastmcp import Client, Context
import pytest

from agentwiki.domain.retrieval import ContextQuery
from agentwiki.mcp import _runtime, mcp
from agentwiki.runtime.context import AgentWikiRuntime, create_runtime

CONTROL = """---
name: Test Wiki
default_type: note
required_fields: [title]
---
# Guide

Use this Wiki.
"""

DOCUMENT = """---
title: 认证方案
type: note
tags: [auth]
---
# 认证方案

## 刷新令牌

认证方案使用刷新令牌，过期后需要重新登录。
"""


@pytest.fixture
def home(tmp_path, monkeypatch) -> Path:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    config_dir = tmp_path / ".agentwiki"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.json").write_text(
        json.dumps({"document_root": "wiki", "embedding_model": None}),
        encoding="utf-8",
    )
    root = tmp_path / "wiki"
    root.mkdir()
    (root / "AGENTWIKI.md").write_text(CONTROL, encoding="utf-8")
    (root / "auth.md").write_text(DOCUMENT, encoding="utf-8")
    return tmp_path


async def test_every_tool_returns_its_payload(home) -> None:
    async with Client(mcp) as client:
        context = await client.call_tool("get_wiki_context", {"query": "刷新令牌"})
        rules = await client.call_tool("get_wiki_rules", {})
        report = await client.call_tool("validate_wiki", {"path": "auth.md"})

    assert context.data["results"][0]["path"] == "auth.md"
    assert context.data["wiki_root"].endswith("wiki")

    assert rules.data["name"] == "Test Wiki"
    assert rules.data["required_fields"] == ["title"]
    assert rules.data["wiki_root"].endswith("wiki")
    assert rules.data["source_size"] > 0

    assert report.data["status"] == "passed"
    assert report.data["checked_paths"] == ["auth.md"]


async def test_get_wiki_context_reports_no_match_instead_of_guessing(home) -> None:
    async with Client(mcp) as client:
        result = await client.call_tool("get_wiki_context", {"query": "量子纠缠实验"})

    assert result.data["matched"] is False
    assert result.data["results"] == []


async def test_validate_wiki_full_checks_every_document(home) -> None:
    async with Client(mcp) as client:
        report = await client.call_tool("validate_wiki", {"full": True})

    assert report.data["status"] == "passed"
    assert "auth.md" in report.data["checked_paths"]


async def test_validate_wiki_reports_missing_required_fields(home) -> None:
    (home / "wiki" / "incomplete.md").write_text("# 无 frontmatter\n", encoding="utf-8")

    async with Client(mcp) as client:
        report = await client.call_tool("validate_wiki", {"path": "incomplete.md"})

    assert report.data["status"] == "failed"
    assert [issue["code"] for issue in report.data["errors"]] == ["frontmatter.required"]


async def test_rules_honour_scope_argument(home) -> None:
    async with Client(mcp) as client:
        rules = await client.call_tool("get_wiki_rules", {"scope": "auth"})

    assert rules.data["name"] == "Test Wiki"


@dataclass
class _FakeContext:
    """Minimal stand-in for a FastMCP context with a chosen lifespan payload."""

    lifespan_context: dict[str, Any]


async def test_runtime_reuses_the_lifespan_runtime(home) -> None:
    """When the lifespan provided a runtime, the tool must not build another one."""
    shared = await create_runtime(home / "wiki", home / "shared.sqlite3", None)
    try:
        ctx = cast(Context, _FakeContext({"agentwiki.runtime": shared}))

        async with _runtime(ctx) as runtime:
            assert runtime is shared
    finally:
        await shared.close()


async def test_runtime_falls_back_when_the_lifespan_has_none(home) -> None:
    """With no lifespan runtime the tool builds its own and closes it afterwards."""
    ctx = cast(Context, _FakeContext({}))
    captured: list[AgentWikiRuntime] = []

    async with _runtime(ctx) as runtime:
        assert isinstance(runtime, AgentWikiRuntime)
        captured.append(runtime)
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="刷新令牌"))
        assert result.matched is True

    # close() released the connection
    with pytest.raises(RuntimeError):
        _ = captured[0].database.connection
