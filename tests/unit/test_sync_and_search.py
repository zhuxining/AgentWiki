import asyncio
from collections.abc import Sequence
import os
from threading import Event

from agentwiki.domain.retrieval import ContextQuery
from agentwiki.runtime.context import create_runtime


class FakeEmbeddingProvider:
    model_name = "fake"

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        return [self._vector(text) for text in texts]

    def embed_query(self, text: str) -> list[float]:
        return self._vector(text)

    @staticmethod
    def _vector(text: str) -> list[float]:
        lowered = text.casefold()
        return [float(lowered.count("python")), float(lowered.count("sqlite"))]


class CountingEmbeddingProvider(FakeEmbeddingProvider):
    def __init__(self, model_name: str) -> None:
        self.model_name = model_name
        self.batch_sizes: list[int] = []

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        self.batch_sizes.append(len(texts))
        return super().embed_documents(texts)


class InconsistentEmbeddingProvider(FakeEmbeddingProvider):
    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        return [[1.0] for _ in texts[:-1]] + [[1.0, 2.0]]


class BlockingEmbeddingProvider(FakeEmbeddingProvider):
    def __init__(self) -> None:
        self.started = Event()
        self.release = Event()

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        self.started.set()
        self.release.wait(timeout=5)
        return super().embed_documents(texts)


async def test_query_preflight_indexes_native_create_update_and_delete(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    path = root / "guide.md"
    path.write_text("SQLite evidence\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        first = await runtime.retrieval.get_wiki_context(ContextQuery(query="SQLite"))
        assert first.results[0].path == "guide.md"

        path.write_text("Python evidence\n", encoding="utf-8")
        stat = path.stat()
        os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))
        second = await runtime.retrieval.get_wiki_context(ContextQuery(query="Python"))
        assert second.results[0].path == "guide.md"
        assert not (await runtime.retrieval.get_wiki_context(ContextQuery(query="SQLite"))).results

        path.unlink()
        assert not (await runtime.retrieval.get_wiki_context(ContextQuery(query="Python"))).results
    finally:
        await runtime.close()


async def test_hybrid_search_returns_semantic_section_snippet(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "languages.md").write_text(
        "# Languages\n\n## Python\n\nLocal Python tooling\n\n## Other\n\nUnrelated",
        encoding="utf-8",
    )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3", FakeEmbeddingProvider())
    try:
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="python"))
        assert result.strategy == "hybrid"
        assert result.results[0].section == "Languages / Python"
        assert result.results[0].snippet == "Local Python tooling"
    finally:
        await runtime.close()


