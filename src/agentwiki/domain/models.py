"""Pure domain values for Markdown documents and search."""

from pathlib import PurePosixPath
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator

SearchMode = Literal["keyword", "semantic", "hybrid"]
type Frontmatter = dict[str, object]


class NotePath(BaseModel):
    """A normalized Markdown path relative to the document root."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    value: str

    @field_validator("value")
    @classmethod
    def normalize(cls, value: str) -> str:
        raw = value.replace("\\", "/")
        if raw.startswith("/"):
            raise ValueError("note path must stay inside the document root")
        normalized = raw.strip("/")
        path = PurePosixPath(normalized)
        if not normalized or path.is_absolute() or ".." in path.parts:
            raise ValueError("note path must stay inside the document root")
        if path.suffix.lower() != ".md":
            raise ValueError("note path must have a .md suffix")
        return path.as_posix()


class Note(BaseModel):
    """A Markdown document with parsed YAML Frontmatter."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    path: NotePath
    content: str
    frontmatter: Frontmatter = Field(default_factory=dict)

    @property
    def title(self) -> str:
        title = self.frontmatter.get("title")
        return str(title) if title is not None else self.path.value.rsplit("/", 1)[-1][:-3]


class SearchQuery(BaseModel):
    """A normalized document search request."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    text: str
    mode: SearchMode = "keyword"
    limit: int = Field(default=20, ge=1, le=100)


class SearchResult(BaseModel):
    """A document returned by the local index."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    path: NotePath
    title: str
    score: float
    frontmatter: Frontmatter
    snippet: str
