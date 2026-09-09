"""Use cases for Markdown documents and their local index."""

from pathlib import Path
import re
import time

from agentwiki.domain.models import (
    Frontmatter,
    Note,
    NotePath,
    SearchMode,
    SearchQuery,
    SearchResult,
)
from agentwiki.markdown.editing import edit_content
from agentwiki.markdown.store import MarkdownStore
from agentwiki.repository.embeddings import EmbeddingProvider
from agentwiki.repository.sqlite_index import SQLiteIndex


class NoteService:
    """Coordinate document mutations and the rebuildable SQLite projection."""

    def __init__(self, store: MarkdownStore, index: SQLiteIndex) -> None:
        self.store = store
        self.index = index

    def write(
        self,
        path: str | None,
        content: str,
        frontmatter: Frontmatter | None = None,
        *,
        title: str | None = None,
        directory: str = "",
        tags: list[str] | None = None,
        note_type: str = "note",
        overwrite: bool = False,
    ) -> Note:
        metadata = dict(frontmatter or {})
        if title is not None:
            metadata.setdefault("title", title)
        if tags is not None:
            metadata["tags"] = tags
        metadata.setdefault("type", note_type)
        resolved_path = path or self._path_from_title(title, directory)
        note = Note(path=NotePath(value=resolved_path), content=content, frontmatter=metadata)
        self.store.write(note, overwrite=overwrite)
        self.index.upsert(note, updated_at=time.time())
        return note

    def read(self, identifier: str) -> Note:
        return self.store.read(self.resolve(identifier))

    def resolve(self, identifier: str) -> NotePath:
        """Resolve a relative path, memory URL, or unique title."""
        candidate = identifier.removeprefix("memory://").strip("/")
        if not candidate.endswith(".md"):
            candidate += ".md"
        try:
            path = NotePath(value=candidate)
            if self.store.exists(path):
                return path
        except ValueError:
            pass
        matches = [
            note.path
            for note in self.store.iter_notes()
            if note.title.casefold() == identifier.casefold()
        ]
        if len(matches) == 1:
            return matches[0]
        if not matches:
            raise FileNotFoundError(identifier)
        raise ValueError(f"identifier is ambiguous: {identifier}")

    def update(
        self,
        path: str,
        *,
        content: str | None = None,
        frontmatter: Frontmatter | None = None,
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

    def edit(
        self,
        identifier: str,
        *,
        operation: str,
        content: str,
        find_text: str | None = None,
        section: str | None = None,
        expected_replacements: int = 1,
        replace_subsections: bool = True,
        metadata: Frontmatter | None = None,
    ) -> Note:
        current = self.read(identifier)
        updated = current.model_copy(
            update={
                "content": edit_content(
                    current.content,
                    operation=operation,
                    value=content,
                    find_text=find_text,
                    section=section,
                    expected_replacements=expected_replacements,
                    replace_subsections=replace_subsections,
                ),
                "frontmatter": (
                    current.frontmatter
                    if metadata is None
                    else {**current.frontmatter, **metadata}
                ),
            }
        )
        self.store.write(updated, overwrite=True)
        self.index.upsert(updated, updated_at=time.time())
        return updated

    def delete(self, path: str, *, is_directory: bool = False) -> None:
        if is_directory:
            deleted = self.store.delete_directory(path)
            for note_path in deleted:
                self.index.delete(note_path)
            return
        note_path = self.resolve(path)
        self.store.delete(note_path)
        self.index.delete(note_path)

    def move(self, source: str, target: str, *, destination_folder: bool = False) -> str:
        source_path = self.resolve(source)
        target_path = (
            NotePath(value=f"{target.rstrip('/')}/{source_path.value.rsplit('/', 1)[-1]}")
            if destination_folder
            else NotePath(value=target)
        )
        self.store.move(source_path, target_path)
        self.index.move(source_path, target_path)
        return target_path.value

    def search(
        self,
        text: str,
        *,
        mode: SearchMode = "keyword",
        limit: int = 20,
        page: int = 1,
        tags: list[str] | None = None,
        note_types: list[str] | None = None,
        metadata_filters: Frontmatter | None = None,
    ) -> list[SearchResult]:
        return self.index.search(
            SearchQuery(
                text=text,
                mode=mode,
                limit=limit,
                page=page,
                tags=tags or [],
                note_types=note_types or [],
                metadata_filters=metadata_filters or {},
            )
        )

    def rebuild_index(self) -> int:
        notes = self.store.iter_notes()
        self.index.rebuild(notes, timestamp=time.time())
        return len(notes)

    def sync_index(self) -> int:
        """Reconcile the complete index with the current Markdown document set."""
        return self.rebuild_index()

    @staticmethod
    def _path_from_title(title: str | None, directory: str) -> str:
        if not title or not title.strip():
            raise ValueError("title is required when path is omitted")
        slug = re.sub(r"[^\w.-]+", "-", title.strip(), flags=re.UNICODE).strip("-.")
        if not slug:
            raise ValueError("title does not produce a valid Markdown filename")
        return f"{directory.strip('/').strip() + '/' if directory.strip('/') else ''}{slug}.md"


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
