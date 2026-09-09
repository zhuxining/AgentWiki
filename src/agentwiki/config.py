"""Runtime configuration for the local AgentWiki process."""

from __future__ import annotations

from pathlib import Path

from pydantic import field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    """Configuration shared by CLI, MCP, services, and repositories."""

    model_config = SettingsConfigDict(env_prefix="AGENTWIKI_", extra="ignore")

    document_root: Path = Path("documents")
    index_path: Path = Path(".agentwiki/index.sqlite3")
    embedding_model: str | None = None

    @field_validator("document_root", "index_path", mode="before")
    @classmethod
    def expand_path(cls, value: str | Path) -> Path:
        """Expand a user home marker before Pydantic validates the path."""
        return Path(value).expanduser()

    @classmethod
    def from_env(cls) -> Settings:
        """Build settings from environment variables with local defaults."""
        return cls()
