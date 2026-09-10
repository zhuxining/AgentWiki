"""JSON configuration loaded from the local AgentWiki project."""

import json
from json import JSONDecodeError
from pathlib import Path

from pydantic import BaseModel, ConfigDict, field_validator

DEFAULT_CONFIG_PATH = Path(".agentwiki/config.json")


class Settings(BaseModel):
    """Validated runtime configuration with project-root-relative paths."""

    model_config = ConfigDict(extra="forbid")

    document_root: Path = Path("documents")
    index_path: Path = Path(".agentwiki/index.sqlite3")
    embedding_model: str | None = None

    @field_validator("document_root", "index_path", mode="before")
    @classmethod
    def normalize_path(cls, value: str | Path) -> Path:
        return Path(value).expanduser()

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
        return settings.model_copy(
            update={
                "document_root": cls._resolve_project_path(
                    settings.document_root, project_root
                ),
                "index_path": cls._resolve_project_path(settings.index_path, project_root),
            }
        )

    @staticmethod
    def _resolve_project_path(path: Path, project_root: Path) -> Path:
        return path.resolve() if path.is_absolute() else (project_root / path).resolve()
