"""Safe local Markdown and YAML Frontmatter storage."""

from __future__ import annotations

import os
from pathlib import Path
import tempfile
from typing import Any

import yaml

from agentwiki.domain.models import Note, NotePath


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

    def move(self, source: NotePath, target: NotePath) -> None:
        source_path = self.path_for(source)
        target_path = self.path_for(target)
        if not source_path.is_file():
            raise FileNotFoundError(source.value)
        if target_path.exists():
            raise FileExistsError(target.value)
        target_path.parent.mkdir(parents=True, exist_ok=True)
        source_path.replace(target_path)

    def iter_notes(self) -> list[Note]:
        """Read all Markdown documents in deterministic path order."""
        notes: list[Note] = []
        for path in sorted(self.root.rglob("*.md")):
            relative = path.relative_to(self.root).as_posix()
            try:
                notes.append(self._parse(NotePath(relative), path.read_text(encoding="utf-8")))
            except UnicodeDecodeError, ValueError, yaml.YAMLError:
                continue
        return notes

    @staticmethod
    def _parse(note_path: NotePath, raw: str) -> Note:
        frontmatter: dict[str, Any] = {}
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
                    content = "".join(lines[end + 1 :]).lstrip("\n")
        return Note(path=note_path, content=content, frontmatter=frontmatter)

    @staticmethod
    def _serialize(note: Note) -> str:
        metadata = yaml.safe_dump(note.frontmatter, allow_unicode=True, sort_keys=False).strip()
        return f"---\n{metadata}\n---\n\n{note.content}"

    @staticmethod
    def _atomic_write(path: Path, payload: str) -> None:
        fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            Path(temporary).replace(path)
        except Exception:
            Path(temporary).unlink(missing_ok=True)
            raise
