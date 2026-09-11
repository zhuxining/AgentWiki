"""CLI command coverage.

Each command is exercised through the real Typer app with an isolated HOME and a
temporary Wiki, because the composition root (`_runtime`) is exactly the layer that had
no tests when the last round of defects shipped.
"""

import json
from pathlib import Path

import pytest
import typer
from typer.testing import CliRunner

from agentwiki.cli import app

runner = CliRunner()

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
def wiki(tmp_path, monkeypatch) -> tuple[Path, Path]:
    """An isolated Wiki plus index path, with semantic search disabled for speed."""
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
    return root, tmp_path / ".agentwiki" / "test.sqlite3"


def _run(args: list[str], wiki: tuple[Path, Path]):
    root, index = wiki
    return runner.invoke(app, [*args, "--root", str(root), "--index", str(index)])


def test_query_returns_evidence(wiki) -> None:
    result = _run(["query", "刷新令牌"], wiki)

    assert result.exit_code == 0, result.stdout
    payload = json.loads(result.stdout)
    assert payload["results"][0]["path"] == "auth.md"
    assert payload["matched"] is True


def test_query_without_text_lists_recent_documents(wiki) -> None:
    result = _run(["query"], wiki)

    assert result.exit_code == 0, result.stdout
    assert json.loads(result.stdout)["strategy"] == "recent"


def test_query_rejects_malformed_metadata(wiki) -> None:
    result = _run(["query", "认证", "--metadata", "{not json"], wiki)

    assert result.exit_code == 2, result.output
    assert "metadata must be valid JSON" in result.output


def test_query_rejects_non_object_metadata(wiki) -> None:
    result = _run(["query", "认证", "--metadata", "[1, 2]"], wiki)

    assert result.exit_code == 2, result.output
    assert "metadata must be a JSON object" in result.output


def test_sync_index_reports_progress(wiki) -> None:
    result = _run(["sync-index"], wiki)

    assert result.exit_code == 0, result.stdout
    payload = json.loads(result.stdout)
    assert payload["indexed_once"] is True
    assert payload["degraded"] == []


def test_rebuild_index_repopulates_the_projection(wiki) -> None:
    _run(["sync-index"], wiki)

    result = _run(["rebuild-index"], wiki)

    assert result.exit_code == 0, result.stdout
    assert json.loads(result.stdout)["indexed"] == 1


def test_rules_returns_the_control_file(wiki) -> None:
    result = _run(["rules"], wiki)

    assert result.exit_code == 0, result.stdout
    payload = json.loads(result.stdout)
    assert payload["name"] == "Test Wiki"
    assert payload["required_fields"] == ["title"]
    assert payload["guide_content"].startswith("# Guide")
    assert payload["source_size"] > 0


def test_rules_accepts_a_scope(wiki) -> None:
    result = _run(["rules", "auth"], wiki)

    assert result.exit_code == 0, result.stdout
    assert json.loads(result.stdout)["name"] == "Test Wiki"


def test_validate_wiki_passes_for_a_conforming_document(wiki) -> None:
    result = _run(["validate-wiki", "auth.md"], wiki)

    assert result.exit_code == 0, result.stdout
    assert json.loads(result.stdout)["status"] == "passed"


def test_validate_wiki_reports_missing_required_fields(wiki) -> None:
    root, _ = wiki
    (root / "incomplete.md").write_text("# 无 frontmatter\n\n正文。\n", encoding="utf-8")

    result = _run(["validate-wiki", "incomplete.md"], wiki)

    assert result.exit_code == 0, result.stdout
    payload = json.loads(result.stdout)
    assert payload["status"] == "failed"
    assert [issue["code"] for issue in payload["errors"]] == ["frontmatter.required"]


def test_validate_wiki_full_checks_every_document(wiki) -> None:
    result = _run(["validate-wiki", "--full"], wiki)

    assert result.exit_code == 0, result.stdout
    payload = json.loads(result.stdout)
    assert "auth.md" in payload["checked_paths"]


def test_validate_wiki_accepts_a_path_that_is_not_indexed(wiki) -> None:
    root, _ = wiki
    (root / "notes").mkdir()
    (root / "notes" / "fresh.md").write_text(
        "---\ntitle: Fresh\n---\n\nBody\n", encoding="utf-8"
    )

    result = _run(["validate-wiki", "notes/fresh.md"], wiki)

    assert result.exit_code == 0, result.stdout
    assert json.loads(result.stdout)["checked_paths"] == ["notes/fresh.md"]


def test_removed_write_commands_are_not_exposed() -> None:
    help_result = runner.invoke(app, ["--help"])

    assert help_result.exit_code == 0
    for removed in ("write-note", "edit-note", "delete-note", "move-note", "list-directory"):
        assert removed not in help_result.stdout


def test_scope_outside_the_root_is_rejected(wiki) -> None:
    """A traversal scope must fail loudly rather than read outside the Wiki."""
    result = _run(["query", "认证", "--scope", "../secrets"], wiki)

    assert result.exit_code != 0
    assert isinstance(result.exception, ValueError)
    assert "inside the Wiki root" in str(result.exception)


def test_metadata_argument_parses_json_into_frontmatter() -> None:
    """`_metadata` is a private adapter, but it is the only JSON->model boundary."""
    from agentwiki.cli import _metadata

    assert _metadata(None) == {}
    assert _metadata('{"owner": "alice"}') == {"owner": "alice"}
    with pytest.raises(typer.BadParameter):
        _metadata("{bad")
    with pytest.raises(typer.BadParameter):
        _metadata("[]")
