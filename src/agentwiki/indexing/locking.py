"""Cross-process mutual exclusion for the derived index.

The in-process ``asyncio.Lock`` used by the synchronizer only serializes coroutines
inside one process. Two long-lived MCP servers (or a CLI run next to a server) each hold
their own SQLite connection and can scan the same change set at the same time, which
lets one process delete rows another just wrote. A per-index file lock closes that
window; SQLite's own writer serialization is not enough because a synchronization round
spans many statements.
"""

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager, suppress
import os
from pathlib import Path
import sqlite3

try:  # POSIX
    import fcntl
except ImportError:  # pragma: no cover - Windows
    fcntl = None  # type: ignore[assignment]

try:  # Windows
    import msvcrt
except ImportError:  # pragma: no cover - POSIX
    msvcrt = None  # type: ignore[assignment]


def _windows_lock(descriptor: int, *, unlock: bool) -> None:
    """Lock or unlock one byte using ``msvcrt``.

    Accessed through ``getattr`` because ``msvcrt`` only exists on Windows, so the type
    checker cannot resolve its members on the platforms this project is also built for.
    """
    locking = getattr(msvcrt, "locking", None)
    if locking is None:
        raise OSError("msvcrt.locking is unavailable")
    mode = getattr(msvcrt, "LK_UNLCK" if unlock else "LK_LOCK")
    locking(descriptor, mode, 1)


class IndexLock:
    """A best-effort advisory lock keyed on one index file.

    The lock is advisory: it only excludes other AgentWiki processes that use the same
    path. If the platform cannot lock at all the operation proceeds unlocked rather than
    failing, because an unlocked sync is still better than an unusable one.
    """

    def __init__(self, index_path: Path) -> None:
        self.path = index_path.with_name(f"{index_path.name}.lock")
        self._held = False

    def _acquire(self) -> bool:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        try:
            descriptor = os.open(self.path, os.O_CREAT | os.O_RDWR, 0o600)
        except OSError:
            return False
        try:
            if fcntl is not None:
                fcntl.flock(descriptor, fcntl.LOCK_EX)
            elif msvcrt is not None:  # pragma: no cover - Windows
                _windows_lock(descriptor, unlock=False)
            else:  # pragma: no cover - no locking primitive available
                return False
        except OSError:
            os.close(descriptor)
            return False
        # Keep the descriptor open for the lifetime of the lock; closing it releases it.
        self._descriptor = descriptor
        self._held = True
        return True

    def _release(self) -> None:
        descriptor = getattr(self, "_descriptor", None)
        if descriptor is None:
            return
        try:
            if fcntl is not None:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            elif msvcrt is not None:  # pragma: no cover - Windows
                os.lseek(descriptor, 0, os.SEEK_SET)
                _windows_lock(descriptor, unlock=True)  # type: ignore[unresolved-attribute]
        except OSError:
            pass
        finally:
            os.close(descriptor)
            self._descriptor = None
            self._held = False


@asynccontextmanager
async def hold_index_lock(index_path: Path) -> AsyncIterator[bool]:
    """Hold the index lock for the duration of one synchronization round.

    Yields True when the lock was taken. Blocking acquisition is offloaded to a thread so
    a waiting process does not stall its event loop.
    """
    lock = IndexLock(index_path)
    acquired = await asyncio.to_thread(lock._acquire)
    try:
        yield acquired
    finally:
        if acquired:
            await asyncio.to_thread(lock._release)


def lock_is_supported() -> bool:
    """Return True when this platform can exclude other processes."""
    return fcntl is not None or msvcrt is not None


def remove_lock_file(index_path: Path) -> None:
    """Delete the lock file, ignoring absence and platforms that keep it open."""
    with suppress(OSError, sqlite3.Error):
        index_path.with_name(f"{index_path.name}.lock").unlink(missing_ok=True)
