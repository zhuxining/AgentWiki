"""Optional local filesystem watcher for keeping the derived index fresh."""

from pathlib import Path
from threading import Event

from watchfiles import watch

from agentwiki.services.notes import NoteService


def watch_documents(service: NoteService, *, stop_event: Event | None = None) -> None:
    """Rebuild the local index after Markdown changes.

    The first implementation deliberately uses a full rebuild for each change
    batch. It keeps correctness simple while the repository grows an incremental
    planner; the Markdown files remain the source of truth in either case.
    """
    for changes in watch(service.store.root):
        if stop_event is not None and stop_event.is_set():
            return
        if any(_is_markdown_change(path) for _, path in changes):
            service.rebuild_index()


def _is_markdown_change(path: str) -> bool:
    return Path(path).suffix.lower() == ".md"
