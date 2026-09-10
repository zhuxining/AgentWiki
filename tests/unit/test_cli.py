import json

from typer.testing import CliRunner

from agentwiki.cli import app

runner = CliRunner()


def test_cli_exposes_maintenance_workflow(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "guide.md").write_text("Use SQLite\n", encoding="utf-8")
    index = tmp_path / "index.sqlite3"
    common = ["--root", str(root), "--index", str(index)]
    queried = runner.invoke(app, ["query", "SQLite", *common])
    assert queried.exit_code == 0, queried.stdout
    assert json.loads(queried.stdout)["results"][0]["path"] == "guide.md"
    help_result = runner.invoke(app, ["--help"])
    assert help_result.exit_code == 0
    for removed in ("write-note", "edit-note", "delete-note", "move-note", "list-directory"):
        assert removed not in help_result.stdout
