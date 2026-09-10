"""Incrementally project native Markdown file changes into the search index."""

import asyncio

from agentwiki.domain.documents import SyncReport
from agentwiki.indexing.chunking import chunk_document
from agentwiki.indexing.graph import extract_graph
from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.services.ports import SearchRepository


class IndexSynchronizer:
    def __init__(self, library: MarkdownLibrary, repository: SearchRepository) -> None:
        self.library = library
        self.repository = repository
        self._lock = asyncio.Lock()

    async def ensure_fresh(self) -> SyncReport:
        async with self._lock:
            descriptors = await asyncio.to_thread(self.library.descriptors)
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
                except (OSError, UnicodeError, ValueError) as exc:
                    reason = f"failed to index {descriptor.path.value}: {exc}"
                    await self.repository.mark_document_error(descriptor.path.value, reason)
                    degraded.append(reason)
            if remaining_removed:
                await self.repository.delete_paths(tuple(sorted(remaining_removed)))
            await self.repository.resolve_edges()
            return SyncReport(
                indexed=completed,
                removed=len(remaining_removed),
                moved=moved,
                unchanged=len(current) - len(changed),
                vectors_ready=vectors_ready,
                vectors_pending=vectors_pending,
                degraded=tuple(degraded),
            )

    async def rebuild(self) -> SyncReport:
        async with self._lock:
            await self.repository.clear()
            descriptors = await asyncio.to_thread(self.library.descriptors)
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
                except (OSError, UnicodeError, ValueError) as exc:
                    reason = f"failed to index {descriptor.path.value}: {exc}"
                    await self.repository.mark_document_error(descriptor.path.value, reason)
                    degraded.append(reason)
            await self.repository.resolve_edges()
            return SyncReport(
                indexed=completed,
                vectors_ready=vectors_ready,
                vectors_pending=vectors_pending,
                degraded=tuple(degraded),
            )
