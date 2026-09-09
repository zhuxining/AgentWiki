"""Runtime configuration for the local AgentWiki process."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path


@dataclass(frozen=True, slots=True)
class Settings:
    """Configuration shared by CLI, MCP, services, and repositories."""

    document_root: Path
    index_path: Path
    embedding_model: str | None = None

    @classmethod
    def from_env(cls) -> Settings:
        """Build settings from environment variables with local defaults."""
        root = Path(os.getenv("AGENTWIKI_DOCUMENT_ROOT", "documents")).expanduser()
        index = Path(os.getenv("AGENTWIKI_INDEX_PATH", ".agentwiki/index.sqlite3")).expanduser()
        model = os.getenv("AGENTWIKI_EMBEDDING_MODEL") or None
        return cls(document_root=root, index_path=index, embedding_model=model)
