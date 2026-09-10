"""Read-only, path-safe access to a local Markdown Wiki."""

from hashlib import sha256
from importlib.resources import files as _package_files
import os
from pathlib import Path

import yaml

from agentwiki.domain.documents import (
    RESERVED_FILE,
    DocumentDescriptor,
    DocumentPath,
    Frontmatter,
    WikiDocument,
)

_TEMPLATE_PATH = "data/default/AGENTWIKI.md"


def default_control_text() -> str:
    """Return the packaged default control file.

    The template is a real ``AGENTWIKI.md`` (YAML frontmatter plus guidance body) so it
    can be reviewed and edited like any other control file. It lives in a plain data
    directory, not a Python package, so every module under ``agentwiki/`` is a real layer.
    """
    template = _package_files("agentwiki").joinpath(_TEMPLATE_PATH)
    return template.read_text(encoding="utf-8")


class _DirectoryCache:
    """Per-directory scan cache keyed by directory mtime.

    A directory's mtime changes when entries are added, removed, or renamed inside it,
    so unchanged directories are not read again. File content edits bump the file's
    own mtime and are still detected, because cached entries keep their ``mtime_ns``
    and ``size``. ``descriptors`` re-stats every file as a cheap safety net, so a
    missed directory-mtime update cannot surface a deleted path. Without this cache
    every query paid a full recursive walk of the Wiki.
    """

    __slots__ = ("_directories",)

    def __init__(self) -> None:
        self._directories: dict[
            str, tuple[int, dict[str, tuple[int, int]], tuple[str, ...]]
        ] = {}

    def scan(self, root: Path) -> dict[str, tuple[int, int, bool]]:
        """Return ``{absolute path: (mtime_ns, size, stat_verified)}`` for Markdown files.

        ``stat_verified`` is False for entries served from cache, which the caller
        re-validates with one ``stat`` so a file deleted without a directory-mtime
        bump can never be reported.
        """
        files: dict[str, tuple[int, int, bool]] = {}
        for directory, (entries, _children, reused) in self._scan_directories(root).items():
            prefix = f"{directory}{os.sep}"
            for name, (mtime_ns, size) in entries.items():
                files[f"{prefix}{name}"] = (mtime_ns, size, not reused)
        return files

    def _scan_directories(
        self, root: Path
    ) -> dict[str, tuple[dict[str, tuple[int, int]], tuple[str, ...], bool]]:
        """Walk the tree, reusing cached listings for directories whose mtime is unchanged.

        ``os.scandir`` is used instead of ``Path.iterdir`` because it exposes the
        ``DirEntry`` metadata without extra ``stat`` calls.
        """
        listings: dict[str, tuple[dict[str, tuple[int, int]], tuple[str, ...], bool]] = {}
        pending = [str(root)]
        while pending:
            key = pending.pop()
            try:
                directory_mtime = Path(key).stat().st_mtime_ns
            except OSError:
                self._directories.pop(key, None)
                continue
            cached = self._directories.get(key)
            if cached is not None and cached[0] == directory_mtime:
                listings[key] = (cached[1], cached[2], True)
                pending.extend(cached[2])
                continue
            entries: dict[str, tuple[int, int]] = {}
            children: list[str] = []
            try:
                with os.scandir(key) as iterator:
                    for entry in iterator:
                        if entry.is_dir(follow_symlinks=False):
                            children.append(entry.path)
                            pending.append(entry.path)
                            continue
                        if not entry.name.lower().endswith(".md"):
                            continue
                        if entry.name == RESERVED_FILE:
                            continue
                        if not entry.is_file(follow_symlinks=True):
                            continue
                        # Symlinks may point outside the Wiki root.
                        target = os.path.realpath(entry.path)
                        try:
                            Path(target).relative_to(root)
                        except ValueError:
                            continue
                        resolved = Path(target)
                        info = resolved.stat()
                        entries[resolved.name] = (info.st_mtime_ns, info.st_size)
            except OSError:
                self._directories.pop(key, None)
                continue
            self._directories[key] = (directory_mtime, entries, tuple(children))
            listings[key] = (entries, tuple(children), False)
        return listings


class MarkdownLibrary:
    """Discover and parse Markdown below one configured root."""

    def __init__(self, root: Path) -> None:
        self.root = root.expanduser().resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self._scan_cache = _DirectoryCache()

    def ensure_control_file(self) -> bool:
        """Seed the default control file into an empty Wiki.

        Returns True when this call created ``AGENTWIKI.md``. An existing file is never
        touched, so a hand-edited control file survives every later startup. A symlink
        (including a dangling one) is never written through: following it could place
        the template outside the Wiki root.
        """
        control = self.root / RESERVED_FILE
        if control.is_symlink() or control.exists():
            return False
        try:
            content = default_control_text()
        except (OSError, ModuleNotFoundError) as exc:
            raise RuntimeError(f"unable to load the default {RESERVED_FILE}") from exc
        control.write_text(content, encoding="utf-8")
        self._scan_cache = _DirectoryCache()
        return True

    def path_for(self, document_path: DocumentPath) -> Path:
        candidate = (self.root / document_path.value).resolve()
        try:
            candidate.relative_to(self.root)
        except ValueError as exc:
            raise ValueError("document path escapes the Wiki root") from exc
        return candidate

    def descriptors(self) -> tuple[DocumentDescriptor, ...]:
        """Return safe Markdown files without parsing their contents.

        Cached listings are re-validated with one ``stat`` per file so a file deleted
        since the last scan cannot be reported even if a directory's mtime was not
        updated. Use :meth:`snapshot` when the caller also needs a fingerprint of the
        whole tree.
        """
        return self.snapshot()[0]

    def snapshot(self) -> tuple[tuple[DocumentDescriptor, ...], str]:
        """Return the current descriptors plus a fingerprint of the whole tree.

        The fingerprint folds in every file's path, size and mtime, so the caller can
        skip its own full-table fingerprint comparison when nothing on disk changed.
        """
        discovered = self._scan_cache.scan(self.root)
        descriptors: list[DocumentDescriptor] = []
        digest = sha256()
        for raw_path, (cached_mtime, cached_size, fresh) in sorted(discovered.items()):
            resolved = Path(raw_path)
            try:
                relative = resolved.relative_to(self.root)
                if relative.name == RESERVED_FILE:
                    continue
                if fresh:
                    modified_at_ns, size = cached_mtime, cached_size
                else:
                    info = resolved.stat()
                    modified_at_ns, size = info.st_mtime_ns, info.st_size
            except (OSError, ValueError):
                continue
            relative_posix = relative.as_posix()
            descriptors.append(
                DocumentDescriptor(
                    path=DocumentPath(value=relative_posix),
                    modified_at_ns=modified_at_ns,
                    size=size,
                )
            )
            digest.update(f"{relative_posix}\0{modified_at_ns}\0{size}\0".encode())
        return tuple(descriptors), digest.hexdigest()

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
            content_hash=sha256(raw.encode("utf-8")).hexdigest(),
        )

    def descriptor(self, document_path: DocumentPath) -> DocumentDescriptor:
        path = self.path_for(document_path)
        stat = path.stat()
        return DocumentDescriptor(document_path, stat.st_mtime_ns, stat.st_size)

    def raw(self, document_path: DocumentPath) -> str:
        return self.path_for(document_path).read_text(encoding="utf-8")

    def reserved_text(self) -> str:
        """Return the reserved control file, or ``""`` when the Wiki has none."""
        path = self.root / RESERVED_FILE
        return path.read_text(encoding="utf-8") if path.is_file() else ""

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
