"""Deterministic, heading-aware Markdown chunking."""

from hashlib import sha256
import re

from agentwiki.domain.documents import WikiDocument
from agentwiki.domain.retrieval import IndexedChunk

_HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
_MAX_CHARS = 1_200
_OVERLAP_CHARS = 150


def chunk_document(document: WikiDocument) -> tuple[IndexedChunk, ...]:
    """Split a document by heading, then bound oversized sections by paragraphs."""
    sections: list[tuple[str, str]] = []
    headings: list[str] = []
    body: list[str] = []

    def flush() -> None:
        content = "\n".join(body).strip()
        if content or not sections:
            sections.append((" / ".join(headings), content))
        body.clear()

    for line in document.content.splitlines():
        match = _HEADING.match(line)
        if match:
            if body:
                flush()
            level = len(match.group(1))
            headings[level - 1 :] = [match.group(2).strip()]
            continue
        body.append(line)
    if body or not sections:
        flush()

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
        chunks.append(current)
        overlap = current[-_OVERLAP_CHARS:].lstrip()
        current = f"{overlap}\n\n{paragraph}" if overlap else paragraph
    if current:
        chunks.append(current)
    return tuple(chunks)


def _window(text: str) -> list[str]:
    step = _MAX_CHARS - _OVERLAP_CHARS
    return [text[start : start + _MAX_CHARS] for start in range(0, len(text), step)]
