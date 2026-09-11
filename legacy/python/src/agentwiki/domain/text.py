"""CJK and script-aware text analysis for the keyword projection.

SQLite FTS5's default ``unicode61`` tokenizer treats a run of CJK characters as a
single token, so any query shorter than that run fails to match. AgentWiki indexes
script segments as their characters plus overlapping bigrams, and renders queries as
ordered FTS5 phrases instead, which restores substring-level recall without adding a
segmentation dependency.

The character ranges follow the "no spaces between words" scripts: Han (including the
common extensions), Kana, Hangul, Bopomofo, and the Southeast Asian scripts that are
written without word separators.
"""

from collections.abc import Iterable
import re
import unicodedata

# (start, end) inclusive code point ranges whose characters are written without word
# separators. Mirrors the ranges basic-memory ships in its script n-gram channel.
_SCRIPT_RANGES: tuple[tuple[int, int], ...] = (
    (0x0E00, 0x0E7F),  # Thai
    (0x0E80, 0x0EFF),  # Lao
    (0x0F00, 0x0FFF),  # Tibetan
    (0x1000, 0x109F),  # Myanmar
    (0x1780, 0x17FF),  # Khmer
    (0x1100, 0x11FF),  # Hangul Jamo
    (0x2E80, 0x2EFF),  # CJK radicals
    (0x3040, 0x309F),  # Hiragana
    (0x30A0, 0x30FF),  # Katakana
    (0x3100, 0x312F),  # Bopomofo
    (0x3130, 0x318F),  # Hangul compatibility Jamo
    (0x31A0, 0x31BF),  # Bopomofo extended
    (0x31F0, 0x31FF),  # Katakana phonetic extensions
    (0x3400, 0x4DBF),  # CJK unified ideographs extension A
    (0x4E00, 0x9FFF),  # CJK unified ideographs
    (0xA960, 0xA97F),  # Hangul Jamo extended A
    (0xAC00, 0xD7AF),  # Hangul syllables
    (0xD7B0, 0xD7FF),  # Hangul Jamo extended B
    (0xF900, 0xFAFF),  # CJK compatibility ideographs
    (0x20000, 0x2A6DF),  # CJK extension B
    (0x2A700, 0x2EBEF),  # CJK extensions C-F
    (0x2F800, 0x2FA1F),  # CJK compatibility ideographs supplement
    (0x30000, 0x323AF),  # CJK extension G-H
)
_COMBINING = frozenset({"Mn", "Mc", "Me"})
# Zero-width joiners must not break a run but must not be indexed either.
_ZERO_WIDTH = frozenset({"\u200c", "\u200d"})
_TOKEN_RE = re.compile(r"[\w-]+", re.UNICODE)
_latin_fallback = object()


def _is_script(char: str) -> bool:
    code = ord(char)
    return any(start <= code <= end for start, end in _SCRIPT_RANGES)


def _is_combining(char: str) -> bool:
    return unicodedata.category(char) in _COMBINING


def script_runs(text: str) -> tuple[str, ...]:
    """Split text into runs of characters written without word separators."""
    normalized = unicodedata.normalize("NFKC", text)
    runs: list[str] = []
    current: list[str] = []
    for char in normalized:
        if char in _ZERO_WIDTH:
            continue
        if _is_script(char) or (_is_combining(char) and current):
            current.append(char)
            continue
        if current:
            runs.append("".join(current))
            current = []
    if current:
        runs.append("".join(current))
    return tuple(runs)


def run_grams(run: str) -> tuple[str, ...]:
    """Return the bigram form of one run."""
    if len(run) < 2:
        return (run,)
    return tuple(run[index : index + 2] for index in range(len(run) - 1))


def analyze(text: str) -> tuple[str, str, str]:
    """Return ``(characters, bigrams, words)`` for the three FTS projection columns.

    The script streams are kept in separate columns: interleaving them would let a
    bigram token split a character phrase, which breaks adjacency matching for longer
    runs. ``words`` carries the non-script tokens (latin words, identifiers) that the
    script columns deliberately drop.
    """
    normalized = unicodedata.normalize("NFKC", text)
    characters: list[str] = []
    bigrams: list[str] = []
    position = 0
    for run in script_runs(text):
        start = normalized.find(run, position)
        if start < 0:
            continue
        characters.extend(run)
        bigrams.extend(run_grams(run))
        position = start + len(run)
    words = [token for token in _TOKEN_RE.findall(normalized) if not _is_script(token[0])]
    return " ".join(characters), " ".join(bigrams), " ".join(words)


def query_terms(query: str) -> tuple[str, ...]:
    """Return the word tokens that must all be present for an exact keyword match."""
    terms = _TOKEN_RE.findall(query)
    if terms:
        return tuple(terms)
    stripped = query.strip()
    return (stripped,) if stripped else ()


def simple_tokens(query: str) -> tuple[str, ...]:
    """Return plain word tokens, used where bigram expansion is not needed."""
    return tuple(_TOKEN_RE.findall(query))


def _phrase(tokens: Iterable[str]) -> str:
    return '"' + " ".join(tokens) + '"'


def _word_clause(token: str) -> str:
    """One FTS5 condition for a non-script token, scoped to the word column."""
    escaped = token.replace('"', '""')
    return f'search_words: "{escaped}"'


def _run_clause(run: str) -> str:
    """One FTS5 condition group covering a whole script run.

    The character phrase pins the exact characters and their order; the bigram phrase
    pins adjacency inside the run. Together they make the candidate set exact, so no
    Python post-filter is needed.
    """
    characters = _phrase(run)
    if len(run) < 2:
        return f"search_chars: {characters}"
    return f"(search_chars: {characters} AND search_bigrams: {_phrase(run_grams(run))})"


def query_expression(query: str) -> str:
    """Build an FTS5 MATCH expression for ``query``.

    Every part of the query is AND-ed: script runs must appear verbatim, word tokens
    must be present. ``*`` prefix matching keeps latin word fragments working.
    """
    normalized = unicodedata.normalize("NFKC", query)
    runs = script_runs(query)
    parts: list[str] = []
    position = 0
    for run in runs:
        start = normalized.find(run, position)
        if start < 0:
            continue
        parts.extend(_word_clause(token) for token in query_terms(normalized[position:start]))
        parts.append(_run_clause(run))
        position = start + len(run)
    parts.extend(_word_clause(token) for token in query_terms(normalized[position:]))
    if not runs:
        parts = [_word_clause(token) for token in query_terms(normalized)]
    return " AND ".join(parts)


def relaxed_query_expression(query: str) -> str:
    """Build a deliberately loose OR expression for a strict query that matched nothing.

    Multi-word natural-language questions rarely have every token in one document; without
    a second, looser attempt the keyword leg contributes nothing and hybrid retrieval
    silently degrades to whatever other sources return. Ranking (bm25) still prefers
    documents that carry more of the query, so a loose recall pass is safe.
    """
    normalized = unicodedata.normalize("NFKC", query)
    parts: list[str] = []
    for run in script_runs(query):
        parts.extend(run_grams(run))
    parts.extend(query_terms(normalized))
    seen: list[str] = []
    for part in parts:
        if part and part not in seen:
            seen.append(part)
    if not seen:
        return ""
    return " OR ".join(_phrase((token,)) for token in seen)
