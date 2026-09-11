"""Wiki organization rules and deterministic validation."""

from fnmatch import fnmatch
from pathlib import Path
import re
from typing import Any

import yaml

from agentwiki.domain.documents import (
    RESERVED_FILE,
    DocumentDescriptor,
    DocumentPath,
    WikiDocument,
)
from agentwiki.domain.formatting import format_markdown
from agentwiki.domain.governance import (
    KnownTag,
    ValidationIssue,
    ValidationReport,
    WikiRules,
)
from agentwiki.domain.scope import normalize_scope
from agentwiki.domain.tags import canonicalize_tag, is_valid_tag
from agentwiki.services.ports import WikiLibrary


class GovernanceService:
    """Load reserved Wiki rules and validate native Markdown changes."""

    def __init__(self, library: WikiLibrary) -> None:
        self.library = library
        # Cached only to avoid re-reading every document on each rules call; invalidated
        # as soon as the control file's size or mtime changes.
        self._tag_cache_key: tuple[int, int] | None = None
        self._tag_cache: tuple[KnownTag, ...] | None = None

    def get_wiki_rules(self, scope: str = "") -> WikiRules:
        scope = normalize_scope(scope)
        rules = self._load_rules()
        return self._effective_rules(rules, scope, self._known_tags_for(rules))

    def get_tag_aliases(self) -> dict[str, tuple[str, ...]]:
        return self._load_rules().tag_aliases

    def _known_tags_for(self, rules: WikiRules) -> tuple[KnownTag, ...]:
        """Return the tag catalogue, recomputed only when the control file changes."""
        key = (rules.source_modified_at_ns, rules.source_size)
        if self._tag_cache is not None and self._tag_cache_key == key:
            return self._tag_cache
        known = self._known_tags(self._read_documents(self.library.descriptors()), rules)
        self._tag_cache_key = key
        self._tag_cache = known
        return known

    def _effective_rules(
        self,
        rules: WikiRules,
        scope: str,
        known_tags: tuple[KnownTag, ...],
    ) -> WikiRules:
        matching = tuple(rule for rule in rules.sections if self._matches(rule.path, scope))
        required_fields = list(dict.fromkeys(rules.required_fields))
        kind = rules.default_type
        for rule in sorted(matching, key=lambda item: len(item.path)):
            required_fields.extend(
                field for field in rule.required_fields if field not in required_fields
            )
            kind = rule.types[0] if rule.types else kind
        return rules.model_copy(
            update={
                "required_fields": tuple(required_fields),
                "default_type": kind,
                "sections": matching,
                "known_tags": known_tags,
            }
        )

    def validate_wiki(self, path: str | None = None, *, full: bool = False) -> ValidationReport:
        descriptors = self.library.descriptors()
        selected = (
            descriptors
            if full or path is None
            else tuple(descriptor for descriptor in descriptors if descriptor.path.value == path)
        )
        if path is not None and not selected:
            selected = (self._descriptor_for(path),)
        documents, failures = self._read_documents_with_failures(selected)
        base_rules = self._load_rules()
        catalog_documents = (
            documents if selected == descriptors else self._read_documents(descriptors)
        )
        known_tags = self._known_tags(catalog_documents, base_rules)
        known_tag_counts = {item.tag: item.count for item in known_tags}
        known = {descriptor.path.value for descriptor in descriptors}
        issues = [*failures]
        for document in documents:
            rules = self._effective_rules(base_rules, document.path.value, known_tags)
            issues.extend(self._validate_document(document, rules, known, known_tag_counts))
            raw = self.library.raw(document.path)
            if format_markdown(raw) != raw:
                issues.append(
                    ValidationIssue(
                        code="markdown.formatting",
                        severity="warning",
                        path=document.path.value,
                        message="Markdown does not match the configured mdformat style",
                    )
                )
        errors = tuple(item for item in issues if item.severity == "error")
        return ValidationReport(
            status="failed" if errors else "passed",
            errors=errors,
            warnings=tuple(item for item in issues if item.severity == "warning"),
            infos=tuple(item for item in issues if item.severity == "info"),
            checked_paths=tuple(document.path.value for document in documents),
        )

    def _read_documents(self, descriptors: tuple[DocumentDescriptor, ...]) -> list[WikiDocument]:
        return self._read_documents_with_failures(descriptors)[0]

    def _read_documents_with_failures(
        self, descriptors: tuple[DocumentDescriptor, ...]
    ) -> tuple[list[WikiDocument], list[ValidationIssue]]:
        documents: list[WikiDocument] = []
        failures: list[ValidationIssue] = []
        for descriptor in descriptors:
            try:
                documents.append(self.library.read(descriptor))
            except (OSError, UnicodeError, ValueError, yaml.YAMLError) as exc:
                failures.append(
                    ValidationIssue(
                        code="markdown.parse",
                        severity="error",
                        path=descriptor.path.value,
                        message=str(exc),
                    )
                )
        return documents, failures

    @staticmethod
    def _known_tags(documents: list[WikiDocument], rules: WikiRules) -> tuple[KnownTag, ...]:
        counts: dict[str, int] = dict.fromkeys(rules.tag_aliases, 0)
        spellings: dict[str, set[str]] = {}
        for document in documents:
            tags = document.frontmatter.get("tags")
            if not isinstance(tags, list):
                continue
            for raw_tag in tags:
                if not isinstance(raw_tag, str) or not is_valid_tag(raw_tag):
                    continue
                canonical = canonicalize_tag(raw_tag, rules.tag_aliases)
                counts[canonical] = counts.get(canonical, 0) + 1
                if raw_tag != canonical:
                    spellings.setdefault(canonical, set()).add(raw_tag)
        return tuple(
            KnownTag(
                tag=tag,
                count=count,
                aliases_seen=tuple(sorted(spellings.get(tag, ()), key=str.casefold)),
            )
            for tag, count in sorted(counts.items())
        )

    def _load_rules(self) -> WikiRules:
        """Read the single supported control file: ``AGENTWIKI.md``.

        Rules live in its frontmatter and the body becomes ``guide_content``. When the
        file is absent the Wiki simply has no rules, so validation reports nothing. The
        returned rules carry the control file's size and mtime so callers can tell
        whether a cached copy has gone stale.
        """
        rules_text = self.library.reserved_text()
        if not rules_text:
            return WikiRules()
        guide, frontmatter = self.library.parse(rules_text)
        data: dict[str, Any] = frontmatter or {}
        if not isinstance(data, dict):
            raise ValueError("AGENTWIKI.md frontmatter must be a YAML mapping")
        modified_at_ns, size = self._control_file_identity()
        return WikiRules.model_validate(
            {
                **data,
                "guide_content": guide,
                "source_modified_at_ns": modified_at_ns,
                "source_size": size,
            }
        )

    def _control_file_identity(self) -> tuple[int, int]:
        try:
            descriptor = self.library.descriptor(DocumentPath(value=RESERVED_FILE))
        except (OSError, ValueError):
            return 0, 0
        return descriptor.modified_at_ns, descriptor.size

    def _descriptor_for(self, path: str) -> DocumentDescriptor:
        document_path = DocumentPath(value=path)
        return self.library.descriptor(document_path)

    @staticmethod
    def _validate_document(
        document: WikiDocument,
        rules: WikiRules,
        known: set[str],
        known_tag_counts: dict[str, int],
    ) -> list[ValidationIssue]:
        result: list[ValidationIssue] = []
        actual_type = str(document.frontmatter.get("type", rules.default_type))
        types = tuple(rule_type for rule in rules.sections for rule_type in rule.types)
        if types and actual_type not in types:
            result.append(
                ValidationIssue(
                    code="type.not_allowed",
                    severity="error",
                    path=document.path.value,
                    field="type",
                    message=f"文档类型 {actual_type!r} 不在允许范围内",
                )
            )
        for rule in rules.sections:
            if rule.filename_pattern and not fnmatch(
                Path(document.path.value).name, rule.filename_pattern
            ):
                result.append(
                    ValidationIssue(
                        code="path.filename",
                        severity="error",
                        path=document.path.value,
                        message=f"文件名不符合目录规则 {rule.filename_pattern!r}",
                    )
                )
        for field in rules.required_fields:
            if field not in document.frontmatter:
                result.append(
                    ValidationIssue(
                        code="frontmatter.required",
                        severity="error",
                        path=document.path.value,
                        field=field,
                        message=f"缺少必填字段 {field!r}",
                    )
                )
        tags = document.frontmatter.get("tags")
        if tags is not None and (
            not isinstance(tags, list)
            or not all(isinstance(tag, str) and tag.strip() for tag in tags)
        ):
            result.append(
                ValidationIssue(
                    code="tags.invalid",
                    severity="error",
                    path=document.path.value,
                    field="tags",
                    message="tags 必须是非空字符串组成的列表",
                )
            )
        elif isinstance(tags, list):
            invalid = [tag for tag in tags if not is_valid_tag(tag)]
            if invalid:
                result.append(
                    ValidationIssue(
                        code="tags.invalid",
                        severity="error",
                        path=document.path.value,
                        field="tags",
                        message=f"标签格式无效: {', '.join(invalid)}",
                    )
                )
            canonical_tags = [canonicalize_tag(tag, rules.tag_aliases) for tag in tags]
            non_canonical = [
                f"{raw} -> {canonical}"
                for raw, canonical in zip(tags, canonical_tags, strict=True)
                if raw != canonical
            ]
            if non_canonical:
                result.append(
                    ValidationIssue(
                        code="tags.non_canonical",
                        severity="warning",
                        path=document.path.value,
                        field="tags",
                        message=f"建议使用规范标签: {', '.join(non_canonical)}",
                    )
                )
            configured = set(rules.tag_aliases)
            new_tags = sorted({
                tag
                for tag in canonical_tags
                if tag not in configured and known_tag_counts.get(tag, 0) <= 1
            })
            if new_tags:
                result.append(
                    ValidationIssue(
                        code="tags.new",
                        severity="warning",
                        path=document.path.value,
                        field="tags",
                        message=f"发现首次使用的新标签: {', '.join(new_tags)}",
                    )
                )
        for match in re.finditer(r"\]\(([^)#]+)(?:#[^)]+)?\)", document.content):
            target = match.group(1)
            if target.startswith(("http://", "https://", "mailto:")):
                continue
            target_path = (Path(document.path.value).parent / target).as_posix()
            target_path = target_path if target_path.endswith(".md") else f"{target_path}.md"
            if target_path not in known:
                result.append(
                    ValidationIssue(
                        code="link.broken",
                        severity="warning",
                        path=document.path.value,
                        message=f"内部链接目标不存在: {target}",
                    )
                )
        return result

    @staticmethod
    def _matches(pattern: str, path: str) -> bool:
        return fnmatch(path, pattern) or path.startswith(pattern.rstrip("/") + "/")
