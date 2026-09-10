import asyncio
from collections import Counter
from collections.abc import Sequence
import os
from threading import Event

from agentwiki.domain.documents import DocumentFingerprint
from agentwiki.domain.retrieval import ContextQuery
from agentwiki.domain.text import query_expression
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
        "---\ntype: guide\ntags: [local]\nowner: alice\n---\n\nTarget evidence\n",
        encoding="utf-8",
    )
    for index in range(25):
        (root / f"new-{index}.md").write_text("Other evidence\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        result = await runtime.retrieval.get_wiki_context(
            ContextQuery(
                scope="guides",
                tags=("local",),
                note_types=("guide",),
                metadata_filters={"owner": "alice"},
            )
        )
        assert [item.path for item in result.results] == ["guides/target.md"]
    finally:
        await runtime.close()


async def test_candidate_pool_is_diverse_across_documents(tmp_path) -> None:
    """One long document must not consume the whole shared candidate pool."""
    root = tmp_path / "documents"
    root.mkdir()
    sections = "\n\n".join(f"## S{index}\n\nzzzq marker section {index}" for index in range(25))
    (root / "long.md").write_text(f"# Long\n\n{sections}\n", encoding="utf-8")
    for index in range(8):
        (root / f"short{index}.md").write_text(
            f"# Short{index}\n\nzzzq marker in short {index}\n", encoding="utf-8"
        )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        candidates = await runtime.repository.keyword_candidates(
            ContextQuery(query="zzzq"), candidate_limit=20
        )
        per_document = Counter(candidate.path.value for candidate in candidates)

        # Every matching document must reach the fusion stage, not just the longest one.
        assert len(per_document) >= 5, per_document
        assert max(per_document.values()) <= 4, per_document

        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="zzzq", limit=5))
        assert len({item.path for item in result.results}) >= 5
    finally:
        await runtime.close()


async def test_candidate_queries_are_bounded_in_sql(tmp_path) -> None:
    """Candidates must be limited and filtered in SQL, not after full materialization."""
    root = tmp_path / "documents"
    root.mkdir()
    for index in range(120):
        (root / f"doc{index}.md").write_text(
            f"---\ntype: note\ntags: [bulk]\n---\n\nshared evidence {index}\n",
            encoding="utf-8",
        )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        rows = await runtime.repository._rows(
            *runtime.repository._query_candidates(
                ContextQuery(query="shared"),
                rows_from=(
                    "wiki_chunks_fts "
                    "JOIN wiki_chunks AS c ON c.chunk_id = wiki_chunks_fts.chunk_id "
                    "JOIN wiki_documents AS d ON d.path = c.path"
                ),
                select=(
                    "c.chunk_id AS chunk_id, c.path AS path, d.title AS title, "
                    "c.section AS section, c.content AS content, "
                    "d.frontmatter_json AS frontmatter_json, "
                    "d.modified_at_ns AS modified_at_ns, bm25(wiki_chunks_fts) AS selected_rank, "
                    "c.ordinal AS ordinal"
                ),
                extra_conditions=("wiki_chunks_fts MATCH ?",),
                extra_parameters=(query_expression("shared"),),
                candidate_limit=20,
            )
        )
        assert len(rows) == 20
    finally:
        await runtime.close()


async def test_metadata_operators_filter_candidates(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "one.md").write_text(
        "---\ntitle: One\ntype: note\ntags: [x]\nconfidence: 5\n"
        "review:\n  status: approved\n---\n\nevidence one\n",
        encoding="utf-8",
    )
    (root / "two.md").write_text(
        "---\ntitle: Two\ntype: note\ntags: [x]\nconfidence: 9\n"
        "review:\n  status: pending\n---\n\nevidence two\n",
        encoding="utf-8",
    )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        low = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"confidence": {"$lt": 7}})
        )
        between = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"confidence": {"$between": [8, 10]}})
        )
        nested = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"review.status": "approved"})
        )
        boolean_bound = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"confidence": {"$gt": True}})
        )
        open_range = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"confidence": {"$gt": 0}})
        )

        assert [item.path for item in low.results] == ["one.md"]
        assert [item.path for item in between.results] == ["two.md"]
        assert [item.path for item in nested.results] == ["one.md"]
        assert {item.path for item in open_range.results} == {"one.md", "two.md"}
        # bool is deliberately not a number and not text, so a numeric range never
        # matches a boolean bound.
        assert not boolean_bound.results
    finally:
        await runtime.close()


