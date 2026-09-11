"""Normalization for Wiki-relative directory scopes."""

from pathlib import PurePosixPath


def normalize_scope(value: str) -> str:
    """Return a normalized Wiki-relative scope, rejecting anything outside the root."""
    if value.startswith(("/", "\\")):
        raise ValueError("scope must stay inside the Wiki root")
    normalized = value.replace("\\", "/").strip("/")
    if not normalized or normalized == ".":
        return ""
    path = PurePosixPath(normalized)
    if path.is_absolute() or ".." in path.parts:
        raise ValueError("scope must stay inside the Wiki root")
    return path.as_posix()
