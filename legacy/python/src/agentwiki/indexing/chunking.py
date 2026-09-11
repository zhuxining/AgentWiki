"""Deterministic, heading-aware Markdown chunking."""

from hashlib import sha256
import re

from agentwiki.domain.documents import WikiDocument
from agentwiki.domain.retrieval import IndexedChunk

_HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
_MAX_CHARS = 1_200
_OVERLAP_CHARS = 150
_MIN_CHUNK_CHARS = 1


def chunk_document(document: WikiDocument) -> tuple[IndexedChunk, ...]:
    """Split a document by heading, then bound oversized sections by paragraphs.

    Blank sections (a heading immediately followed by a subheading) are not emitted:
    an empty chunk would otherwise be indexed, embedded, and returned as empty
    evidence. A document that contains no content at all still produces one chunk so
    that it remains addressable in the projection.
    """
    sections: list[tuple[str, str]] = []
    headings: list[str] = []
    body: list[str] = []
    emitted = False

    def flush() -> None:
        nonlocal emitted
        content = "\n".join(body).strip()
        body.clear()
        if not content:
            return
        sections.append((" / ".join(headings), content))
        emitted = True

    for line in document.content.splitlines():
        match = _HEADING.match(line)
        if match:
            flush()
            level = len(match.group(1))
            headings[level - 1 :] = [match.group(2).strip()]
            continue
        body.append(line)
    flush()
    if not emitted:
        sections.append((" / ".join(headings), ""))

    chunks: list[IndexedChunk] = []
    tags = document.frontmatter.get("tags", [])
    tag_text = " ".join(str(item) for item in tags) if isinstance(tags, list) else str(tags)
    ordinal = 0
    for section, content in sections:
        for fragment in _split_oversized(content):
            source = f"{section}\n{fragment}".strip()
            chunk_id = sha256(
                f"{document.path.value}\0{ordinal}\0{source}".encode()
            ).hexdigest()
            chunks.append(
                IndexedChunk(
                    chunk_id=chunk_id,
                    ordinal=ordinal,
                    section=section,
                    content=fragment,
                    source_hash=sha256(source.encode()).hexdigest(),
                    embedding_hash=sha256(
                        f"{document.title}\n{tag_text}\n{source}".encode()
                    ).hexdigest(),
                )
            )
            ordinal += 1
    return tuple(chunks)


def _split_oversized(content: str) -> tuple[str, ...]:
    """Split content into fragments that never exceed ``_MAX_CHARS``.

    The overlap carried into the next fragment is budgeted against the limit, so a
    paragraph plus its overlap can no longer overshoot the bound.
    """
    if len(content) <= _MAX_CHARS:
        return (content,)
    paragraphs = [part.strip() for part in re.split(r"\n\s*\n", content) if part.strip()]
    chunks: list[str] = []
    current = ""
    for paragraph in paragraphs:
        if len(paragraph) > _MAX_CHARS:
            if current:
                chunks.append(current)
                current = ""
            chunks.extend(_window(paragraph))
            continue
        candidate = f"{current}\n\n{paragraph}" if current else paragraph
        if len(candidate) <= _MAX_CHARS:
            current = candidate
            continue
        if current:
            chunks.append(current)
        overlap = _overlap_for(current, paragraph)
        current = f"{overlap}\n\n{paragraph}" if overlap else paragraph
    if current:
        chunks.append(current)
    return tuple(chunks)


def _overlap_for(previous: str, upcoming: str) -> str:
    """Return the overlap prefix that keeps the next fragment inside the bound."""
    budget = _MAX_CHARS - len(upcoming) - 2
    if budget <= 0:
        return ""
    return previous[-min(_OVERLAP_CHARS, budget) :].lstrip()


def _window(text: str) -> list[str]:
    """Hard-wrap one oversized paragraph, carrying overlap between windows."""
    step = _MAX_CHARS - _OVERLAP_CHARS
    windows: list[str] = []
    start = 0
    while start < len(text):
        window = text[start : start + _MAX_CHARS]
        if windows and len(window) <= _OVERLAP_CHARS:
            # A tail that is nothing but repeated overlap adds no information.
            break
        windows.append(window)
        if start + _MAX_CHARS >= len(text):
            break
        start += step
    return windows
