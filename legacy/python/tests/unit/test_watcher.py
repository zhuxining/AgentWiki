"""Watcher coverage.

`watch_documents` is a thin adapter, so most behaviour is tested by injecting a fake
event stream: that pins the ".md only" filter, the sync trigger, and the stop-event
short circuit without depending on filesystem event ordering. One integration test then
proves the real watcher fires at all.
"""

import asyncio
from threading import Event

import pytest

from agentwiki.domain.documents import SyncReport
from agentwiki.runtime.context import create_runtime
from agentwiki.runtime.watcher import watch_documents


class _FakeSynchronizer:
    def __init__(self) -> None:
        self.calls = 0

    async def ensure_fresh(self) -> SyncReport:
        self.calls += 1
        return SyncReport()

    async def rebuild(self) -> SyncReport:
        return SyncReport()


def _scripted_watch(batches: list[set[tuple[object, str]]], *, then_hang: bool = True):
    """Return an `awatch` replacement yielding the given change batches."""

    async def _awatch(_root, **_kwargs):
        for batch in batches:
            yield batch
        if then_hang:
            await asyncio.sleep(3600)

    return _awatch


async def test_markdown_change_triggers_a_sync(monkeypatch, tmp_path) -> None:
    synchronizer = _FakeSynchronizer()
    monkeypatch.setattr(
        "agentwiki.runtime.watcher.awatch",
        _scripted_watch([{(1, str(tmp_path / "a.md"))}]),
    )

    task = asyncio.create_task(watch_documents(tmp_path, synchronizer))
    await asyncio.sleep(0.15)
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)

    assert synchronizer.calls == 1


async def test_non_markdown_changes_are_ignored(monkeypatch, tmp_path) -> None:
    synchronizer = _FakeSynchronizer()
    monkeypatch.setattr(
        "agentwiki.runtime.watcher.awatch",
        _scripted_watch([{(1, str(tmp_path / "notes.txt"))}, {(2, str(tmp_path / "b.MD"))}]),
    )

    task = asyncio.create_task(watch_documents(tmp_path, synchronizer))
    await asyncio.sleep(0.15)
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)

    # .txt ignored, .MD (case-insensitive) accepted
    assert synchronizer.calls == 1


async def test_every_batch_triggers_its_own_sync(monkeypatch, tmp_path) -> None:
    synchronizer = _FakeSynchronizer()
    monkeypatch.setattr(
        "agentwiki.runtime.watcher.awatch",
        _scripted_watch([
            {(1, str(tmp_path / "a.md"))},
            {(2, str(tmp_path / "b.md"))},
            {(3, str(tmp_path / "c.md"))},
        ]),
    )

    task = asyncio.create_task(watch_documents(tmp_path, synchronizer))
    await asyncio.sleep(0.15)
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)

    assert synchronizer.calls == 3


async def test_stop_event_ends_the_loop(monkeypatch, tmp_path) -> None:
    synchronizer = _FakeSynchronizer()
    stop = Event()
    stop.set()
    monkeypatch.setattr(
        "agentwiki.runtime.watcher.awatch",
        _scripted_watch([{(1, str(tmp_path / "a.md"))}]),
    )

    await asyncio.wait_for(watch_documents(tmp_path, synchronizer, stop_event=stop), timeout=5)

    assert synchronizer.calls == 0


@pytest.mark.integration
async def test_real_watcher_syncs_after_a_file_appears(tmp_path) -> None:
    """End-to-end: the real watchfiles backend must observe a Markdown write."""
    root = tmp_path / "wiki"
    root.mkdir()
    runtime = await create_runtime(root, tmp_path / "index.sqlite3", None)
    task = asyncio.create_task(watch_documents(root, runtime.synchronizer))
    try:
        await asyncio.sleep(0.5)  # let the backend install its watch
        (root / "guided.md").write_text("# New\n\nbody\n", encoding="utf-8")
        deadline = asyncio.get_running_loop().time() + 15
        while asyncio.get_running_loop().time() < deadline:
            if await runtime.repository.fingerprints():
                break
            await asyncio.sleep(0.25)
        assert await runtime.repository.fingerprints(), "watcher never indexed the new file"
    finally:
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        await runtime.close()
