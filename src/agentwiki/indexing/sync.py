"""Incrementally project native Markdown file changes into the search index."""

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path
import sqlite3

from agentwiki.domain.documents import SyncReport
from agentwiki.indexing.chunking import chunk_document
from agentwiki.indexing.graph import extract_graph
from agentwiki.indexing.locking import hold_index_lock
from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.services.ports import SearchRepository

# A single document must never abort a whole synchronization round: a YAML/Markdown
# problem, an unreadable file, or a SQLite constraint error all degrade to a recorded
# failure and the remaining documents continue.
_DOCUMENT_ERRORS = (OSError, UnicodeError, ValueError, sqlite3.Error)


class IndexSynchronizer:
    def __init__(
        self,
        library: MarkdownLibrary,
        repository: SearchRepository,
        index_path: Path | None = None,
    ) -> None:
        self.library = library
        self.repository = repository
        self.index_path = index_path
        self._lock = asyncio.Lock()

    async def ensure_fresh(self) -> SyncReport:
        async with self._lock, hold_index_lock(self.index_path) if self.index_path else _no_lock():
            descriptors, generation = await asyncio.to_thread(self.library.snapshot)
            indexed_once = await self.repository.indexed_once()
            # Fast path: the document set is byte-for-byte identical to what was last
            # written, so there is nothing to reconcile and no need to read the whole
            # fingerprint table (or the chunk-level vector staleness query).
            if indexed_once and generation == await self.repository.index_generation():
                return SyncReport(
                    unchanged=len(descriptors),
                    generation=generation,
                    indexed_once=True,
                )
            indexed = await self.repository.fingerprints()
            current = {descriptor.path.value: descriptor for descriptor in descriptors}
            removed = tuple(sorted(indexed.keys() - current.keys()))
            remaining_removed = set(removed)
            stale_vectors = await self.repository.vector_stale_paths()

            changed = [
                descriptor
                for path, descriptor in current.items()
                if (
                    path not in indexed
                    or indexed[path].content_hash == ""
                    or indexed[path].modified_at_ns != descriptor.modified_at_ns
                    or indexed[path].size != descriptor.size
                    or path in stale_vectors
                )
            ]
            degraded: list[str] = []
            completed = 0
            moved = 0
            vectors_ready = 0
            vectors_pending = 0
            for descriptor in changed:
                try:
                    document = await asyncio.to_thread(self.library.read, descriptor)
                    graph = extract_graph(document)
                    degraded.extend(
                        f"{descriptor.path.value}: {warning}" for warning in graph.warnings
                    )
                    move_sources = tuple(
                        path
                        for path in remaining_removed
                        if indexed[path].content_hash
                        and indexed[path].content_hash == document.content_hash
                    )
                    moved_from = move_sources[0] if len(move_sources) == 1 else None
                    if len(move_sources) == 1:
                        remaining_removed.remove(move_sources[0])
                        moved += 1
                    vector_warning = await self.repository.replace_document(
                        document,
                        chunk_document(document),
                        graph.edges,
                        moved_from=moved_from,
                    )
                    if vector_warning:
                        degraded.append(vector_warning)
                    vector_state = await self.repository.vector_state(descriptor.path.value)
                    if vector_state == "pending":
                        vectors_pending += 1
                    elif vector_state == "ready":
                        vectors_ready += 1
                    completed += 1
                except _DOCUMENT_ERRORS as exc:
                    reason = f"failed to index {descriptor.path.value}: {exc}"
                    degraded.append(reason)
                    await self._record_failure(descriptor.path.value, reason, degraded)
            if remaining_removed:
                await self.repository.delete_paths(tuple(sorted(remaining_removed)))
            await self._resolve_edges(degraded)
            await self.repository.mark_index_complete(generation)
            self._finish_schema_rebuild()
            return SyncReport(
                indexed=completed,
                removed=len(remaining_removed),
                moved=moved,
                unchanged=len(current) - len(changed),
                vectors_ready=vectors_ready,
                vectors_pending=vectors_pending,
                degraded=tuple(degraded),
                generation=generation,
                indexed_once=True,
            )

    async def rebuild(self) -> SyncReport:
        async with self._lock, hold_index_lock(self.index_path) if self.index_path else _no_lock():
            await self.repository.clear()
            descriptors, generation = await asyncio.to_thread(self.library.snapshot)
            degraded: list[str] = []
            completed = 0
            vectors_ready = 0
            vectors_pending = 0
            for descriptor in descriptors:
                try:
                    document = await asyncio.to_thread(self.library.read, descriptor)
                    graph = extract_graph(document)
                    degraded.extend(
                        f"{descriptor.path.value}: {warning}" for warning in graph.warnings
                    )
                    vector_warning = await self.repository.replace_document(
                        document,
                        chunk_document(document),
                        graph.edges,
                    )
                    if vector_warning:
                        degraded.append(vector_warning)
                    vector_state = await self.repository.vector_state(descriptor.path.value)
                    if vector_state == "pending":
                        vectors_pending += 1
                    elif vector_state == "ready":
                        vectors_ready += 1
                    completed += 1
                except _DOCUMENT_ERRORS as exc:
                    reason = f"failed to index {descriptor.path.value}: {exc}"
                    degraded.append(reason)
                    await self._record_failure(descriptor.path.value, reason, degraded)
            await self._resolve_edges(degraded)
            await self.repository.mark_index_complete(generation)
            self._finish_schema_rebuild()
            return SyncReport(
                indexed=completed,
                vectors_ready=vectors_ready,
                vectors_pending=vectors_pending,
                degraded=tuple(degraded),
                generation=generation,
                indexed_once=True,
            )

    def _finish_schema_rebuild(self) -> None:
        """Clear the "schema rebuild in progress" marker once the projection is filled."""
        finish = getattr(self.repository, "finish_rebuild", None)
        if finish is not None:
            finish()

    async def _record_failure(self, path: str, reason: str, degraded: list[str]) -> None:
        """Persist a per-document failure without letting it mask other documents."""
        try:
            await self.repository.mark_document_error(path, reason)
        except _DOCUMENT_ERRORS as exc:
            degraded.append(f"{path}: unable to record sync failure: {exc}")

    async def _resolve_edges(self, degraded: list[str]) -> None:
        """Resolve graph targets; a failure here must not discard the sync report."""
        try:
            await self.repository.resolve_edges()
        except _DOCUMENT_ERRORS as exc:
            degraded.append(f"unable to resolve document relations: {exc}")


@asynccontextmanager
async def _no_lock() -> AsyncIterator[bool]:
    """Stand-in for :func:`hold_index_lock` when no index path was provided."""
    yield False