async def test_ne_and_in_metadata_operators(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    for name, owner in (("one.md", "alice"), ("two.md", "bob")):
        (root / name).write_text(
            f"---\ntitle: {name}\ntype: note\ntags: [x]\nowner: {owner}\n---\n\nshared evidence\n",
            encoding="utf-8",
        )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        not_alice = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"owner": {"$ne": "alice"}})
        )
        in_list = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"owner": {"$in": ["bob", "carol"]}})
        )
        unknown_operator = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="evidence", metadata_filters={"owner": {"$regex": "a.*"}})
        )

        assert [item.path for item in not_alice.results] == ["two.md"]
        assert [item.path for item in in_list.results] == ["two.md"]
        assert not unknown_operator.results
    finally:
        await runtime.close()


async def test_tag_filter_uses_aliases_and_includes_hierarchy_descendants(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir(parents=True)
    (root / "AGENTWIKI.md").write_text(
        "---\ntag_aliases:\n  engineering:\n    - eng\n---\n", encoding="utf-8"
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
            "anchor": None,
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
        await first_runtime.retrieval.get_wiki_context(
            ContextQuery(query="SQLite", min_similarity=0.0)
        )
    finally:
        await first_runtime.close()

    second_provider = CountingEmbeddingProvider("model-b")
    second_runtime = await create_runtime(root, index, second_provider)
    try:
        result = await second_runtime.retrieval.get_wiki_context(
            ContextQuery(query="SQLite", min_similarity=0.0)
        )
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
            ContextQuery(query="SQLite", min_similarity=0.0)
        )
        assert result.strategy == "hybrid"
        cursor = await second_runtime.database.connection.execute(
            "SELECT DISTINCT status FROM wiki_vector_manifest"
        )
        assert {str(row[0]) for row in await cursor.fetchall()} == {"ready"}
    finally:
        await second_runtime.close()


async def test_wikilink_anchor_is_preserved_on_related_evidence(tmp_path) -> None:
    """`[[doc#section]]` must keep its fragment instead of discarding it."""
    root = tmp_path / "documents"
    root.mkdir()
    (root / "source.md").write_text(
        "---\ntitle: 来源\ntype: note\ntags: [x]\n---\n# 来源\n\n详见 [[target#回滚流程]]。\n",
        encoding="utf-8",
    )
    (root / "target.md").write_text(
        "---\ntitle: 目标\ntype: note\ntags: [x]\n---\n# 目标\n\n## 回滚流程\n\n正文。\n",
        encoding="utf-8",
    )
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        result = await runtime.retrieval.get_wiki_context(ContextQuery(query="来源"))
        related = result.results[0].related

        assert [(item.path, item.anchor) for item in related] == [("target.md", "回滚流程")]
    finally:
        await runtime.close()


async def test_index_records_that_a_full_pass_completed(tmp_path) -> None:
    """A zero row count cannot distinguish "indexed and empty" from "never indexed"."""
    root = tmp_path / "documents"
    root.mkdir()
    (root / "guide.md").write_text("# Guide\n\nBody\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        assert await runtime.repository.indexed_once() is False

        report = await runtime.synchronizer.rebuild()

        assert report.indexed_once is True
        assert report.generation
        assert await runtime.repository.indexed_once() is True
        assert await runtime.repository.index_generation() == report.generation
    finally:
        await runtime.close()


async def test_unchanged_document_set_takes_the_fast_path(tmp_path, monkeypatch) -> None:
    """An identical document set must not re-read the fingerprint table."""
    root = tmp_path / "documents"
    root.mkdir()
    (root / "guide.md").write_text("# Guide\n\nBody\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        first = await runtime.synchronizer.ensure_fresh()
        assert first.generation

        calls = 0
        original = runtime.repository.fingerprints

        async def counting() -> dict[str, DocumentFingerprint]:
            nonlocal calls
            calls += 1
            return await original()

        monkeypatch.setattr(runtime.repository, "fingerprints", counting)
        second = await runtime.synchronizer.ensure_fresh()

        assert calls == 0, "fast path should skip the fingerprint query"
        assert second.unchanged == 1
        assert second.generation == first.generation
    finally:
        await runtime.close()


async def test_editing_a_document_leaves_the_fast_path(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    path = root / "guide.md"
    path.write_text("# Guide\n\nOld\n", encoding="utf-8")
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        first = await runtime.synchronizer.ensure_fresh()
        path.write_text("# Guide\n\nNew body\n", encoding="utf-8")
        stat = path.stat()
        os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))

        second = await runtime.synchronizer.ensure_fresh()

        assert second.indexed == 1
        assert second.generation != first.generation
    finally:
        await runtime.close()
