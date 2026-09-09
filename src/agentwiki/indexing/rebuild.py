"""Index rebuild workflow kept separate from document business services."""

from pathlib import Path

from agentwiki.markdown.store import MarkdownStore
from agentwiki.repository.sqlite_index import SQLiteIndex


def rebuild(document_root: Path, index_path: Path) -> int:
    """Rebuild the complete SQLite projection from Markdown files."""
    store = MarkdownStore(document_root)
    index = SQLiteIndex(index_path)
    try:
        notes = store.iter_notes()
        import time

        index.rebuild(notes, timestamp=time.time())
        return len(notes)
    finally:
        index.close()
