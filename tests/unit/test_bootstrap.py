"""Startup behaviour: a Wiki without a control file gets the packaged default."""

import os

from agentwiki.domain.documents import RESERVED_FILE
from agentwiki.domain.governance import WikiRules
from agentwiki.domain.retrieval import ContextQuery
from agentwiki.markdown.library import MarkdownLibrary, default_control_text
from agentwiki.runtime.context import create_runtime


def test_default_template_is_a_valid_control_file() -> None:
    """The packaged template must parse as AGENTWIKI.md and yield usable rules."""
    body, frontmatter = MarkdownLibrary.parse(default_control_text())

    assert frontmatter["name"] == "AgentWiki"
    assert body.startswith("# Wiki 使用指南")
    rules = WikiRules.model_validate(frontmatter)
    assert rules.required_fields == ("title", "type", "tags")
    assert rules.default_type == "note"


def test_default_template_explains_itself_to_the_reader() -> None:
    """A generated control file must tell the reader it is generated and how to relax it."""
    body, _ = MarkdownLibrary.parse(default_control_text())

    assert "仅在缺失时写入" in body
    assert "required_fields" in body
    assert "frontmatter.required" in body


async def test_startup_seeds_the_control_file_and_enforces_its_rules(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "note.md").write_text("# Note\n\nBody\n", encoding="utf-8")
    index = tmp_path / "index.sqlite3"

    runtime = await create_runtime(root, index)
    try:
        assert (root / RESERVED_FILE).is_file()
        rules = runtime.governance.get_wiki_rules()
        assert rules.required_fields == ("title", "type", "tags")
        assert rules.guide_content.startswith("# Wiki 使用指南")

        report = runtime.governance.validate_wiki("note.md")
        # One error per missing required field, in the configured order.
        assert [issue.code for issue in report.errors] == ["frontmatter.required"] * 3
        assert [issue.field for issue in report.errors] == ["title", "type", "tags"]
    finally:
        await runtime.close()


async def test_startup_never_overwrites_an_existing_control_file(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    original = "---\nname: Hand Written\nrequired_fields: [owner]\n---\n\n# Custom\n"
    (root / RESERVED_FILE).write_text(original, encoding="utf-8")
    index = tmp_path / "index.sqlite3"

    runtime = await create_runtime(root, index)
    try:
        assert (root / RESERVED_FILE).read_text(encoding="utf-8") == original
        assert runtime.governance.get_wiki_rules().name == "Hand Written"
    finally:
        await runtime.close()


async def test_seeded_control_file_is_not_indexed_as_a_document(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "note.md").write_text("# Note\n\nBody\n", encoding="utf-8")

    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        await runtime.synchronizer.rebuild()
        paths = {descriptor.path.value for descriptor in runtime.library.descriptors()}
        assert paths == {"note.md"}
        result = await runtime.retrieval.get_wiki_context(
            ContextQuery(query="Wiki 使用指南")
        )
        assert all(item.path != RESERVED_FILE for item in result.results)
    finally:
        await runtime.close()


def test_ensure_control_file_reports_whether_it_created_the_file(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")

    assert library.ensure_control_file() is True
    assert library.ensure_control_file() is False
    assert library.reserved_text().startswith("---")


def test_ensure_control_file_never_writes_through_a_dangling_symlink(tmp_path) -> None:
    """A symlink target outside the root must stay untouched."""
    library = MarkdownLibrary(tmp_path / "documents")
    target = tmp_path / "outside.md"
    (library.root / RESERVED_FILE).symlink_to(target)

    assert library.ensure_control_file() is False
    assert not target.exists()
    assert (library.root / RESERVED_FILE).is_symlink()


def test_ensure_control_file_leaves_a_linked_control_file_intact(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    target = tmp_path / "shared-rules.md"
    target.write_text("---\nname: Shared\n---\n\n# Shared guidance\n", encoding="utf-8")
    (library.root / RESERVED_FILE).symlink_to(target)

    assert library.ensure_control_file() is False
    assert target.read_text(encoding="utf-8").startswith("---\nname: Shared")


async def test_rules_carry_the_control_file_identity(tmp_path) -> None:
    """A caller caching rules can compare these to notice they went stale."""
    root = tmp_path / "documents"
    root.mkdir()
    control = root / RESERVED_FILE
    control.write_text("---\nname: First\n---\n", encoding="utf-8")

    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        first = runtime.governance.get_wiki_rules()
        assert first.source_modified_at_ns > 0
        assert first.source_size == control.stat().st_size

        control.write_text("---\nname: Second\n---\n\n# Changed\n", encoding="utf-8")
        stat = control.stat()
        os.utime(control, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))
        second = runtime.governance.get_wiki_rules()

        assert second.name == "Second"
        assert (second.source_modified_at_ns, second.source_size) != (
            first.source_modified_at_ns,
            first.source_size,
        )
    finally:
        await runtime.close()


async def test_rules_without_a_control_file_report_zero_identity(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        (root / RESERVED_FILE).unlink()
        rules = runtime.governance.get_wiki_rules()

        assert rules.source_modified_at_ns == 0
        assert rules.source_size == 0
    finally:
        await runtime.close()


async def test_known_tags_are_recomputed_when_the_control_file_changes(tmp_path) -> None:
    root = tmp_path / "documents"
    root.mkdir()
    (root / "note.md").write_text(
        "---\ntitle: N\ntype: note\ntags: [alpha]\n---\n\nBody\n", encoding="utf-8"
    )
    control = root / RESERVED_FILE
    control.write_text("---\nrequired_fields: []\n---\n", encoding="utf-8")

    runtime = await create_runtime(root, tmp_path / "index.sqlite3")
    try:
        before = {item.tag for item in runtime.governance.get_wiki_rules().known_tags}
        assert before == {"alpha"}

        control.write_text(
            "---\nrequired_fields: []\ntag_aliases:\n  alpha:\n    - a\n---\n", encoding="utf-8"
        )
        stat = control.stat()
        os.utime(control, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))
        after = {item.tag for item in runtime.governance.get_wiki_rules().known_tags}

        assert after == {"alpha"}
        assert runtime.governance.get_wiki_rules().tag_aliases == {"alpha": ("a",)}
    finally:
        await runtime.close()
