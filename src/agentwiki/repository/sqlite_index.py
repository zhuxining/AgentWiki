"""Local SQLite FTS5 index derived from Markdown documents."""

import asyncio
import contextlib
from hashlib import sha256
import json
import math
import operator
from pathlib import Path
import re
import sqlite3

import aiosqlite

from agentwiki.domain.models import Note, NotePath, SearchQuery, SearchResult
from agentwiki.repository.embeddings import EmbeddingProvider

_TOKEN_RE = re.compile(r"[\w-]+", re.UNICODE)


class SQLiteIndex:
    """Own the rebuildable local metadata and FTS5 search projection."""

    def __init__(self, path: Path, embedding_provider: EmbeddingProvider | None = None) -> None:
        self.path = path.expanduser()
        self.embedding_provider = embedding_provider
        self._sqlite_vec_loaded = False
        self._vector_dimensions: int | None = None
        self._connection: aiosqlite.Connection | None = None
        self.path.parent.mkdir(parents=True, exist_ok=True)

    @property
    def connection(self) -> aiosqlite.Connection:
        if self._connection is None:
            raise RuntimeError("SQLiteIndex must be initialized asynchronously")
        return self._connection

    async def initialize(self) -> None:
        """Open the async SQLite connection and initialize local projections."""
        if self._connection is not None:
            return
        self._connection = await aiosqlite.connect(self.path)
        try:
            self.connection.row_factory = sqlite3.Row
            await self.connection.execute("PRAGMA busy_timeout=5000")
            await self.connection.execute("PRAGMA synchronous=NORMAL")
            await self.connection.execute("PRAGMA journal_mode=WAL")
            await self.connection.execute("PRAGMA foreign_keys=ON")
            await self._initialize()
            if self.embedding_provider is not None:
                self._sqlite_vec_loaded = await self._load_sqlite_vec()
                await self._prepare_vector_store()
        except BaseException:
            await self.close()
            raise

    async def close(self) -> None:
        if self._connection is not None:
            await self._connection.close()
            self._connection = None

    async def _initialize(self) -> None:
        await self.connection.executescript(
            """
            CREATE TABLE IF NOT EXISTS document_index (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                updated_at REAL NOT NULL,
                content_hash TEXT NOT NULL DEFAULT ''
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS document_fts USING fts5(
                path UNINDEXED,
                title,
                content,
                frontmatter
            );
            CREATE TABLE IF NOT EXISTS document_vectors (
                path TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                vector_json TEXT NOT NULL,
                source_hash TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS index_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_document_index_updated_at
                ON document_index(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_document_index_title
                ON document_index(title COLLATE NOCASE);
            """
        )
        await self._ensure_column("document_index", "content_hash", "TEXT NOT NULL DEFAULT ''")
        await self._ensure_column("document_vectors", "source_hash", "TEXT NOT NULL DEFAULT ''")
        await self.connection.commit()

    async def _ensure_column(self, table: str, column: str, definition: str) -> None:
        columns = {
            row["name"]
            async for row in await self.connection.execute(f"PRAGMA table_info({table})")
        }
        if column not in columns:
            await self.connection.execute(f"ALTER TABLE {table} ADD COLUMN {column} {definition}")

    async def _load_sqlite_vec(self) -> bool:
        """Load sqlite-vec when the local SQLite build supports extensions."""
        try:
            import sqlite_vec

            await self.connection.enable_load_extension(True)
            await self.connection.load_extension(sqlite_vec.loadable_path())
            await self.connection.enable_load_extension(False)
            await self.connection.execute("SELECT vec_version()")
        except AttributeError, ImportError, sqlite3.Error, OSError:
            with contextlib.suppress(AttributeError, sqlite3.Error):
                await self.connection.enable_load_extension(False)
            return False
        return True

    async def _ensure_vector_table(self, dimensions: int) -> None:
        if not self._sqlite_vec_loaded:
            return
        if self._vector_dimensions is None:
            stored = await self._get_meta("vector_dimensions")
            self._vector_dimensions = int(stored) if stored is not None else None
        if self._vector_dimensions == dimensions and await self._vector_table_exists():
            return
        if self._vector_dimensions is not None or await self._vector_table_exists():
            await self.connection.execute("DROP TABLE IF EXISTS document_vectors_vec")
            await self.connection.execute("DELETE FROM document_vectors")
        await self.connection.execute(
            f"""
            CREATE VIRTUAL TABLE IF NOT EXISTS document_vectors_vec
            USING vec0(embedding float[{dimensions}])
            """
        )
        self._vector_dimensions = dimensions
        await self._set_meta("vector_dimensions", str(dimensions))
        if self.embedding_provider is not None:
            await self._set_meta("vector_model", self.embedding_provider.model_name)

    async def _vector_table_exists(self) -> bool:
        cursor = await self.connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'document_vectors_vec'"
        )
        row = await cursor.fetchone()
        return row is not None

    async def _prepare_vector_store(self) -> None:
        """Invalidate persisted vectors when the configured model changes."""
        if self.embedding_provider is None:
            return
        stored_model = await self._get_meta("vector_model")
        if stored_model is not None and stored_model != self.embedding_provider.model_name:
            await self.connection.execute("DELETE FROM document_vectors")
            if self._sqlite_vec_loaded and await self._vector_table_exists():
                await self.connection.execute("DROP TABLE document_vectors_vec")
            self._vector_dimensions = None
        else:
            stored_dimensions = await self._get_meta("vector_dimensions")
            self._vector_dimensions = int(stored_dimensions) if stored_dimensions else None
        await self._set_meta("vector_model", self.embedding_provider.model_name)
        await self.connection.commit()

    async def _get_meta(self, key: str) -> str | None:
        cursor = await self.connection.execute("SELECT value FROM index_meta WHERE key = ?", (key,))
        row = await cursor.fetchone()
        return None if row is None else str(row["value"])

    async def _set_meta(self, key: str, value: str) -> None:
        await self.connection.execute(
            "INSERT INTO index_meta(key, value) VALUES (?, ?) "
            "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (key, value),
        )

    async def upsert(self, note: Note, *, updated_at: float) -> None:
        path = note.path.value
        metadata = json.dumps(note.frontmatter, ensure_ascii=False, sort_keys=True)
        content_hash = self._content_hash(note, metadata)
        await self.connection.execute(
            """
                INSERT INTO document_index(
                    path, title, content, frontmatter_json, updated_at, content_hash
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(path) DO UPDATE SET
                    title=excluded.title,
                    content=excluded.content,
                    frontmatter_json=excluded.frontmatter_json,
                    updated_at=excluded.updated_at,
                    content_hash=excluded.content_hash
                """,
            (path, note.title, note.content, metadata, updated_at, content_hash),
        )
        await self.connection.execute("DELETE FROM document_fts WHERE path = ?", (path,))
        await self.connection.execute(
            "INSERT INTO document_fts(path, title, content, frontmatter) VALUES (?, ?, ?, ?)",
            (path, note.title, note.content, metadata),
        )
        await self._upsert_vector(note)
        await self.connection.commit()

    async def delete(self, note_path: NotePath) -> None:
        cursor = await self.connection.execute(
            "SELECT rowid FROM document_vectors WHERE path = ?",
            (note_path.value,),
        )
        vector = await cursor.fetchone()
        await self.connection.execute(
            "DELETE FROM document_index WHERE path = ?", (note_path.value,)
        )
        await self.connection.execute("DELETE FROM document_fts WHERE path = ?", (note_path.value,))
        await self.connection.execute(
            "DELETE FROM document_vectors WHERE path = ?", (note_path.value,)
        )
        if vector is not None and self._sqlite_vec_loaded and await self._vector_table_exists():
            await self.connection.execute(
                "DELETE FROM document_vectors_vec WHERE rowid = ?",
                (vector["rowid"],),
            )
        await self.connection.commit()

    async def move(self, source: NotePath, target: NotePath) -> None:
        cursor = await self.connection.execute(
            """
            SELECT title, content, frontmatter_json, updated_at, content_hash
            FROM document_index
            WHERE path = ?
            """,
            (source.value,),
        )
        row = await cursor.fetchone()
        if row is None:
            return
        await self.connection.execute("DELETE FROM document_index WHERE path = ?", (source.value,))
        await self.connection.execute("DELETE FROM document_fts WHERE path = ?", (source.value,))
        cursor = await self.connection.execute(
            """SELECT rowid, model, vector_json, source_hash
            FROM document_vectors WHERE path = ?""",
            (source.value,),
        )
        vector = await cursor.fetchone()
        await self.connection.execute(
            "DELETE FROM document_vectors WHERE path = ?", (source.value,)
        )
        if vector is not None and self._sqlite_vec_loaded and await self._vector_table_exists():
            await self.connection.execute(
                "DELETE FROM document_vectors_vec WHERE rowid = ?",
                (vector["rowid"],),
            )
        await self.connection.execute(
            """
                INSERT INTO document_index(
                    path, title, content, frontmatter_json, updated_at, content_hash
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
            (
                target.value,
                row["title"],
                row["content"],
                row["frontmatter_json"],
                row["updated_at"],
                row["content_hash"],
            ),
        )
        await self.connection.execute(
            "INSERT INTO document_fts(path, title, content, frontmatter) VALUES (?, ?, ?, ?)",
            (target.value, row["title"], row["content"], row["frontmatter_json"]),
        )
        if vector is not None:
            cursor = await self.connection.execute(
                """INSERT INTO document_vectors(path, model, vector_json, source_hash)
                VALUES (?, ?, ?, ?)""",
                (
                    target.value,
                    vector["model"],
                    vector["vector_json"],
                    vector["source_hash"],
                ),
            )
            if self._sqlite_vec_loaded:
                await self.connection.execute(
                    "INSERT INTO document_vectors_vec(rowid, embedding) VALUES (?, ?)",
                    (cursor.lastrowid, vector["vector_json"]),
                )
        await self.connection.commit()

    async def move_prefix(self, source: str, target: str) -> None:
        """Move all indexed paths below a directory after the files move."""
        prefix = source.strip("/") + "/"
        cursor = await self.connection.execute(
            "SELECT path FROM document_index WHERE path LIKE ? ORDER BY length(path)",
            (prefix + "%",),
        )
        rows = await cursor.fetchall()
        for row in rows:
            old = row["path"]
            new = target.strip("/") + "/" + old[len(prefix) :]
            await self.move(NotePath(value=old), NotePath(value=new))

    async def search(self, query: SearchQuery) -> list[SearchResult]:
        if query.mode in {"semantic", "vector"}:
            if not query.text.strip():
                raise ValueError("semantic search requires non-empty text")
            return self._paginate(await self._semantic_search(query), query)
        if not query.text.strip():
            cursor = await self.connection.execute(
                """
                SELECT path, title, frontmatter_json
                FROM document_index
                ORDER BY updated_at DESC
                LIMIT -1
                """,
                (),
            )
            rows = await cursor.fetchall()
            return self._paginate(
                self._filter_results(
                    [self._result(row, score=0.0, snippet="") for row in rows], query
                ),
                query,
            )

        if query.mode in {"title", "permalink"}:
            column = "title" if query.mode == "title" else "path"
            cursor = await self.connection.execute(
                f"""
                SELECT path, title, frontmatter_json
                FROM document_index
                WHERE {column} LIKE ? COLLATE NOCASE ESCAPE '\\'
                ORDER BY updated_at DESC
                """,
                (self._like_pattern(query.text),),
            )
            rows = await cursor.fetchall()
            return self._paginate(
                self._filter_results(
                    [self._result(row, score=1.0, snippet="") for row in rows], query
                ),
                query,
            )

        match = self._fts_query(query.text)
        cursor = await self.connection.execute(
            """
            SELECT d.path, d.title, d.frontmatter_json,
                   bm25(document_fts) AS score,
                   snippet(document_fts, 2, '[', ']', '…', 24) AS snippet
            FROM document_fts
            JOIN document_index AS d ON d.path = document_fts.path
            WHERE document_fts MATCH ?
            ORDER BY score
            LIMIT -1
            """,
            (match,),
        )
        rows = await cursor.fetchall()
        keyword_results = [
            self._result(row, score=-float(row["score"]), snippet=row["snippet"] or "")
            for row in rows
        ]
        keyword_results = self._filter_results(keyword_results, query)
        if query.mode == "hybrid" and self.embedding_provider is not None:
            return await self._hybrid_search(query, keyword_results)
        return self._paginate(keyword_results, query)

    async def rebuild(self, notes: list[Note], *, timestamp: float) -> None:
        await self.connection.execute("DELETE FROM document_index")
        await self.connection.execute("DELETE FROM document_fts")
        await self.connection.execute("DELETE FROM document_vectors")
        if self._sqlite_vec_loaded and await self._vector_table_exists():
            await self.connection.execute("DELETE FROM document_vectors_vec")
        for note in notes:
            metadata = json.dumps(note.frontmatter, ensure_ascii=False, sort_keys=True)
            await self.connection.execute(
                """
                INSERT INTO document_index(
                    path, title, content, frontmatter_json, updated_at, content_hash
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    note.path.value,
                    note.title,
                    note.content,
                    metadata,
                    timestamp,
                    self._content_hash(note, metadata),
                ),
            )
            await self.connection.execute(
                """
                INSERT INTO document_fts(path, title, content, frontmatter)
                VALUES (?, ?, ?, ?)
                """,
                (note.path.value, note.title, note.content, metadata),
            )
            await self._upsert_vector(note)
        await self.connection.commit()

    async def _upsert_vector(self, note: Note) -> None:
        if self.embedding_provider is None:
            return
        metadata = json.dumps(note.frontmatter, ensure_ascii=False, sort_keys=True)
        source_hash = self._content_hash(note, metadata)
        cursor = await self.connection.execute(
            "SELECT rowid, model, source_hash FROM document_vectors WHERE path = ?",
            (note.path.value,),
        )
        existing = await cursor.fetchone()
        if (
            existing is not None
            and existing["model"] == self.embedding_provider.model_name
            and existing["source_hash"] == source_hash
            and (not self._sqlite_vec_loaded or await self._vector_table_exists())
        ):
            return
        text = f"{note.title}\n{note.content}\n{metadata}"
        vectors = await asyncio.to_thread(
            self.embedding_provider.embed_documents,
            [text],
        )
        vector = vectors[0]
        if not vector or not all(math.isfinite(value) for value in vector):
            raise ValueError("embedding provider returned an invalid vector")
        await self._ensure_vector_table(len(vector))
        if existing is not None and self._sqlite_vec_loaded:
            await self.connection.execute(
                "DELETE FROM document_vectors_vec WHERE rowid = ?",
                (existing["rowid"],),
            )
        await self.connection.execute(
            "DELETE FROM document_vectors WHERE path = ?", (note.path.value,)
        )
        cursor = await self.connection.execute(
            """
            INSERT INTO document_vectors(path, model, vector_json, source_hash)
            VALUES (?, ?, ?, ?)
            """,
            (
                note.path.value,
                self.embedding_provider.model_name,
                json.dumps(vector),
                source_hash,
            ),
        )
        if self._sqlite_vec_loaded:
            await self.connection.execute(
                "INSERT INTO document_vectors_vec(rowid, embedding) VALUES (?, ?)",
                (cursor.lastrowid, json.dumps(vector)),
            )

    async def _semantic_search(self, query: SearchQuery) -> list[SearchResult]:
        if self.embedding_provider is None:
            raise RuntimeError("semantic search requires a configured embedding provider")
        query_vector = await asyncio.to_thread(
            self.embedding_provider.embed_query,
            query.text,
        )
        if self._sqlite_vec_loaded and self._vector_dimensions == len(query_vector):
            cursor = await self.connection.execute(
                """
                SELECT rowid, distance
                FROM document_vectors_vec
                WHERE embedding MATCH ? AND k = ?
                """,
                (json.dumps(query_vector), 100),
            )
            rows = await cursor.fetchall()
            results: list[SearchResult] = []
            for row in rows:
                cursor = await self.connection.execute(
                    """
                    SELECT d.path, d.title, d.frontmatter_json
                    FROM document_vectors AS v
                    JOIN document_index AS d ON d.path = v.path
                    WHERE v.rowid = ?
                    """,
                    (row["rowid"],),
                )
                document = await cursor.fetchone()
                if document is not None:
                    distance = float(row["distance"])
                    results.append(
                        self._result(
                            document,
                            score=max(0.0, 1.0 - (distance**2) / 2),
                            snippet="",
                        )
                    )
            return self._filter_results(results, query)
        cursor = await self.connection.execute(
            """
            SELECT d.path, d.title, d.frontmatter_json, v.vector_json
            FROM document_index AS d
            JOIN document_vectors AS v ON v.path = d.path
            """
        )
        rows = await cursor.fetchall()
        ranked = sorted(
            ((self._cosine(query_vector, json.loads(row["vector_json"])), row) for row in rows),
            key=operator.itemgetter(0),
            reverse=True,
        )[: query.limit]
        return self._filter_results(
            [self._result(row, score=score, snippet="") for score, row in ranked], query
        )

    async def _hybrid_search(
        self,
        query: SearchQuery,
        keyword_results: list[SearchResult],
    ) -> list[SearchResult]:
        semantic_results = await self._semantic_search(query)
        by_path = {result.path.value: result for result in keyword_results}
        for result in semantic_results:
            existing = by_path.get(result.path.value)
            if existing is None:
                by_path[result.path.value] = result
            else:
                by_path[result.path.value] = SearchResult(
                    path=result.path,
                    title=result.title,
                    score=existing.score + result.score,
                    frontmatter=result.frontmatter,
                    snippet=existing.snippet,
                )
        return self._paginate(
            sorted(
                by_path.values(),
                key=lambda result: result.score,
                reverse=True,
            ),
            query,
        )

    @staticmethod
    def _filter_results(results: list[SearchResult], query: SearchQuery) -> list[SearchResult]:
        def matches(result: SearchResult) -> bool:
            metadata = result.frontmatter
            note_type = str(metadata.get("type", metadata.get("note_type", ""))).casefold()
            if query.note_types and note_type not in {
                value.casefold() for value in query.note_types
            }:
                return False
            if query.tags:
                raw_tags = metadata.get("tags", [])
                note_tags = [raw_tags] if isinstance(raw_tags, str) else raw_tags
                if not isinstance(note_tags, list):
                    return False
                normalized_tags = {str(item).casefold() for item in note_tags}
                if not all(tag.casefold() in normalized_tags for tag in query.tags):
                    return False
            return all(
                SQLiteIndex._metadata_matches(metadata, key, expected)
                for key, expected in query.metadata_filters.items()
            )

        return [result for result in results if matches(result)]

    @staticmethod
    def _metadata_matches(metadata: dict[str, object], key: str, expected: object) -> bool:
        """Match scalar, nested, collection, and basic comparison filters."""
        actual: object = metadata
        for part in key.split("."):
            if not isinstance(actual, dict) or part not in actual:
                actual = None
                break
            actual = actual[part]
        if isinstance(expected, dict):
            if len(expected) != 1:
                return False
            operator_name, bound = next(iter(expected.items()))
            if actual is None and operator_name != "$in":
                return False
            if operator_name == "$in":
                return isinstance(bound, list) and actual in bound
            if operator_name in {"$gt", "$gte", "$lt", "$lte"}:
                if (
                    isinstance(actual, (int, float))
                    and not isinstance(actual, bool)
                    and isinstance(bound, (int, float))
                    and not isinstance(bound, bool)
                ):
                    left, right = float(actual), float(bound)
                    return {
                        "$gt": left > right,
                        "$gte": left >= right,
                        "$lt": left < right,
                        "$lte": left <= right,
                    }[operator_name]
                else:
                    left, right = str(actual), str(bound)
                    return {
                        "$gt": left > right,
                        "$gte": left >= right,
                        "$lt": left < right,
                        "$lte": left <= right,
                    }[operator_name]
            if operator_name == "$between" and isinstance(bound, list) and len(bound) == 2:
                numeric = (
                    isinstance(actual, (int, float))
                    and not isinstance(actual, bool)
                    and all(isinstance(item, (int, float)) for item in bound)
                    and not any(isinstance(item, bool) for item in bound)
                )
                if numeric:
                    value = float(actual)
                    lower, upper = float(bound[0]), float(bound[1])
                    return lower <= value <= upper
                else:
                    value = str(actual)
                    lower, upper = str(bound[0]), str(bound[1])
                    return lower <= value <= upper
            return False
        if isinstance(actual, list) and isinstance(expected, list):
            return all(item in actual for item in expected)
        return actual == expected

    @staticmethod
    def _paginate(results: list[SearchResult], query: SearchQuery) -> list[SearchResult]:
        start = (query.page - 1) * query.limit
        return results[start : start + query.limit]

    @staticmethod
    def _cosine(left: list[float], right: list[float]) -> float:
        if len(left) != len(right):
            raise ValueError("embedding dimensions do not match")
        numerator = sum(a * b for a, b in zip(left, right, strict=True))
        left_norm = sum(value * value for value in left) ** 0.5
        right_norm = sum(value * value for value in right) ** 0.5
        if not left_norm or not right_norm:
            return 0.0
        return numerator / (left_norm * right_norm)

    @staticmethod
    def _fts_query(text: str) -> str:
        tokens = _TOKEN_RE.findall(text)
        if not tokens:
            raise ValueError("search text must contain searchable characters")
        return " AND ".join(f'"{token.replace(chr(34), chr(34) * 2)}"' for token in tokens)

    @staticmethod
    def _like_pattern(text: str) -> str:
        escaped = text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        return f"%{escaped}%"

    @staticmethod
    def _result(row: sqlite3.Row, *, score: float, snippet: str) -> SearchResult:
        return SearchResult(
            path=NotePath(value=row["path"]),
            title=row["title"],
            score=score,
            frontmatter=json.loads(row["frontmatter_json"]),
            snippet=snippet,
        )

    @staticmethod
    def _content_hash(note: Note, metadata: str) -> str:
        payload = f"{note.title}\n{note.content}\n{metadata}".encode()
        return sha256(payload).hexdigest()
