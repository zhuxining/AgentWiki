"""Advisory lock coverage, including the paths that must degrade instead of failing.

Exclusivity is only asserted across processes: within one process a second `flock` on a
different descriptor blocks forever, so a nested acquisition would hang the suite.
"""

import asyncio
import os
from pathlib import Path

import pytest

from agentwiki.indexing.locking import (
    IndexLock,
    hold_index_lock,
    lock_is_supported,
    remove_lock_file,
)


async def test_lock_is_acquired_and_released(tmp_path) -> None:
    index = tmp_path / "index.sqlite3"
    lock = IndexLock(index)

    assert await asyncio.to_thread(lock._acquire) is True
    assert lock._held is True
    assert index.with_name("index.sqlite3.lock").exists()

    await asyncio.to_thread(lock._release)
    assert lock._held is False


async def test_unusable_path_degrades_to_unlocked(tmp_path, monkeypatch) -> None:
    """An unwritable lock location must not break synchronization."""
    index = tmp_path / "index.sqlite3"

    def boom(*_args, **_kwargs):
        raise OSError("read-only filesystem")

    monkeypatch.setattr(os, "open", boom)
    async with hold_index_lock(index) as held:
        assert held is False


async def test_missing_lock_directory_is_created(tmp_path) -> None:
    index = tmp_path / "nested" / "deeper" / "index.sqlite3"

    async with hold_index_lock(index) as held:
        assert held is True
        assert index.with_name("index.sqlite3.lock").exists()


def test_release_without_holding_is_a_noop(tmp_path) -> None:
    lock = IndexLock(tmp_path / "index.sqlite3")

    lock._release()  # must not raise

    assert not lock._held


def test_remove_lock_file_is_idempotent(tmp_path) -> None:
    index = tmp_path / "index.sqlite3"
    lock_path = index.with_name("index.sqlite3.lock")
    lock_path.write_text("", encoding="utf-8")

    remove_lock_file(index)
    assert not lock_path.exists()

    remove_lock_file(index)  # second call must not raise


def test_locking_is_supported_on_this_platform() -> None:
    """POSIX and Windows both provide a primitive; only exotic platforms do not."""
    assert isinstance(lock_is_supported(), bool)


@pytest.mark.integration
async def test_lock_excludes_another_process(tmp_path) -> None:
    """Acquiring from a second process must block until the first releases."""
    import subprocess
    import sys
    import textwrap
    import time

    index = tmp_path / "index.sqlite3"
    script = tmp_path / "holder.py"
    script.write_text(
        textwrap.dedent(
            """
            import asyncio, sys, time
            sys.path.insert(0, "src")
            from pathlib import Path
            from agentwiki.indexing.locking import hold_index_lock

            async def main() -> None:
                async with hold_index_lock(Path(sys.argv[1])) as held:
                    print("held" if held else "unheld", flush=True)
                    time.sleep(1.5)

            asyncio.run(main())
            """
        ),
        encoding="utf-8",
    )
    repo_root = Path(__file__).resolve().parents[2]
    child = subprocess.Popen(
        [sys.executable, str(script), str(index)],
        cwd=str(repo_root),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert child.stdout is not None
        assert child.stdout.readline().strip() == "held"

        started = time.monotonic()
        async with asyncio.timeout(20):
            async with hold_index_lock(index) as held:
                assert held is True

        assert time.monotonic() - started >= 0.5, "second holder was not blocked"
    finally:
        assert child.stdout is not None
        child.stdout.close()
        _, stderr = child.communicate(timeout=10)
        assert child.returncode == 0, f"lock holder failed: {stderr}"
