"""Stable application ports implemented by local adapters."""

from typing import Literal, Protocol

from agentwiki.domain.documents import (
    DocumentDescriptor,
    DocumentFingerprint,
    DocumentPath,
    Frontmatter,
    SyncReport,
    WikiDocument,
)
from agentwiki.domain.retrieval import (
    ContextQuery,
    IndexedChunk,
    RelatedDocument,
    SearchCandidate,
)
from agentwiki.domain.tags import TagAliases
from agentwiki.indexing.graph import GraphEdgeDraft


class SearchRepository(Protocol):
    @property
    def semantic_available(self) -> bool: ...

    async def fingerprints(self) -> dict[str, DocumentFingerprint]: ...

    async def vector_stale_paths(self) -> set[str]: ...

    async def vector_state(
        self, path: str
    ) -> Literal["ready", "pending", "error", "unavailable", "none"]: ...

    async def mark_document_error(self, path: str, error: str) -> None: ...

    async def replace_document(
        self,
        document: WikiDocument,
        chunks: tuple[IndexedChunk, ...],
        edges: tuple[GraphEdgeDraft, ...] = (),
        moved_from: str | None = None,
    ) -> str | None: ...

    async def resolve_edges(self) -> None: ...

    async def related_documents(
        self, paths: tuple[str, ...], *, limit: int = 5
    ) -> dict[str, tuple[RelatedDocument, ...]]: ...

    async def delete_paths(self, paths: tuple[str, ...], *, commit: bool = True) -> None: ...

    async def clear(self) -> None: ...

    async def exact_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def keyword_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def graph_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def semantic_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def recent_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def wait_for_initial_vector_sync(self) -> None: ...

    async def wait_for_vector_sync(self) -> None: ...

    async def close(self) -> None: ...


class FreshnessSynchronizer(Protocol):
    async def ensure_fresh(self) -> SyncReport: ...

    async def rebuild(self) -> SyncReport: ...


class TagRulesProvider(Protocol):
    def get_tag_aliases(self) -> TagAliases: ...


class WikiLibrary(Protocol):
    def descriptors(self) -> tuple[DocumentDescriptor, ...]: ...

    def descriptor(self, document_path: DocumentPath) -> DocumentDescriptor: ...

    def read(self, descriptor: DocumentDescriptor | DocumentPath) -> WikiDocument: ...

    def raw(self, document_path: DocumentPath) -> str: ...

    def reserved_text(self, name: str) -> str: ...

    def parse(self, raw: str) -> tuple[str, Frontmatter]: ...
