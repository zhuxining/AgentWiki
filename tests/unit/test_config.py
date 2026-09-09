from pathlib import Path

from agentwiki.config import Settings


def test_settings_reads_and_expands_environment_paths(monkeypatch, tmp_path) -> None:
    document_root = tmp_path / "documents"
    index_path = tmp_path / "index.sqlite3"
    monkeypatch.setenv("AGENTWIKI_DOCUMENT_ROOT", str(document_root))
    monkeypatch.setenv("AGENTWIKI_INDEX_PATH", str(index_path))
    monkeypatch.setenv("AGENTWIKI_EMBEDDING_MODEL", "fake-model")

    settings = Settings.from_env()

    assert settings.document_root == Path(document_root)
    assert settings.index_path == Path(index_path)
    assert settings.embedding_model == "fake-model"
