"""Pure tag normalization and hierarchy rules."""

type TagAliases = dict[str, tuple[str, ...]]


def is_valid_tag(tag: str) -> bool:
    """Accept Unicode alphanumerics and hyphens separated by hierarchy slashes."""
    parts = tag.split("/")
    return bool(parts) and all(
        part
        and not part.startswith("-")
        and not part.endswith("-")
        and all(character.isalnum() or character == "-" for character in part)
        for part in parts
    )


def alias_lookup(aliases: TagAliases) -> dict[str, str]:
    lookup: dict[str, str] = {}
    for canonical, values in aliases.items():
        lookup[canonical.casefold()] = canonical
        for alias in values:
            lookup[alias.casefold()] = canonical
    return lookup


def canonicalize_tag(tag: str, aliases: TagAliases) -> str:
    return alias_lookup(aliases).get(tag.casefold(), tag.casefold())


def tag_matches_filter(document_tag: str, requested_tag: str, aliases: TagAliases) -> bool:
    document = canonicalize_tag(document_tag, aliases)
    requested = canonicalize_tag(requested_tag, aliases)
    return document == requested or document.startswith(f"{requested}/")
