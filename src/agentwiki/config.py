"""JSON configuration loaded from the local AgentWiki project."""

import json
from json import JSONDecodeError
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field, field_validator

DEFAULT_CONFIG_PATH = Path("~/.agentwiki/config.json")
DEFAULT_INDEX_PATH = Path("~/.agentwiki/agentwiki.sqlite3")
DEFAULT_DOCUMENT_ROOT = Path("~/AgentWiki")
# Minimum cosine similarity for a vector hit to count. Vectors are L2-normalized and
# sqlite-vec reports L2 distance, so this is directly comparable across queries.
#
# Calibrated against the default multilingual model on a small Chinese corpus: true
# paraphrases scored 0.34-0.58 while an unrelated query peaked at 0.10. 0.30 keeps every
# true paraphrase and still rejects the unrelated query. The value is model-dependent -
# re-measure before changing the embedding model.
DEFAULT_MIN_SIMILARITY = 0.30
# A multilingual model, because the searches this layer serves are routinely mixed
# Chinese/English: `bge-small-zh` is stronger on Chinese but weaker on English, and
# fastembed's English-only models cannot embed Chinese at all. 384 dimensions keeps the
# vector table small (the model is ~220 MB).
DEFAULT_EMBEDDING_MODEL = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"


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
