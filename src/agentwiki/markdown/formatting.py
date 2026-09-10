"""Markdown formatting used for non-mutating governance checks."""

import mdformat


def format_markdown(text: str) -> str:
    """Return canonical Markdown while preserving Frontmatter and GFM syntax."""
    return mdformat.text(text, extensions={"gfm", "frontmatter"}).rstrip() + "\n"
