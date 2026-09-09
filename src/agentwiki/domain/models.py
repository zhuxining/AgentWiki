"""Pure domain values for Markdown documents and search."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import PurePosixPath
from typing import Literal

SearchMode = Literal["keyword", "semantic", "hybrid"]


@dataclass(frozen=True, slots=True)
class NotePath:
    """A normalized Markdown path relative to the document root."""

    value: str

    def __post_init__(self) -> None:
        raw = self.value.replace("\\", "/")
        if raw.startswith("/"):
            raise ValueError("note path must stay inside the document root")
        normalized = raw.strip("/")
        path = PurePosixPath(normalized)
        if not normalized or path.is_absolute() or ".." in path.parts:
            raise ValueError("note path must stay inside the document root")
        if path.suffix.lower() != ".md":
            raise ValueError("note path must have a .md suffix")
        object.__setattr__(self, "value", path.as_posix())


@dataclass(frozen=True, slots=True)
class Note:
    """A Markdown document with parsed YAML Frontmatter."""

    path: NotePath
    content: str
    frontmatter: dict[str, object] = field(default_factory=dict)

    @property
    def title(self) -> str:
        title = self.frontmatter.get("title")
        return str(title) if title is not None else self.path.value.rsplit("/", 1)[-1][:-3]


@dataclass(frozen=True, slots=True)
class SearchQuery:
    """A normalized document search request."""

    text: str
    mode: SearchMode = "keyword"
    limit: int = 20

    def __post_init__(self) -> None:
        if self.mode not in {"keyword", "semantic", "hybrid"}:
            raise ValueError(f"unsupported search mode: {self.mode}")
        if not 1 <= self.limit <= 100:
            raise ValueError("search limit must be between 1 and 100")


@dataclass(frozen=True, slots=True)
class SearchResult:
    """A document returned by the local index."""

    path: NotePath
    title: str
    score: float
    frontmatter: dict[str, object]
    snippet: str
