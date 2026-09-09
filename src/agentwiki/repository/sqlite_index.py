"""Local SQLite FTS5 index derived from Markdown documents."""

from __future__ import annotations

import contextlib
import json
import operator
from pathlib import Path
import re
import sqlite3

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
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.connection = sqlite3.connect(self.path)
        self.connection.row_factory = sqlite3.Row
        self.connection.execute("PRAGMA journal_mode=WAL")
        self.connection.execute("PRAGMA foreign_keys=ON")
        self._initialize()
        if self.embedding_provider is not None:
            self._sqlite_vec_loaded = self._load_sqlite_vec()

    def close(self) -> None:
        self.connection.close()

    def _initialize(self) -> None:
        self.connection.executescript(
            """
            CREATE TABLE IF NOT EXISTS document_index (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                updated_at REAL NOT NULL
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
                vector_json TEXT NOT NULL
            );
            """
        )
        self.connection.commit()

    def _load_sqlite_vec(self) -> bool:
        """Load sqlite-vec when the local SQLite build supports extensions."""
        try:
            import sqlite_vec

            self.connection.enable_load_extension(True)
            self.connection.load_extension(sqlite_vec.loadable_path())
            self.connection.enable_load_extension(False)
            self.connection.execute("SELECT vec_version()")
        except AttributeError, ImportError, sqlite3.Error, OSError:
            with contextlib.suppress(AttributeError, sqlite3.Error):
                self.connection.enable_load_extension(False)
            return False
        return True

    def _ensure_vector_table(self, dimensions: int) -> None:
        if not self._sqlite_vec_loaded:
            return
        if self._vector_dimensions == dimensions:
            return
        if self._vector_dimensions is not None:
            self.connection.execute("DROP TABLE IF EXISTS document_vectors_vec")
        self.connection.execute(
            f"""
            CREATE VIRTUAL TABLE IF NOT EXISTS document_vectors_vec
            USING vec0(embedding float[{dimensions}])
            """
        )
        self._vector_dimensions = dimensions

    def _vector_table_exists(self) -> bool:
        row = self.connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'document_vectors_vec'"
        ).fetchone()
        return row is not None

    def upsert(self, note: Note, *, updated_at: float) -> None:
        path = note.path.value
        metadata = json.dumps(note.frontmatter, ensure_ascii=False, sort_keys=True)
        with self.connection:
            self.connection.execute(
                """
                INSERT INTO document_index(path, title, content, frontmatter_json, updated_at)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(path) DO UPDATE SET
                    title=excluded.title,
                    content=excluded.content,
                    frontmatter_json=excluded.frontmatter_json,
                    updated_at=excluded.updated_at
                """,
                (path, note.title, note.content, metadata, updated_at),
            )
            self.connection.execute("DELETE FROM document_fts WHERE path = ?", (path,))
            self.connection.execute(
                "INSERT INTO document_fts(path, title, content, frontmatter) VALUES (?, ?, ?, ?)",
                (path, note.title, note.content, metadata),
            )
            self._upsert_vector(note)

    def delete(self, note_path: NotePath) -> None:
        vector = self.connection.execute(
            "SELECT rowid FROM document_vectors WHERE path = ?",
            (note_path.value,),
        ).fetchone()
        with self.connection:
            self.connection.execute("DELETE FROM document_index WHERE path = ?", (note_path.value,))
            self.connection.execute("DELETE FROM document_fts WHERE path = ?", (note_path.value,))
            self.connection.execute(
                "DELETE FROM document_vectors WHERE path = ?",
                (note_path.value,),
            )
            if vector is not None and self._sqlite_vec_loaded and self._vector_table_exists():
                self.connection.execute(
                    "DELETE FROM document_vectors_vec WHERE rowid = ?",
                    (vector["rowid"],),
                )

    def move(self, source: NotePath, target: NotePath) -> None:
        row = self.connection.execute(
            """
            SELECT title, content, frontmatter_json, updated_at
            FROM document_index
            WHERE path = ?
            """,
            (source.value,),
        ).fetchone()
        if row is None:
            return
        with self.connection:
            self.connection.execute("DELETE FROM document_index WHERE path = ?", (source.value,))
            self.connection.execute("DELETE FROM document_fts WHERE path = ?", (source.value,))
            vector = self.connection.execute(
                "SELECT rowid, model, vector_json FROM document_vectors WHERE path = ?",
                (source.value,),
            ).fetchone()
            self.connection.execute("DELETE FROM document_vectors WHERE path = ?", (source.value,))
            if vector is not None and self._sqlite_vec_loaded and self._vector_table_exists():
                self.connection.execute(
                    "DELETE FROM document_vectors_vec WHERE rowid = ?",
                    (vector["rowid"],),
                )
            self.connection.execute(
                """
                INSERT INTO document_index(path, title, content, frontmatter_json, updated_at)
                VALUES (?, ?, ?, ?, ?)
                """,
                (
                    target.value,
                    row["title"],
                    row["content"],
                    row["frontmatter_json"],
                    row["updated_at"],
                ),
            )
            self.connection.execute(
                "INSERT INTO document_fts(path, title, content, frontmatter) VALUES (?, ?, ?, ?)",
                (target.value, row["title"], row["content"], row["frontmatter_json"]),
            )
            if vector is not None:
                cursor = self.connection.execute(
                    "INSERT INTO document_vectors(path, model, vector_json) VALUES (?, ?, ?)",
                    (target.value, vector["model"], vector["vector_json"]),
                )
                if self._sqlite_vec_loaded:
                    self.connection.execute(
                        "INSERT INTO document_vectors_vec(rowid, embedding) VALUES (?, ?)",
                        (cursor.lastrowid, vector["vector_json"]),
                    )

    def search(self, query: SearchQuery) -> list[SearchResult]:
        if query.mode == "semantic":
            return self._semantic_search(query)
        if not query.text.strip():
            rows = self.connection.execute(
                """
                SELECT path, title, frontmatter_json
                FROM document_index
                ORDER BY updated_at DESC
                LIMIT ?
                """,
                (query.limit,),
            ).fetchall()
            return [self._result(row, score=0.0, snippet="") for row in rows]

        match = self._fts_query(query.text)
        rows = self.connection.execute(
            """
            SELECT d.path, d.title, d.frontmatter_json,
                   bm25(document_fts) AS score,
                   snippet(document_fts, 2, '[', ']', '…', 24) AS snippet
            FROM document_fts
            JOIN document_index AS d ON d.path = document_fts.path
            WHERE document_fts MATCH ?
            ORDER BY score
            LIMIT ?
            """,
            (match, query.limit),
        ).fetchall()
        keyword_results = [
            self._result(row, score=-float(row["score"]), snippet=row["snippet"] or "")
            for row in rows
        ]
        if query.mode == "hybrid" and self.embedding_provider is not None:
            return self._hybrid_search(query, keyword_results)
        return keyword_results

    def rebuild(self, notes: list[Note], *, timestamp: float) -> None:
        with self.connection:
            self.connection.execute("DELETE FROM document_index")
            self.connection.execute("DELETE FROM document_fts")
            self.connection.execute("DELETE FROM document_vectors")
            if self._sqlite_vec_loaded and self._vector_table_exists():
                self.connection.execute("DELETE FROM document_vectors_vec")
            for note in notes:
                metadata = json.dumps(note.frontmatter, ensure_ascii=False, sort_keys=True)
                self.connection.execute(
                    """
                    INSERT INTO document_index(path, title, content, frontmatter_json, updated_at)
                    VALUES (?, ?, ?, ?, ?)
                    """,
                    (note.path.value, note.title, note.content, metadata, timestamp),
                )
                self.connection.execute(
                    """
                    INSERT INTO document_fts(path, title, content, frontmatter)
                    VALUES (?, ?, ?, ?)
                    """,
                    (note.path.value, note.title, note.content, metadata),
                )
                self._upsert_vector(note)

    def _upsert_vector(self, note: Note) -> None:
        if self.embedding_provider is None:
            return
        text = f"{note.title}\n{note.content}\n{json.dumps(note.frontmatter, ensure_ascii=False)}"
        vector = self.embedding_provider.embed_documents([text])[0]
        self._ensure_vector_table(len(vector))
        existing = self.connection.execute(
            "SELECT rowid FROM document_vectors WHERE path = ?",
            (note.path.value,),
        ).fetchone()
        if existing is not None and self._sqlite_vec_loaded:
            self.connection.execute(
                "DELETE FROM document_vectors_vec WHERE rowid = ?",
                (existing["rowid"],),
            )
        self.connection.execute("DELETE FROM document_vectors WHERE path = ?", (note.path.value,))
        cursor = self.connection.execute(
            """
            INSERT INTO document_vectors(path, model, vector_json)
            VALUES (?, ?, ?)
            """,
            (note.path.value, self.embedding_provider.model_name, json.dumps(vector)),
        )
        if self._sqlite_vec_loaded:
            self.connection.execute(
                "INSERT INTO document_vectors_vec(rowid, embedding) VALUES (?, ?)",
                (cursor.lastrowid, json.dumps(vector)),
            )

    def _semantic_search(self, query: SearchQuery) -> list[SearchResult]:
        if self.embedding_provider is None:
            raise RuntimeError("semantic search requires a configured embedding provider")
        query_vector = self.embedding_provider.embed_query(query.text)
        if self._sqlite_vec_loaded and self._vector_dimensions == len(query_vector):
            rows = self.connection.execute(
                """
                SELECT rowid, distance
                FROM document_vectors_vec
                WHERE embedding MATCH ? AND k = ?
                """,
                (json.dumps(query_vector), query.limit),
            ).fetchall()
            results: list[SearchResult] = []
            for row in rows:
                document = self.connection.execute(
                    """
                    SELECT d.path, d.title, d.frontmatter_json
                    FROM document_vectors AS v
                    JOIN document_index AS d ON d.path = v.path
                    WHERE v.rowid = ?
                    """,
                    (row["rowid"],),
                ).fetchone()
                if document is not None:
                    distance = float(row["distance"])
                    results.append(
                        self._result(document, score=max(0.0, 1.0 - (distance**2) / 2), snippet="")
                    )
            return results
        rows = self.connection.execute(
            """
            SELECT d.path, d.title, d.frontmatter_json, v.vector_json
            FROM document_index AS d
            JOIN document_vectors AS v ON v.path = d.path
            """
        ).fetchall()
        ranked = sorted(
            ((self._cosine(query_vector, json.loads(row["vector_json"])), row) for row in rows),
            key=operator.itemgetter(0),
            reverse=True,
        )[: query.limit]
        return [self._result(row, score=score, snippet="") for score, row in ranked]

    def _hybrid_search(
        self,
        query: SearchQuery,
        keyword_results: list[SearchResult],
    ) -> list[SearchResult]:
        semantic_results = self._semantic_search(query)
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
        return sorted(
            by_path.values(),
            key=lambda result: result.score,
            reverse=True,
        )[: query.limit]

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
    def _result(row: sqlite3.Row, *, score: float, snippet: str) -> SearchResult:
        return SearchResult(
            path=NotePath(row["path"]),
            title=row["title"],
            score=score,
            frontmatter=json.loads(row["frontmatter_json"]),
            snippet=snippet,
        )
