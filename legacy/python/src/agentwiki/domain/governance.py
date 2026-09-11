"""Domain models for Wiki context and validation."""

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator

from agentwiki.domain.tags import TagAliases, alias_lookup, is_valid_tag

Severity = Literal["error", "warning", "info"]


class WikiSectionRule(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    path: str
    description: str = ""
    types: tuple[str, ...] = ()
    required_fields: tuple[str, ...] = ()
    filename_pattern: str | None = None


class KnownTag(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    tag: str
    count: int = Field(ge=0)
    aliases_seen: tuple[str, ...] = ()


class WikiRules(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    version: int = Field(default=1, ge=1)
    name: str = "AgentWiki"
    purpose: str = ""
    default_type: str = "note"
    required_fields: tuple[str, ...] = ()
    tag_aliases: TagAliases = Field(default_factory=dict)
    sections: tuple[WikiSectionRule, ...] = ()
    guide_content: str = ""
    known_tags: tuple[KnownTag, ...] = ()
    # Identity of the control file that produced these rules. Callers that cache the
    # result (an agent reusing a previous call) can compare these to detect that the
    # rules went stale; both are 0 when the Wiki has no control file.
    source_modified_at_ns: int = 0
    source_size: int = 0

    @field_validator("tag_aliases")
    @classmethod
    def validate_tag_aliases(cls, value: TagAliases) -> TagAliases:
        destinations: dict[str, str] = {}
        for canonical, aliases in value.items():
            if canonical != canonical.casefold() or not is_valid_tag(canonical):
                raise ValueError("tag_aliases keys must be canonical lowercase tags")
            if any(not alias.strip() or not is_valid_tag(alias) for alias in aliases):
                raise ValueError("tag aliases must be valid tag spellings")
            for spelling, destination in alias_lookup({canonical: aliases}).items():
                existing = destinations.get(spelling)
                if existing is not None and existing != destination:
                    raise ValueError(f"tag alias {spelling!r} maps to multiple canonical tags")
                destinations[spelling] = destination
        return value


class ValidationIssue(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    code: str
    severity: Severity
    path: str
    message: str
    field: str | None = None


class ValidationReport(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")

    status: Literal["passed", "failed"]
    errors: tuple[ValidationIssue, ...] = ()
    warnings: tuple[ValidationIssue, ...] = ()
    infos: tuple[ValidationIssue, ...] = ()
    checked_paths: tuple[str, ...] = ()
