"""Markdown formatting shared by all document writes."""

import mdformat


def format_markdown(text: str) -> str:
    """Format Markdown while preserving YAML Frontmatter and GFM syntax."""
    return mdformat.text(text, extensions={"gfm", "frontmatter"}).rstrip() + "\n"
