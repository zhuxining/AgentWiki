import json
from pathlib import Path

from agentwiki.config import Settings


def test_settings_loads_json_and_resolves_paths_from_project_root(tmp_path) -> None:
    config_path = tmp_path / ".agentwiki" / "config.json"
    config_path.parent.mkdir()
    config_path.write_text(
        json.dumps(
            {
                "document_root": "wiki",
                "index_path": ".agentwiki/search.sqlite3",
                "embedding_model": "fake-model",
            }
        ),
        encoding="utf-8",
    )

    settings = Settings.load(config_path)

    assert settings.document_root == tmp_path / "wiki"
    assert settings.index_path == tmp_path / ".agentwiki" / "search.sqlite3"
    assert settings.embedding_model == "fake-model"


def test_settings_defaults_to_user_agentwiki_directory(tmp_path) -> None:
    config_path = tmp_path / ".agentwiki" / "config.json"
    settings = Settings.load(config_path)

    assert json.loads(config_path.read_text(encoding="utf-8")) == {
        "document_root": "~/AgentWiki",
        "embedding_model": None,
    }
    assert settings.document_root == Path.home() / "AgentWiki"
    assert settings.index_path == Path.home() / ".agentwiki" / "agentwiki.sqlite3"


def test_settings_defaults_index_under_document_root(tmp_path) -> None:
    config_path = tmp_path / ".agentwiki" / "config.json"
    config_path.parent.mkdir()
    config_path.write_text('{"document_root":"wiki"}', encoding="utf-8")

    settings = Settings.load(config_path)

    assert settings.index_path == Path.home() / ".agentwiki" / "agentwiki.sqlite3"


def test_settings_never_read_environment_variables(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("AGENTWIKI_DOCUMENT_ROOT", "/environment-must-not-win")
    config_path = tmp_path / ".agentwiki" / "config.json"
    config_path.parent.mkdir()
    config_path.write_text('{"document_root":"documents"}', encoding="utf-8")

    settings = Settings.load(config_path)

    assert settings.document_root == tmp_path / "documents"
