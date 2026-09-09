"""Use cases for Markdown documents and their local index."""

from __future__ import annotations

from pathlib import Path
import time

from agentwiki.domain.models import Note, NotePath, SearchMode, SearchQuery, SearchResult
from agentwiki.markdown.store import MarkdownStore
from agentwiki.repository.embeddings import EmbeddingProvider
from agentwiki.repository.sqlite_index import SQLiteIndex


class NoteService:
    """Coordinate document mutations and the rebuildable SQLite projection."""

    def __init__(self, store: MarkdownStore, index: SQLiteIndex) -> None:
        self.store = store
        self.index = index

    def write(self, path: str, content: str, frontmatter: dict[str, object] | None = None) -> Note:
        note = Note(path=NotePath(value=path), content=content, frontmatter=dict(frontmatter or {}))
        self.store.write(note)
        self.index.upsert(note, updated_at=time.time())
        return note

    def read(self, path: str) -> Note:
        return self.store.read(NotePath(value=path))

    def update(
        self,
        path: str,
        *,
        content: str | None = None,
        frontmatter: dict[str, object] | None = None,
    ) -> Note:
        current = self.read(path)
        note = current.model_copy(
            update={
                "content": current.content if content is None else content,
                "frontmatter": current.frontmatter if frontmatter is None else dict(frontmatter),
            }
        )
        self.store.write(note, overwrite=True)
        self.index.upsert(note, updated_at=time.time())
        return note

    def delete(self, path: str) -> None:
        note_path = NotePath(value=path)
        self.store.delete(note_path)
        self.index.delete(note_path)

    def move(self, source: str, target: str) -> None:
        source_path = NotePath(value=source)
        target_path = NotePath(value=target)
        self.store.move(source_path, target_path)
        self.index.move(source_path, target_path)

    def search(
        self,
        text: str,
        *,
        mode: SearchMode = "keyword",
        limit: int = 20,
    ) -> list[SearchResult]:
        return self.index.search(SearchQuery(text=text, mode=mode, limit=limit))

    def rebuild_index(self) -> int:
        notes = self.store.iter_notes()
        self.index.rebuild(notes, timestamp=time.time())
        return len(notes)

    def sync_index(self) -> int:
        """Reconcile the complete index with the current Markdown document set."""
        return self.rebuild_index()


def create_service(
    document_root: Path,
    index_path: Path,
    embedding_provider: EmbeddingProvider | None = None,
) -> NoteService:
    """Create a local service and its explicitly owned resources."""
    return NoteService(
        MarkdownStore(document_root),
        SQLiteIndex(index_path, embedding_provider=embedding_provider),
    )
