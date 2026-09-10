"""JSON configuration loaded from the local AgentWiki project."""

import json
from json import JSONDecodeError
from pathlib import Path

from pydantic import BaseModel, ConfigDict, field_validator

DEFAULT_CONFIG_PATH = Path("~/.agentwiki/config.json")
DEFAULT_INDEX_PATH = Path("~/.agentwiki/agentwiki.sqlite3")
DEFAULT_DOCUMENT_ROOT = Path("~/AgentWiki")


class Settings(BaseModel):
    """Validated runtime configuration with project-root-relative paths."""

    model_config = ConfigDict(extra="forbid")

    document_root: Path = DEFAULT_DOCUMENT_ROOT
    index_path: Path | None = None
    embedding_model: str | None = None

    @field_validator("document_root", "index_path", mode="before")
    @classmethod
    def normalize_path(cls, value: str | Path | None) -> Path | None:
        return Path(value).expanduser() if value is not None else None

    @classmethod
    def load(cls, config_path: Path = DEFAULT_CONFIG_PATH) -> Settings:
        """Load JSON without consulting process environment variables."""
        resolved_config = config_path.expanduser().resolve()
        project_root = resolved_config.parent.parent
        if resolved_config.is_file():
            try:
                raw: object = json.loads(resolved_config.read_text(encoding="utf-8"))
            except JSONDecodeError as exc:
                raise ValueError(f"invalid AgentWiki config JSON: {resolved_config}") from exc
            if not isinstance(raw, dict):
                raise ValueError("AgentWiki config must be a JSON object")
            settings = cls.model_validate(raw)
        else:
            settings = cls()
            resolved_config.parent.mkdir(parents=True, exist_ok=True)
            resolved_config.write_text(
                json.dumps(
                    {
                        "document_root": str(DEFAULT_DOCUMENT_ROOT),
                        "embedding_model": None,
                    },
                    ensure_ascii=False,
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
        document_root = cls._resolve_project_path(settings.document_root, project_root)
        index_path = (
            DEFAULT_INDEX_PATH.expanduser().resolve()
            if settings.index_path is None
            else cls._resolve_project_path(settings.index_path, project_root)
        )
        return settings.model_copy(
            update={"document_root": document_root, "index_path": index_path}
        )

    @staticmethod
    def _resolve_project_path(path: Path, project_root: Path) -> Path:
        expanded = path.expanduser()
        return expanded.resolve() if expanded.is_absolute() else (project_root / expanded).resolve()
