from __future__ import annotations

import json

from typer.testing import CliRunner

from agentwiki.cli import app

runner = CliRunner()


def test_cli_supports_document_lifecycle(tmp_path) -> None:
    root = tmp_path / "documents"
    index = tmp_path / "index.sqlite3"
    common = ["--root", str(root), "--index", str(index)]

    created = runner.invoke(
        app,
        [
            "write-note",
            "guide.md",
            "--content",
            "Use SQLite",
            "--metadata",
            '{"kind":"guide"}',
            *common,
        ],
    )
    assert created.exit_code == 0, created.stdout

    searched = runner.invoke(app, ["search-notes", "SQLite", *common])
    assert searched.exit_code == 0, searched.stdout
    assert json.loads(searched.stdout)[0]["path"]["value"] == "guide.md"

    moved = runner.invoke(app, ["move-note", "guide.md", "docs/guide.md", *common])
    assert moved.exit_code == 0, moved.stdout

    deleted = runner.invoke(app, ["delete-note", "docs/guide.md", *common])
    assert deleted.exit_code == 0, deleted.stdout
    assert not (root / "docs/guide.md").exists()
