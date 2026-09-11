"""Optional filesystem watcher using the shared incremental synchronizer."""

from pathlib import Path
from threading import Event

from watchfiles import awatch

from agentwiki.services.ports import FreshnessSynchronizer


async def watch_documents(
    root: Path,
    synchronizer: FreshnessSynchronizer,
    *,
    stop_event: Event | None = None,
) -> None:
    async for changes in awatch(root, debounce=300):
        if stop_event is not None and stop_event.is_set():
            return
        if any(Path(path).suffix.lower() == ".md" for _, path in changes):
            await synchronizer.ensure_fresh()
