"""Domain value for one declared document-to-document relationship."""

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class GraphEdgeDraft:
    """One relationship extracted from a document, before it is projected.

    ``anchor`` carries the ``#fragment`` of a wikilink so related evidence can point at
    the section it came from.
    """

    target_path: str
    relation_type: str
    source_kind: str
    anchor: str | None = None
    source_section: str | None = None
    context: str | None = None
