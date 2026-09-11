import pytest

from agentwiki.domain.documents import DocumentPath
from agentwiki.markdown.library import MarkdownLibrary


def test_document_path_rejects_escape_and_non_markdown() -> None:
    for value in ("/outside.md", "../outside.md", "notes/today.txt"):
        with pytest.raises(ValueError):
            DocumentPath(value=value)


def test_library_excludes_control_file_and_external_symlink(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "valid.md").write_text("valid\n", encoding="utf-8")
    (library.root / "AGENTWIKI.md").write_text("---\nname: x\n---\n", encoding="utf-8")
    nested = library.root / "notes"
    nested.mkdir()
    (nested / "guide.md").write_text("guide\n", encoding="utf-8")
    outside = tmp_path / "outside.md"
    outside.write_text("outside\n", encoding="utf-8")
    try:
        (library.root / "external.md").symlink_to(outside)
    except OSError as exc:
        pytest.skip(f"symbolic links are unavailable: {exc}")

    # Only the reserved control file is excluded; every other Markdown file is content.
    assert [item.path.value for item in library.descriptors()] == [
        "notes/guide.md",
        "valid.md",
    ]


def test_library_parses_frontmatter_with_real_file_identity(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    path = library.root / "guide.md"
    path.write_text("---\ntitle: Guide\n---\n\nBody\n", encoding="utf-8")

    descriptor = library.descriptors()[0]
    document = library.read(descriptor)

    assert document.title == "Guide"
    assert document.content == "Body"
    assert document.modified_at_ns == path.stat().st_mtime_ns
    assert document.size == path.stat().st_size
