"""Safe local Markdown and YAML Frontmatter storage."""

from contextlib import suppress
import fnmatch
import os
from pathlib import Path
import tempfile

from loguru import logger
import yaml

from agentwiki.domain.models import DirectoryEntry, Frontmatter, Note, NotePath
from agentwiki.markdown.formatting import format_markdown


class MarkdownStore:
    """Read and mutate Markdown files below one document root."""

    def __init__(self, root: Path) -> None:
        self.root = root.expanduser().resolve()
        self.root.mkdir(parents=True, exist_ok=True)

    def path_for(self, note_path: NotePath) -> Path:
        """Resolve a note path and enforce the document-root boundary."""
        candidate = (self.root / note_path.value).resolve()
        try:
            candidate.relative_to(self.root)
        except ValueError as exc:
            raise ValueError("note path escapes the document root") from exc
        return candidate

    def exists(self, note_path: NotePath) -> bool:
        return self.path_for(note_path).is_file()

    def read(self, note_path: NotePath) -> Note:
        path = self.path_for(note_path)
        if not path.is_file():
            raise FileNotFoundError(note_path.value)
        return self._parse(note_path, path.read_text(encoding="utf-8"))

    def raw(self, note_path: NotePath) -> str:
        path = self.path_for(note_path)
        if not path.is_file():
            raise FileNotFoundError(note_path.value)
        return path.read_text(encoding="utf-8")

    def write(self, note: Note, *, overwrite: bool = False) -> Note:
        path = self.path_for(note.path)
        if path.exists() and not overwrite:
            raise FileExistsError(note.path.value)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = self._serialize(note)
        self._atomic_write(path, payload)
        return note

    def delete(self, note_path: NotePath) -> None:
        path = self.path_for(note_path)
        if not path.is_file():
            raise FileNotFoundError(note_path.value)
        path.unlink()

    def delete_directory(self, directory: str) -> list[NotePath]:
        path = self._directory_path(directory)
        if path == self.root:
            raise ValueError("document root cannot be deleted as a directory")
        if not path.is_dir():
            raise FileNotFoundError(directory)
        deleted = [
            NotePath(value=file.relative_to(self.root).as_posix())
            for file in sorted(path.rglob("*.md"))
            if file.is_file() and file.resolve().is_relative_to(self.root)
        ]
        for file in sorted(path.rglob("*"), reverse=True):
            if file.is_file() or file.is_symlink():
                file.unlink()
            elif file.is_dir():
                file.rmdir()
        return deleted

    def move(self, source: NotePath, target: NotePath) -> None:
        source_path = self.path_for(source)
        target_path = self.path_for(target)
        if not source_path.is_file():
            raise FileNotFoundError(source.value)
        if target_path.exists():
            raise FileExistsError(target.value)
        target_path.parent.mkdir(parents=True, exist_ok=True)
        source_path.replace(target_path)

    def move_directory(self, source: str, target: str) -> None:
        source_path = self._directory_path(source)
        target_path = self._directory_path(target)
        if source_path == self.root:
            raise ValueError("document root cannot be moved as a directory")
        if not source_path.is_dir():
            raise FileNotFoundError(source)
        if target_path.exists():
            raise FileExistsError(target)
        if target_path.is_relative_to(source_path):
            raise ValueError("target directory cannot be inside the source directory")
        target_path.parent.mkdir(parents=True, exist_ok=True)
        source_path.replace(target_path)

    def iter_notes(self) -> list[Note]:
        """Read all Markdown documents in deterministic path order."""
        notes: list[Note] = []
        for path in sorted(self.root.rglob("*.md")):
            relative = path.relative_to(self.root).as_posix()
            try:
                resolved = path.resolve()
                resolved.relative_to(self.root)
                notes.append(
                    self._parse(NotePath(value=relative), resolved.read_text(encoding="utf-8"))
                )
            except (UnicodeDecodeError, ValueError, yaml.YAMLError, OSError) as exc:
                logger.warning("Skipping Markdown document {} during index scan: {}", relative, exc)
                continue
        return notes

    def list_directory(
        self,
        directory: str = "",
        *,
        depth: int = 1,
        file_name_glob: str | None = None,
    ) -> list[DirectoryEntry]:
        """List Markdown files and directories below a document directory."""
        if depth < 1:
            raise ValueError("directory depth must be at least 1")
        if depth > 10:
            raise ValueError("directory depth must be at most 10")

        base = self._directory_path(directory)
        if not base.is_dir():
            raise FileNotFoundError(directory or ".")

        entries: list[DirectoryEntry] = []
        for candidate in sorted(base.rglob("*")):
            relative = candidate.relative_to(base)
            if len(relative.parts) > depth:
                continue
            resolved = candidate.resolve()
            try:
                resolved.relative_to(self.root)
            except ValueError:
                continue
            if candidate.is_dir():
                kind = "directory"
            elif candidate.is_file() and candidate.suffix.lower() == ".md":
                if file_name_glob and not fnmatch.fnmatch(candidate.name, file_name_glob):
                    continue
                kind = "file"
            else:
                continue
            try:
                stat = resolved.stat()
            except OSError:
                continue
            entries.append(
                DirectoryEntry(
                    path=candidate.relative_to(self.root).as_posix(),
                    name=candidate.name,
                    kind=kind,
                    size=stat.st_size if kind == "file" else None,
                    modified_at=stat.st_mtime,
                )
            )
        return sorted(
            entries,
            key=lambda entry: (entry.kind != "directory", entry.path.casefold()),
        )

    @staticmethod
    def _parse(note_path: NotePath, raw: str) -> Note:
        return MarkdownStore.parse(note_path, raw)

    @staticmethod
    def parse(note_path: NotePath, raw: str) -> Note:
        """Parse Markdown text into a note, including an optional YAML header."""
        frontmatter: Frontmatter = {}
        content = raw
        if raw.startswith("---"):
            lines = raw.splitlines(keepends=True)
            if lines and lines[0].strip() == "---":
                end = next(
                    (i for i, line in enumerate(lines[1:], 1) if line.strip() == "---"),
                    None,
                )
                if end is not None:
                    parsed = yaml.safe_load("".join(lines[1:end]))
                    if parsed is not None and not isinstance(parsed, dict):
                        raise ValueError("Frontmatter must be a YAML mapping")
                    frontmatter = dict(parsed or {})
                    content = "".join(lines[end + 1 :]).lstrip("\n").rstrip("\n")
        return Note(path=note_path, content=content, frontmatter=frontmatter)

    @staticmethod
    def _serialize(note: Note) -> str:
        metadata = yaml.safe_dump(note.frontmatter, allow_unicode=True, sort_keys=False).strip()
        return format_markdown(f"---\n{metadata}\n---\n\n{note.content}")

    def _directory_path(self, directory: str) -> Path:
        relative = directory.replace("\\", "/").strip("/")
        if relative in {"", "."}:
            return self.root
        candidate = (self.root / relative).resolve()
        try:
            candidate.relative_to(self.root)
        except ValueError as exc:
            raise ValueError("directory escapes the document root") from exc
        return candidate

    @staticmethod
    def _atomic_write(path: Path, payload: str) -> None:
        fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            Path(temporary).replace(path)
        except OSError, UnicodeError:
            with suppress(OSError):
                Path(temporary).unlink()
            raise
