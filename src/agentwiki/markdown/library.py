"""Read-only, path-safe access to a local Markdown Wiki."""

from pathlib import Path

import yaml

from agentwiki.domain.documents import DocumentDescriptor, DocumentPath, Frontmatter, WikiDocument

RESERVED_FILE = "AGENTWIKI.md"
LEGACY_RESERVED_DIRECTORY = "agentwiki"


class MarkdownLibrary:
    """Discover and parse Markdown below one configured root."""

    def __init__(self, root: Path) -> None:
        self.root = root.expanduser().resolve()
        self.root.mkdir(parents=True, exist_ok=True)

    def path_for(self, document_path: DocumentPath) -> Path:
        candidate = (self.root / document_path.value).resolve()
        try:
            candidate.relative_to(self.root)
        except ValueError as exc:
            raise ValueError("document path escapes the Wiki root") from exc
        return candidate

    def descriptors(self) -> tuple[DocumentDescriptor, ...]:
        """Return safe Markdown files without parsing their contents."""
        descriptors: list[DocumentDescriptor] = []
        for candidate in sorted(self.root.rglob("*.md")):
            relative = candidate.relative_to(self.root)
            if relative.name == RESERVED_FILE or LEGACY_RESERVED_DIRECTORY in relative.parts:
                continue
            try:
                resolved = candidate.resolve(strict=True)
                resolved.relative_to(self.root)
                if not resolved.is_file():
                    continue
                stat = resolved.stat()
                descriptors.append(
                    DocumentDescriptor(
                        path=DocumentPath(value=relative.as_posix()),
                        modified_at_ns=stat.st_mtime_ns,
                        size=stat.st_size,
                    )
                )
            except (OSError, ValueError):
                continue
        return tuple(descriptors)

    def read(self, descriptor: DocumentDescriptor | DocumentPath) -> WikiDocument:
        if isinstance(descriptor, DocumentPath):
            path = self.path_for(descriptor)
            stat = path.stat()
            descriptor = DocumentDescriptor(descriptor, stat.st_mtime_ns, stat.st_size)
        path = self.path_for(descriptor.path)
        raw = path.read_text(encoding="utf-8")
        content, frontmatter = self.parse(raw)
        return WikiDocument(
            path=descriptor.path,
            content=content,
            frontmatter=frontmatter,
            modified_at_ns=descriptor.modified_at_ns,
            size=descriptor.size,
        )

    def descriptor(self, document_path: DocumentPath) -> DocumentDescriptor:
        path = self.path_for(document_path)
        stat = path.stat()
        return DocumentDescriptor(document_path, stat.st_mtime_ns, stat.st_size)

    def raw(self, document_path: DocumentPath) -> str:
        return self.path_for(document_path).read_text(encoding="utf-8")

    def reserved_text(self, name: str) -> str:
        if name not in {RESERVED_FILE, "context.yaml", "guide.md"}:
            raise ValueError("unknown reserved Wiki file")
        path = self.root / RESERVED_FILE
        if name == RESERVED_FILE and path.is_file():
            return path.read_text(encoding="utf-8")
        legacy = self.root / LEGACY_RESERVED_DIRECTORY / name
        return legacy.read_text(encoding="utf-8") if legacy.is_file() else ""

    @staticmethod
    def parse(raw: str) -> tuple[str, Frontmatter]:
        frontmatter: Frontmatter = {}
        content = raw
        if raw.startswith("---"):
            lines = raw.splitlines(keepends=True)
            if lines and lines[0].strip() == "---":
                end = next(
                    (index for index, line in enumerate(lines[1:], 1) if line.strip() == "---"),
                    None,
                )
                if end is not None:
                    try:
                        parsed = yaml.safe_load("".join(lines[1:end]))
                    except yaml.YAMLError as exc:
                        raise ValueError(f"invalid YAML Frontmatter: {exc}") from exc
                    if parsed is not None and not isinstance(parsed, dict):
                        raise ValueError("Frontmatter must be a YAML mapping")
                    frontmatter = dict(parsed or {})
                    content = "".join(lines[end + 1 :]).lstrip("\n").rstrip("\n")
        return content, frontmatter
