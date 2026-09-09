"""Index rebuild workflow kept separate from document business services."""

from pathlib import Path
import time

from agentwiki.markdown.store import MarkdownStore
from agentwiki.repository.embeddings import EmbeddingProvider
from agentwiki.repository.sqlite_index import SQLiteIndex


async def rebuild(
    document_root: Path,
    index_path: Path,
    embedding_provider: EmbeddingProvider | None = None,
) -> int:
    """Rebuild the complete SQLite projection from Markdown files."""
    store = MarkdownStore(document_root)
    index = SQLiteIndex(index_path, embedding_provider=embedding_provider)
    await index.initialize()
    try:
        notes = store.iter_notes()
        await index.rebuild(notes, timestamp=time.time())
        return len(notes)
    finally:
        await index.close()
