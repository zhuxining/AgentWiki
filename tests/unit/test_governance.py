import pytest
import yaml

from agentwiki.markdown.library import MarkdownLibrary
from agentwiki.services.governance import GovernanceService


def test_rules_and_validation_apply_to_native_documents(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    reserved = library.root / "agentwiki"
    reserved.mkdir()
    (reserved / "context.yaml").write_text(
        yaml.safe_dump(
            {
                "name": "Team Wiki",
                "required_fields": ["owner"],
                "sections": [{"path": "guides", "types": ["guide"]}],
            }
        ),
        encoding="utf-8",
    )
    (reserved / "guide.md").write_text("# Use the Wiki\n", encoding="utf-8")
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
    reserved = library.root / "agentwiki"
    reserved.mkdir()
    (reserved / "context.yaml").write_text(removed_rule, encoding="utf-8")

    with pytest.raises(ValueError):
        GovernanceService(library).get_wiki_rules()


def test_filename_pattern_returns_validation_error(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    reserved = library.root / "agentwiki"
    reserved.mkdir()
    (reserved / "context.yaml").write_text(
        "sections:\n  - path: decisions\n    filename_pattern: 'decision-*.md'\n",
        encoding="utf-8",
    )
    decisions = library.root / "decisions"
    decisions.mkdir()
    (decisions / "wrong.md").write_text("# Decision\n", encoding="utf-8")

    report = GovernanceService(library).validate_wiki("decisions/wrong.md")

    assert "path.filename" in {issue.code for issue in report.errors}


def test_configured_and_scoped_required_fields_are_additive(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    reserved = library.root / "agentwiki"
    reserved.mkdir()
    (reserved / "context.yaml").write_text(
        "required_fields: [owner]\n"
        "sections:\n  - path: decisions\n    required_fields: [decided_at]\n",
        encoding="utf-8",
    )

    rules = GovernanceService(library).get_wiki_rules("decisions/one.md")

    assert rules.required_fields == ("owner", "decided_at")


def test_tag_aliases_warn_and_new_tags_remain_allowed(tmp_path) -> None:
    library = MarkdownLibrary(tmp_path / "documents")
    reserved = library.root / "agentwiki"
    reserved.mkdir()
    (reserved / "context.yaml").write_text(
        "tag_aliases:\n  architecture:\n    - Architecture\n    - arch\n",
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
