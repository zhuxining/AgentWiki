"""Deterministic extraction of document-to-document Wiki relationships."""

from dataclasses import dataclass
from pathlib import PurePosixPath
import posixpath
import re

from agentwiki.domain.documents import DocumentPath, WikiDocument

_WIKILINK = re.compile(r"\[\[([^\]|#]+)(?:#[^\]|]+)?(?:\|[^\]]+)?\]\]")
_MARKDOWN_LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+[^)]*)?\)")


@dataclass(frozen=True, slots=True)
class GraphEdgeDraft:
    target_path: str
    relation_type: str
    source_kind: str
    anchor: str | None = None
    source_section: str | None = None
    context: str | None = None


@dataclass(frozen=True, slots=True)
class GraphExtraction:
    edges: tuple[GraphEdgeDraft, ...]
    warnings: tuple[str, ...] = ()


def extract_edges(document: WikiDocument) -> tuple[GraphEdgeDraft, ...]:
    """Extract safe, explicit links and Frontmatter relations from one document."""
    return extract_graph(document).edges


def extract_graph(document: WikiDocument) -> GraphExtraction:
    """Extract explicit relationships and report malformed relationship declarations."""
    edges: list[GraphEdgeDraft] = []
    warnings: list[str] = []
    for match in _WIKILINK.finditer(document.content):
        target = _normalize_target(document.path.value, match.group(1))
        if target is not None:
            edges.append(
                GraphEdgeDraft(
                    target,
                    "links_to",
                    "wikilink",
                    source_section=_section_at(document.content, match.start()),
                    context=_context_at(document.content, match.start()),
                )
            )
    for match in _MARKDOWN_LINK.finditer(document.content):
        target = _normalize_target(document.path.value, match.group(1))
        if target is not None:
            edges.append(
                GraphEdgeDraft(
                    target,
                    "links_to",
                    "markdown_link",
                    source_section=_section_at(document.content, match.start()),
                    context=_context_at(document.content, match.start()),
                )
            )

    raw_relations = document.frontmatter.get("relations", [])
    if "relations" in document.frontmatter and not isinstance(raw_relations, list):
        warnings.append("relations must be a list of {type, target} mappings")
    elif isinstance(raw_relations, list):
        for index, relation in enumerate(raw_relations):
            if not isinstance(relation, dict):
                warnings.append(f"relations[{index}] must be a mapping")
                continue
            target_value = relation.get("target")
            relation_type = relation.get("type")
            if not isinstance(target_value, str) or not isinstance(relation_type, str):
                warnings.append(f"relations[{index}] requires string type and target")
                continue
            target = _normalize_target(document.path.value, target_value)
            relation_name = relation_type.strip().casefold()
            if target is None:
                warnings.append(f"relations[{index}] target is outside the Wiki root")
            elif not relation_name:
                warnings.append(f"relations[{index}] type must not be empty")
            else:
                edges.append(
                    GraphEdgeDraft(
                        target,
                        relation_name,
                        "frontmatter",
                        source_section="Frontmatter",
                        context=f"{relation_name}: {target}",
                    )
                )

    unique = {(edge.target_path, edge.relation_type, edge.source_kind): edge for edge in edges}
    return GraphExtraction(tuple(unique.values()), tuple(warnings))


def _section_at(content: str, offset: int) -> str | None:
    headings: list[str] = []
    position = 0
    for line in content.splitlines(keepends=True):
        if position > offset:
            break
        match = re.match(r"^(#{1,6})\s+(.+?)\s*$", line.rstrip("\n"))
        if match:
            level = len(match.group(1))
            headings[level - 1 :] = [match.group(2).strip()]
        position += len(line)
    return " / ".join(headings) or None


def _context_at(content: str, offset: int) -> str | None:
    start = content.rfind("\n", 0, offset) + 1
    end = content.find("\n", offset)
    line = content[start:] if end == -1 else content[start:end]
    return " ".join(line.split()) or None


def _normalize_target(source_path: str, raw_target: str) -> str | None:
    target = raw_target.strip().strip("<>").split("#", 1)[0].strip()
    if not target or "://" in target or target.startswith(("mailto:", "#")):
        return None
    target = target.replace("\\", "/")
    if not target.lower().endswith(".md"):
        target = f"{target}.md"
    if target.startswith("/"):
        target = target.lstrip("/")
    else:
        target = str(PurePosixPath(source_path).parent / target)
    target = posixpath.normpath(target)
    if target == ".." or target.startswith("../"):
        return None
    try:
        return DocumentPath(value=target).value
    except ValueError:
        return None
