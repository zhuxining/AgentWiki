"""JSON configuration loaded from the local AgentWiki project."""

import json
from json import JSONDecodeError
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field, field_validator

from agentwiki.domain.retrieval import DEFAULT_MIN_SIMILARITY

DEFAULT_CONFIG_PATH = Path("~/.agentwiki/config.json")
DEFAULT_INDEX_PATH = Path("~/.agentwiki/agentwiki.sqlite3")
DEFAULT_DOCUMENT_ROOT = Path("~/AgentWiki")
# Re-exported from the domain layer, which owns it because `ContextQuery` uses the same value
# as its field default. Keeping one definition makes it impossible for a direct
# `ContextQuery(...)` caller to run at a different threshold than the configured entry
# points, which is a drift that has already happened once.
# Chinese-first default: `BAAI/bge-small-zh-v1.5` is 512 dimensions (~90 MB, smaller than
# the multilingual model it replaced) and retrieves the Chinese paraphrases in
# tests/unit/test_semantic_retrieval.py at least as well. The tradeoff is English - an
# English query now scores noticeably higher against Chinese documents, so the similarity
# threshold carries more of the abstention burden. Set `embedding_model` to
# `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2` (384 dimensions) for a wiki
# whose searches are routinely mixed Chinese/English, or to `null` to disable semantic
# search entirely.
DEFAULT_EMBEDDING_MODEL = "BAAI/bge-small-zh-v1.5"


class Settings(BaseModel):
    """Validated runtime configuration with project-root-relative paths."""

    model_config = ConfigDict(extra="forbid")

    document_root: Path = DEFAULT_DOCUMENT_ROOT
    index_path: Path = DEFAULT_INDEX_PATH
    embedding_model: str | None = DEFAULT_EMBEDDING_MODEL
    min_similarity: float = Field(default=DEFAULT_MIN_SIMILARITY, ge=0.0, le=1.0)

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
            resolved_config.parent.mkdir(parents=True, exist_ok=True)
            resolved_config.write_text(
                json.dumps(
                    {
                        "document_root": str(DEFAULT_DOCUMENT_ROOT),
                        "embedding_model": DEFAULT_EMBEDDING_MODEL,
                    },
                    ensure_ascii=False,
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
        document_root = cls._resolve_project_path(settings.document_root, project_root)
        index_path = cls._resolve_project_path(settings.index_path, project_root)
        return settings.model_copy(
            update={"document_root": document_root, "index_path": index_path}
        )

    @staticmethod
    def _resolve_project_path(path: Path, project_root: Path) -> Path:
        expanded = path.expanduser()
        return expanded.resolve() if expanded.is_absolute() else (project_root / expanded).resolve()
