"""Core values for Markdown documents stored in a Wiki library."""

from pathlib import PurePosixPath
from typing import NamedTuple

from pydantic import BaseModel, ConfigDict, Field, field_validator

type Frontmatter = dict[str, object]


class DocumentPath(BaseModel):
    """A normalized Markdown path relative to the configured Wiki root."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    value: str

    @field_validator("value")
    @classmethod
    def normalize(cls, value: str) -> str:
        raw = value.replace("\\", "/")
        if raw.startswith("/"):
            raise ValueError("document path must stay inside the Wiki root")
        normalized = raw.strip("/")
        path = PurePosixPath(normalized)
        if not normalized or path.is_absolute() or ".." in path.parts:
            raise ValueError("document path must stay inside the Wiki root")
        if path.suffix.lower() != ".md":
            raise ValueError("document path must have a .md suffix")
        return path.as_posix()


class WikiDocument(BaseModel):
    """A parsed Markdown document and its filesystem identity."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    path: DocumentPath
    content: str
    frontmatter: Frontmatter = Field(default_factory=dict)
    modified_at_ns: int
    size: int

    @property
    def title(self) -> str:
        title = self.frontmatter.get("title")
        return str(title) if title is not None else self.path.value.rsplit("/", 1)[-1][:-3]


class DocumentDescriptor(NamedTuple):
    path: DocumentPath
    modified_at_ns: int
    size: int


class DocumentFingerprint(NamedTuple):
    modified_at_ns: int
    size: int


class DocumentReadFailure(NamedTuple):
    path: str
    reason: str


class SyncReport(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    indexed: int = 0
    removed: int = 0
    unchanged: int = 0
    degraded: tuple[str, ...] = ()
