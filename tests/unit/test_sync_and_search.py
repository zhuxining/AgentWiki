from collections.abc import Sequence
import os

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
    reserved = root / "_agentwiki"
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
