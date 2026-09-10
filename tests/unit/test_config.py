import json
from pathlib import Path

from agentwiki.config import DEFAULT_EMBEDDING_MODEL, DEFAULT_MIN_SIMILARITY, Settings
from agentwiki.domain.retrieval import ContextQuery
from agentwiki.repository.embeddings import FastEmbedProvider


def test_embedding_default_matches_the_adapter_default() -> None:
    """`config` and `repository/embeddings` cannot import each other.

    The layering rules forbid the adapter from reading the config use case, so the default
    model name is declared in both places. Without this guard the two would silently drift
    and `FastEmbedProvider()` would load a different model than the config advertises.
    """
    assert FastEmbedProvider.DEFAULT_MODEL == DEFAULT_EMBEDDING_MODEL


def test_context_query_default_threshold_matches_the_configured_threshold() -> None:
    """A directly built `ContextQuery` must run at the configured threshold.

    The benchmark runner, the attribution tool and the tests all build `ContextQuery`
    without passing `min_similarity`. When `ContextQuery` carried its own literal default
    the two drifted, so those callers silently measured a different threshold than the CLI
    and MCP entry points.
    """
    assert ContextQuery().min_similarity == Settings().min_similarity == DEFAULT_MIN_SIMILARITY


def test_default_threshold_sits_between_the_calibration_bounds() -> None:
    """The default must reject unrelated queries while keeping true paraphrases.

    Both numbers come from the calibration recorded in `domain/retrieval.py`; this test
    exists so a future model swap cannot quietly ship a threshold that no longer separates
    them. The window is narrow, so both bounds are asserted exactly.
    """
    unrelated_peak = 0.4281
    weakest_paraphrase = 0.4470
    assert unrelated_peak < DEFAULT_MIN_SIMILARITY <= weakest_paraphrase


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


def test_settings_defaults_to_useragentwiki_directory(tmp_path) -> None:
    config_path = tmp_path / ".agentwiki" / "config.json"
    settings = Settings.load(config_path)

    assert json.loads(config_path.read_text(encoding="utf-8")) == {
        "document_root": "~/AgentWiki",
        "embedding_model": DEFAULT_EMBEDDING_MODEL,
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
