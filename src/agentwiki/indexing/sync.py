"""Incrementally project native Markdown file changes into the search index."""

import asyncio

from agentwiki.domain.documents import DocumentFingerprint, SyncReport
from agentwiki.indexing.chunking import chunk_document
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
            if removed:
                await self.repository.delete_paths(removed)

            changed = [
                descriptor
                for path, descriptor in current.items()
                if indexed.get(path)
                != DocumentFingerprint(descriptor.modified_at_ns, descriptor.size)
            ]
            degraded: list[str] = []
            completed = 0
            for descriptor in changed:
                try:
                    document = await asyncio.to_thread(self.library.read, descriptor)
                    vector_warning = await self.repository.replace_document(
                        document, chunk_document(document)
                    )
                    if vector_warning:
                        degraded.append(vector_warning)
                    completed += 1
                except (OSError, UnicodeError, ValueError) as exc:
                    await self.repository.delete_paths((descriptor.path.value,))
                    degraded.append(f"failed to index {descriptor.path.value}: {exc}")
            return SyncReport(
                indexed=completed,
                removed=len(removed),
                unchanged=len(current) - len(changed),
                degraded=tuple(degraded),
            )

    async def rebuild(self) -> SyncReport:
        async with self._lock:
            await self.repository.clear()
            descriptors = await asyncio.to_thread(self.library.descriptors)
            degraded: list[str] = []
            completed = 0
            for descriptor in descriptors:
                try:
                    document = await asyncio.to_thread(self.library.read, descriptor)
                    vector_warning = await self.repository.replace_document(
                        document, chunk_document(document)
                    )
                    if vector_warning:
                        degraded.append(vector_warning)
                    completed += 1
                except (OSError, UnicodeError, ValueError) as exc:
                    degraded.append(f"failed to index {descriptor.path.value}: {exc}")
            return SyncReport(indexed=completed, degraded=tuple(degraded))
