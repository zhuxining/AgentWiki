import pytest

from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.services.governance import GovernanceService


def test_rules_and_validation_apply_to_native_documents(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text(
        "---\n"
        "name: Team Wiki\n"
        "required_fields: [owner]\n"
        "sections:\n  - path: guides\n    types: [guide]\n"
        "---\n"
        "# Use the Wiki\n",
        encoding="utf-8",
    )
    guides = library.root / "guides"
    guides.mkdir()
    (guides / "one.md").write_text(
        "---\ntype: note\nextra: value\n---\n\nSee [missing](missing.md)\n",
        encoding="utf-8",
    )
    service = GovernanceService(library)
    report = service.validate_wiki("guides/one.md")
    assert report.status == "failed"
    assert {issue.code for issue in report.errors} == {
        "frontmatter.required",
        "type.not_allowed",
    }
    assert "link.broken" in {issue.code for issue in report.warnings}
    assert service.get_wiki_rules("guides").guide_content.startswith("# Use")


def test_rules_load_from_agentwiki_frontmatter_and_body(tmp_path) -> None:
    """The control file carries rules in frontmatter and guidance in the body."""
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text(
        "---\n"
        "name: Team Wiki\n"
        "required_fields: [owner]\n"
        "tag_aliases:\n  architecture:\n    - arch\n"
        "sections:\n  - path: guides\n    types: [guide]\n"
        "---\n"
        "# Wiki 使用指南\n\n先检索再读取原文。\n",
        encoding="utf-8",
    )
    guides = library.root / "guides"
    guides.mkdir()
    (guides / "one.md").write_text("---\ntype: note\n---\n\nBody\n", encoding="utf-8")

    service = GovernanceService(library)
    rules = service.get_wiki_rules("guides")
    report = service.validate_wiki("guides/one.md")

    assert rules.name == "Team Wiki"
    assert rules.required_fields == ("owner",)
    assert rules.guide_content.startswith("# Wiki 使用指南")
    assert rules.tag_aliases == {"architecture": ("arch",)}
    assert {issue.code for issue in report.errors} == {
        "frontmatter.required",
        "type.not_allowed",
    }


def test_wiki_without_control_file_has_no_rules(tmp_path) -> None:
    """No AGENTWIKI.md means no configured rules, so validation reports nothing."""
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "note.md").write_text("# Note\n\nBody\n", encoding="utf-8")

    service = GovernanceService(library)
    rules = service.get_wiki_rules()
    report = service.validate_wiki("note.md")

    assert rules.required_fields == ()
    assert rules.sections == ()
    assert report.status == "passed"
    assert report.errors == ()


def test_agentwiki_frontmatter_must_be_a_mapping(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text("---\n- item\n---\n\nBody\n", encoding="utf-8")

    with pytest.raises(ValueError):
        GovernanceService(library).get_wiki_rules()


def test_validation_reports_formatting_without_rewriting(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    path = library.root / "rough.md"
    original = "# Heading\n\n-   item"
    path.write_text(original, encoding="utf-8")
    report = GovernanceService(library).validate_wiki("rough.md")
    assert "markdown.formatting" in {issue.code for issue in report.warnings}
    assert path.read_text(encoding="utf-8") == original


@pytest.mark.parametrize(
    "removed_rule",
    [
        "block_on:\n  - frontmatter.required\n",
        "sections:\n  - path: guides\n    allowed_fields: [type]\n",
        "allowed_fields: [type, owner]\n",
        "required: [owner]\n",
    ],
)
def test_rules_reject_removed_or_renamed_fields(
    tmp_path,
    removed_rule: str,
) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text(
        f"---\n{removed_rule}---\n\nGuide\n", encoding="utf-8"
    )

    with pytest.raises(ValueError):
        GovernanceService(library).get_wiki_rules()


def test_filename_pattern_returns_validation_error(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text(
        "---\nsections:\n  - path: decisions\n    filename_pattern: 'decision-*.md'\n---\n",
        encoding="utf-8",
    )
    decisions = library.root / "decisions"
    decisions.mkdir()
    (decisions / "wrong.md").write_text("# Decision\n", encoding="utf-8")

    report = GovernanceService(library).validate_wiki("decisions/wrong.md")

    assert "path.filename" in {issue.code for issue in report.errors}


def test_configured_and_scoped_required_fields_are_additive(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text(
        "---\n"
        "required_fields: [owner]\n"
        "sections:\n  - path: decisions\n    required_fields: [decided_at]\n"
        "---\n",
        encoding="utf-8",
    )

    rules = GovernanceService(library).get_wiki_rules("decisions/one.md")

    assert rules.required_fields == ("owner", "decided_at")


def test_tag_aliases_warn_and_new_tags_remain_allowed(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    (library.root / "AGENTWIKI.md").write_text(
        "---\ntag_aliases:\n  architecture:\n    - Architecture\n    - arch\n---\n",
        encoding="utf-8",
    )
    valid_frontmatter = (
        "title: Example\ntype: note\ntags: [architecture]\n"
        "created_at: 2026-09-10\nupdated_at: 2026-09-10\n"
    )
    (library.root / "valid.md").write_text(
        f"---\n{valid_frontmatter}---\n\nValid\n",
        encoding="utf-8",
    )
    (library.root / "invalid.md").write_text(
        f"---\n{valid_frontmatter.replace('[architecture]', '[Architecture, arch, new-tag]')}"
        "---\n\nAllowed with warnings\n",
        encoding="utf-8",
    )

    service = GovernanceService(library)

    assert not service.validate_wiki("valid.md").errors
    report = service.validate_wiki("invalid.md")
    assert report.status == "passed"
    assert {issue.code for issue in report.warnings} >= {
        "tags.non_canonical",
        "tags.new",
    }
    assert "tags.duplicate" not in {issue.code for issue in report.warnings}
    catalog = {item.tag: item for item in service.get_wiki_rules().known_tags}
    assert catalog["architecture"].count == 3
    assert catalog["architecture"].aliases_seen == ("arch", "Architecture")
    assert catalog["new-tag"].count == 1


def test_repeated_unconfigured_tag_is_known_without_new_warning(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    frontmatter = (
        "title: Example\ntype: note\ntags: [shared]\n"
        "created_at: 2026-09-10\nupdated_at: 2026-09-10\n"
    )
    for name in ("one.md", "two.md"):
        (library.root / name).write_text(
            f"---\n{frontmatter}---\n\nBody\n", encoding="utf-8"
        )

    report = GovernanceService(library).validate_wiki("one.md")

    assert "tags.new" not in {issue.code for issue in report.warnings}


def test_tags_must_be_a_string_list(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    path = library.root / "invalid-tags.md"
    path.write_text(
        "---\ntitle: Example\ntype: note\ntags: architecture\n"
        "created_at: 2026-09-10\nupdated_at: 2026-09-10\n---\n\nBody\n",
        encoding="utf-8",
    )

    report = GovernanceService(library).validate_wiki("invalid-tags.md")

    assert "tags.invalid" in {issue.code for issue in report.errors}
