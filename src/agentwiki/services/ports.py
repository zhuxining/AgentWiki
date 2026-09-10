"""Stable application ports implemented by local adapters."""

from typing import Protocol

from agentwiki.domain.documents import (
    DocumentDescriptor,
    DocumentFingerprint,
    DocumentPath,
    SyncReport,
    WikiDocument,
)
from agentwiki.domain.retrieval import ContextQuery, IndexedChunk, SearchCandidate
from agentwiki.domain.tags import TagAliases


class SearchRepository(Protocol):
    @property
    def semantic_available(self) -> bool: ...

    async def fingerprints(self) -> dict[str, DocumentFingerprint]: ...

    async def replace_document(
        self, document: WikiDocument, chunks: tuple[IndexedChunk, ...]
    ) -> str | None: ...

    async def delete_paths(self, paths: tuple[str, ...], *, commit: bool = True) -> None: ...

    async def clear(self) -> None: ...

    async def exact_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def keyword_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def semantic_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...

    async def recent_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]: ...


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
