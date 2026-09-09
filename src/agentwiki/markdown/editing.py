"""Small, deterministic Markdown body editing operations."""

import re

_HEADING = re.compile(r"^(#{1,6})[ \t]+(.+?)[ \t]*#*[ \t]*$", re.MULTILINE)


def edit_content(
    content: str,
    *,
    operation: str,
    value: str,
    find_text: str | None = None,
    section: str | None = None,
    expected_replacements: int = 1,
    replace_subsections: bool = True,
) -> str:
    """Apply one Basic Memory-style edit operation to a Markdown body."""
    if operation == "append":
        return content.rstrip() + "\n\n" + value.strip() if content.strip() else value.strip()
    if operation == "prepend":
        return value.strip() + "\n\n" + content.lstrip() if content.strip() else value.strip()
    if operation == "find_replace":
        if find_text is None:
            raise ValueError("find_text is required for find_replace")
        count = content.count(find_text)
        if count != expected_replacements:
            raise ValueError(
                f"expected {expected_replacements} replacements, found {count}"
            )
        return content.replace(find_text, value)
    if operation in {"replace_section", "insert_before_section", "insert_after_section"}:
        if not section:
            raise ValueError("section is required for section editing")
        return _edit_section(
            content,
            operation=operation,
            section=section,
            value=value,
            replace_subsections=replace_subsections,
        )
    raise ValueError(f"unsupported edit operation: {operation}")


def _edit_section(
    content: str,
    *,
    operation: str,
    section: str,
    value: str,
    replace_subsections: bool,
) -> str:
    headings = list(_HEADING.finditer(content))
    selector = section.strip()
    duplicate_index = 0
    duplicate_match = re.search(r"\[(\d+)\]\s*$", selector)
    if duplicate_match:
        duplicate_index = int(duplicate_match.group(1))
        selector = selector[: duplicate_match.start()].rstrip()
    segments = [segment.lstrip("#").strip().casefold() for segment in selector.split("/")]
    stack: list[tuple[int, str]] = []
    occurrences: dict[tuple[str, ...], int] = {}
    match = None
    for candidate in headings:
        level = len(candidate.group(1))
        while stack and stack[-1][0] >= level:
            stack.pop()
        stack.append((level, candidate.group(2).casefold()))
        path = tuple(text for _, text in stack)
        occurrence = occurrences.get(path, 0)
        occurrences[path] = occurrence + 1
        if path == tuple(segments) and occurrence == duplicate_index:
            match = candidate
            break
    if match is None:
        raise ValueError(f"section not found: {section}")
    level = len(match.group(1))
    end = len(content)
    for candidate in headings:
        if candidate.start() <= match.start():
            continue
        candidate_level = len(candidate.group(1))
        if candidate_level <= level or not replace_subsections:
            end = candidate.start()
            break
    if operation == "replace_section":
        return content[: match.end()] + "\n\n" + value.strip() + "\n\n" + content[end:]
    if operation == "insert_before_section":
        return content[: match.start()] + value.strip() + "\n\n" + content[match.start():]
    return content[:end].rstrip() + "\n\n" + value.strip() + "\n\n" + content[end:]