async def test_recent_uses_file_mtime_not_rebuild_time(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    old = root / "old.md"
    new = root / "new.md"
    old.write_text("old\n", encoding="utf-8")
    new.write_text("new\n", encoding="utf-8")
    os.utime(old, ns=(1_000_000_000, 1_000_000_000))
    os.utime(new, ns=(2_000_000_000, 2_000_000_000))
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        result = await runtime.retrieval.get_wiki_context(ContextQuery())
        assert [item.path for item in result.results] == ["new.md", "old.md"]
    finally:
        await runtime.close()


async def test_scope_and_metadata_filters_apply_before_candidate_limit(tmp_path) -> None:
    root = tmp_path / "documents"
    (root / "guides").mkdir(parents=True)
    (root / "guides" / "target.md").write_text(
        "---\ntype: guide\ntags: [local]\n---\n\nTarget evidence\n",
        encoding="utf-8",
    )
    for index in range(25):
        (root / f"new-{index}.md").write_text("Other evidence\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        result = await runtime.retrieval.get_wiki_context(
            ContextQuery(scope="guides", tags=("local",), note_types=("guide",))
        )
        assert [item.path for item in result.results] == ["guides/target.md"]
    finally:
        await runtime.close()


async def test_tag_filter_uses_aliases_and_includes_hierarchy_descendants(tmp_path) -> None:
    root = tmp_path / "documents"
    reserved = root / "agentwiki"
    reserved.mkdir(parents=True)
    (reserved / "context.yaml").write_text(
        "tag_aliases:\n  engineering:\n    - eng\n", encoding="utf-8"
    )
    (root / "child.md").write_text(
        "---\ntitle: Child\ntype: note\ntags: [engineering/backend]\n"
        "created_at: 2026-09-10\nupdated_at: 2026-09-10\n---\n\nTagged\n",
        encoding="utf-8",
    )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        result = await runtime.retrieval.get_wiki_context(ContextQuery(tags=("eng",)))
        assert [item.path for item in result.results] == ["child.md"]
    finally:
        await runtime.close()


async def test_parse_failure_is_reported_without_hiding_valid_results(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "valid.md").write_text("searchable evidence\n", encoding="utf-8")
    (root / "broken.md").write_text("---\nkey: [\n---\n\nbroken\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="searchable"))
        assert result.results[0].path == "valid.md"
        assert any("broken.md" in reason for reason in result.degraded)
    finally:
        await runtime.close()


async def test_graph_links_and_frontmatter_relations_are_attached_to_evidence(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "source.md").write_text(
        "---\nrelations:\n  - type: depends_on\n    target: target.md\n---\n\n"
        "# Source\n\nSee [[target]] for details.\n",
        encoding="utf-8",
    )
    (root / "target.md").write_text("# Target\n\nTarget evidence\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        source = await runtime.retrieval.get_wiki_context(ContextQuery(query="Source"))
        assert source.results[0].path == "source.md"
        assert {
            (item.path, item.relation_type, item.direction) for item in source.results[0].related
        } == {
            ("target.md", "links_to", "outgoing"),
            ("target.md", "depends_on", "outgoing"),
        }

        target = await runtime.retrieval.get_wiki_context(ContextQuery(query="Target"))
        assert target.results[0].path == "target.md"
        assert target.results[0].related[0].path == "source.md"
        assert target.results[0].related[0].direction == "incoming"
        assert target.results[0].related[0].source_section == "Source"
        assert target.results[0].related[0].context == "See [[target]] for details."
    finally:
        await runtime.close()


async def test_unique_content_hash_move_preserves_document_identity(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    old = root / "old.md"
    new = root / "new.md"
    old.write_text("stable document\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.ensure_fresh()
        before = await runtime.repository.fingerprints()
        old_id = before["old.md"].document_id
        old.rename(new)

        report = await runtime.synchronizer.ensure_fresh()
        after = await runtime.repository.fingerprints()
        assert report.moved == 1
        assert report.removed == 0
        assert after["new.md"].document_id == old_id
        assert "old.md" not in after
    finally:
        await runtime.close()


async def test_relation_metadata_can_recall_source_document(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "source.md").write_text(
        "---\nrelations:\n  - type: depends_on\n    target: target.md\n---\n\n"
        "# Architecture\n\nThe source body has no relation keywords.\n",
        encoding="utf-8",
    )
    (root / "target.md").write_text("# Retrieval\n\nTarget document\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        result = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="depends_on")
        )
        assert result.results[0].path == "source.md"
        assert "graph" in result.results[0].match_sources
    finally:
        await runtime.close()


async def test_unresolved_relation_keeps_target_path_and_status(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "source.md").write_text(
        "---\nrelations:\n  - type: depends_on\n    target: missing.md\n---\n\n"
        "# Source\n\nSource evidence\n",
        encoding="utf-8",
    )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="Source"))
        assert result.results[0].related[0].model_dump() == {
            "path": "missing.md",
            "title": "missing.md",
            "relation_type": "depends_on",
            "direction": "outgoing",
            "resolution_status": "unresolved",
            "source_section": "Frontmatter",
            "context": "depends_on: missing.md",
        }
    finally:
        await runtime.close()


async def test_sync_errors_are_persisted_and_cleared_after_recovery(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    path = root / "broken.md"
    path.write_text("---\nkey: [\n---\n\nbroken\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        report = await runtime.synchronizer.ensure_fresh()
        assert report.degraded
        cursor = await runtime.database.connection.execute(
            "SELECT sync_state, sync_error FROM wiki_documents WHERE path = 'broken.md'"
        )
        row = await cursor.fetchone()
        assert row is not None
        state, error = row
        assert state == "error"
        assert "broken.md" in error

        path.write_text("# Recovered\n\nvalid\n", encoding="utf-8")
        await runtime.synchronizer.ensure_fresh()
        cursor = await runtime.database.connection.execute(
            "SELECT sync_state, sync_error FROM wiki_documents WHERE path = 'broken.md'"
        )
        row = await cursor.fetchone()
        assert row is not None
        state, error = row
        assert state == "ready"
        assert error is None
    finally:
        await runtime.close()


async def test_embedding_model_change_rebuilds_vectors(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "guide.md").write_text("# Guide\n\nSQLite evidence\n", encoding="utf-8")
    index = tmp_path / "index.sqlite3"
    first_provider = CountingEmbeddingProvider("model-a")
    first_runtime = await create_runtime(root, index, first_provider)
    try:
        await first_runtime.retrieval.get_wiki_context(ContextQuery(query="SQLite"))
    finally:
        await first_runtime.close()

    second_provider = CountingEmbeddingProvider("model-b")
    second_runtime = await create_runtime(root, index, second_provider)
    try:
        result = await second_runtime.retrieval.get_wiki_context(ContextQuery(query="SQLite"))
        cursor = await second_runtime.database.connection.execute(
            "SELECT DISTINCT model, status FROM wiki_vector_manifest"
        )
        rows = await cursor.fetchall()
        assert {(str(row["model"]), str(row["status"])) for row in rows} == {
            ("model-b", "ready")
        }
        assert result.strategy == "hybrid"
        assert second_provider.batch_sizes == [1]
    finally:
        await second_runtime.close()


async def test_changed_document_reuses_unchanged_chunk_vectors(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    path = root / "guide.md"
    path.write_text("# Guide\n\nFirst stable chunk\n\n## Second\n\nOld text\n", encoding="utf-8")
    provider = CountingEmbeddingProvider("model")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3", provider)
    try:
        await runtime.retrieval.get_wiki_context(ContextQuery(query="stable"))
        path.write_text(
            "# Guide\n\nFirst stable chunk\n\n## Second\n\nNew text\n", encoding="utf-8"
        )
        await runtime.synchronizer.ensure_fresh()
        await runtime.repository.wait_for_vector_sync()
        assert provider.batch_sizes == [2, 1]
    finally:
        await runtime.close()


async def test_invalid_vector_dimensions_are_persisted_and_retried(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "guide.md").write_text(
        "# Guide\n\nFirst chunk\n\n## Second\n\nSecond chunk\n", encoding="utf-8"
    )
    provider = InconsistentEmbeddingProvider()
    runtime = await create_runtime(root, tmp_path / "index.sqlite3", provider)
    try:
        await runtime.retrieval.get_wiki_context(ContextQuery(query="chunk"))
        cursor = await runtime.database.connection.execute(
            "SELECT status, error FROM wiki_vector_manifest"
        )
        rows = await cursor.fetchall()
        assert {str(row["status"]) for row in rows} == {"error"}
        assert all("inconsistent vector dimensions" in str(row["error"]) for row in rows)
        assert await runtime.repository.vector_stale_paths() == {"guide.md"}
    finally:
        await runtime.close()


async def test_interrupted_pending_vectors_are_retried_by_next_runtime(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "guide.md").write_text("# Guide\n\nSQLite evidence\n", encoding="utf-8")
    index = tmp_path / "index.sqlite3"
    blocking_provider = BlockingEmbeddingProvider()
    first_runtime = await create_runtime(root, index, blocking_provider)
    try:
        await first_runtime.synchronizer.ensure_fresh()
        await asyncio.to_thread(blocking_provider.started.wait, 2)
    finally:
        await first_runtime.close()
        blocking_provider.release.set()

    second_runtime = await create_runtime(root, index, FakeEmbeddingProvider())
    try:
        result = await second_runtime.retrieval.get_wiki_context(
            ContextQuery(query="SQLite")
        )
        assert result.strategy == "hybrid"
        cursor = await second_runtime.database.connection.execute(
            "SELECT DISTINCT status FROM wiki_vector_manifest"
        )
        assert {str(row[0]) for row in await cursor.fetchall()} == {"ready"}
    finally:
        await second_runtime.close()
