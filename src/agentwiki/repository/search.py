"""SQLite-backed document projection and retrieval candidates."""

import asyncio
from datetime import date, datetime
import json
import math
import operator
import re
import sqlite3
from typing import TypeGuard

from agentwiki.domain.documents import DocumentFingerprint, WikiDocument
from agentwiki.domain.retrieval import ContextQuery, IndexedChunk, SearchCandidate
from agentwiki.domain.tags import tag_matches_filter
from agentwiki.repository.embeddings import EmbeddingProvider
from agentwiki.repository.sqlite import SQLiteDatabase

_TOKEN_RE = re.compile(r"[\w-]+", re.UNICODE)


class SQLiteSearchRepository:
    def __init__(
        self,
        database: SQLiteDatabase,
        embedding_provider: EmbeddingProvider | None = None,
    ) -> None:
        self.database = database
        self.embedding_provider = embedding_provider

    @property
    def semantic_available(self) -> bool:
        return self.embedding_provider is not None

    async def initialize(self) -> None:
        await self._invalidate_changed_model()

    async def fingerprints(self) -> dict[str, DocumentFingerprint]:
        cursor = await self.database.connection.execute(
            "SELECT path, modified_at_ns, size FROM wiki_documents"
        )
        return {
            str(row["path"]): DocumentFingerprint(
                modified_at_ns=int(row["modified_at_ns"]),
                size=int(row["size"]),
            )
            for row in await cursor.fetchall()
        }

    async def replace_document(
        self,
        document: WikiDocument,
        chunks: tuple[IndexedChunk, ...],
    ) -> str | None:
        vectors: list[list[float]] | None = None
        vector_error: str | None = None
        if self.embedding_provider is not None and chunks:
            texts = [f"{document.title}\n{chunk.section}\n{chunk.content}" for chunk in chunks]
            try:
                vectors = await asyncio.to_thread(self.embedding_provider.embed_documents, texts)
                if len(vectors) != len(chunks):
                    raise ValueError("embedding provider returned an unexpected vector count")
                if any(
                    not vector or not all(math.isfinite(value) for value in vector)
                    for vector in vectors
                ):
                    raise ValueError("embedding provider returned an invalid vector")
            except (ImportError, OSError, RuntimeError, ValueError) as exc:
                vectors = None
                vector_error = f"semantic indexing unavailable for {document.path.value}: {exc}"
        connection = self.database.connection
        metadata = json.dumps(
            document.frontmatter,
            default=self._json_default,
            ensure_ascii=False,
            sort_keys=True,
        )
        try:
            await self.delete_paths((document.path.value,), commit=False)
            await connection.execute(
                """
                INSERT INTO wiki_documents(path, title, frontmatter_json, modified_at_ns, size)
                VALUES (?, ?, ?, ?, ?)
                """,
                (
                    document.path.value,
                    document.title,
                    metadata,
                    document.modified_at_ns,
                    document.size,
                ),
            )
            for position, chunk in enumerate(chunks):
                await connection.execute(
                    """INSERT INTO wiki_chunks(
                        chunk_id, path, ordinal, section, content, source_hash
                    ) VALUES (?, ?, ?, ?, ?, ?)""",
                    (
                        chunk.chunk_id,
                        document.path.value,
                        chunk.ordinal,
                        chunk.section,
                        chunk.content,
                        chunk.source_hash,
                    ),
                )
                await connection.execute(
                    """INSERT INTO wiki_chunks_fts(chunk_id, path, title, section, content)
                    VALUES (?, ?, ?, ?, ?)""",
                    (
                        chunk.chunk_id,
                        document.path.value,
                        document.title,
                        chunk.section,
                        chunk.content,
                    ),
                )
                if vectors is not None and self.embedding_provider is not None:
                    vector = vectors[position]
                    await connection.execute(
                        """INSERT INTO wiki_vectors(
                            chunk_id, path, model, vector_json, source_hash
                        ) VALUES (?, ?, ?, ?, ?)""",
                        (
                            chunk.chunk_id,
                            document.path.value,
                            self.embedding_provider.model_name,
                            json.dumps(vector),
                            chunk.source_hash,
                        ),
                    )
            await connection.commit()
        except BaseException:
            await connection.rollback()
            raise
        return vector_error

    async def delete_paths(self, paths: tuple[str, ...], *, commit: bool = True) -> None:
        connection = self.database.connection
        for path in paths:
            await connection.execute("DELETE FROM wiki_chunks_fts WHERE path = ?", (path,))
            await connection.execute("DELETE FROM wiki_vectors WHERE path = ?", (path,))
            await connection.execute("DELETE FROM wiki_documents WHERE path = ?", (path,))
        if commit:
            await connection.commit()

    async def clear(self) -> None:
        connection = self.database.connection
        await connection.execute("DELETE FROM wiki_chunks_fts")
        await connection.execute("DELETE FROM wiki_vectors")
        await connection.execute("DELETE FROM wiki_documents")
        await connection.commit()

    async def exact_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        if not query.query.strip():
            return []
        pattern = self._like_pattern(query.query.strip())
        rows = await self._rows(
            """
            SELECT c.chunk_id, c.path, d.title, c.section, c.content,
                   d.frontmatter_json, d.modified_at_ns
            FROM wiki_chunks AS c JOIN wiki_documents AS d ON d.path = c.path
            WHERE (d.path LIKE ? ESCAPE '\\' COLLATE NOCASE
                   OR d.title LIKE ? ESCAPE '\\' COLLATE NOCASE)
            ORDER BY d.modified_at_ns DESC, c.ordinal
            """,
            (pattern, pattern),
        )
        return self._filtered(rows, query, score=1.0)[:candidate_limit]

    async def keyword_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        tokens = _TOKEN_RE.findall(query.query)
        if not tokens:
            return []
        match = " OR ".join(f'"{token.replace(chr(34), chr(34) * 2)}"' for token in tokens)
        rows = await self._rows(
            """
            SELECT c.chunk_id, c.path, d.title, c.section, c.content,
                   d.frontmatter_json, d.modified_at_ns,
                   bm25(wiki_chunks_fts) AS source_score
            FROM wiki_chunks_fts
            JOIN wiki_chunks AS c ON c.chunk_id = wiki_chunks_fts.chunk_id
            JOIN wiki_documents AS d ON d.path = c.path
            WHERE wiki_chunks_fts MATCH ?
            ORDER BY source_score
            """,
            (match,),
        )
        return self._filtered(rows, query, score_from_row=True)[:candidate_limit]

    async def semantic_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        if self.embedding_provider is None or not query.query.strip():
            return []
        cursor = await self.database.connection.execute(
            """SELECT
                (SELECT COUNT(*) FROM wiki_chunks) AS chunk_count,
                (SELECT COUNT(*) FROM wiki_vectors WHERE model = ?) AS vector_count
            """,
            (self.embedding_provider.model_name,),
        )
        counts = await cursor.fetchone()
        if counts is not None and int(counts["vector_count"]) < int(counts["chunk_count"]):
            raise RuntimeError("semantic index is incomplete")
        query_vector = await asyncio.to_thread(self.embedding_provider.embed_query, query.query)
        rows = await self._rows(
            """
            SELECT c.chunk_id, c.path, d.title, c.section, c.content,
                   d.frontmatter_json, d.modified_at_ns, v.vector_json
            FROM wiki_vectors AS v
            JOIN wiki_chunks AS c ON c.chunk_id = v.chunk_id
            JOIN wiki_documents AS d ON d.path = c.path
            WHERE v.model = ?
            """,
            (self.embedding_provider.model_name,),
        )
        candidates = self._filtered(rows, query, score=0.0)
        by_id = {str(row["chunk_id"]): json.loads(row["vector_json"]) for row in rows}
        ranked = [
            candidate.model_copy(
                update={"score": self._cosine(query_vector, by_id[candidate.chunk_id])}
            )
            for candidate in candidates
        ]
        return sorted(ranked, key=lambda item: item.score, reverse=True)[:candidate_limit]

    async def recent_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        rows = await self._rows(
            """
            SELECT c.chunk_id, c.path, d.title, c.section, c.content,
                   d.frontmatter_json, d.modified_at_ns
            FROM wiki_documents AS d
            JOIN wiki_chunks AS c ON c.path = d.path
            WHERE c.ordinal = 0
            ORDER BY d.modified_at_ns DESC
            """,
            (),
        )
        return self._filtered(rows, query, score=0.0)[:candidate_limit]

    async def _rows(self, sql: str, parameters: tuple[object, ...]) -> list[sqlite3.Row]:
        cursor = await self.database.connection.execute(sql, parameters)
        return list(await cursor.fetchall())

    @classmethod
    def _filtered(
        cls,
        rows: list[sqlite3.Row],
        query: ContextQuery,
        *,
        score: float | None = None,
        score_from_row: bool = False,
    ) -> list[SearchCandidate]:
        candidates: list[SearchCandidate] = []
        scope = query.scope.strip("/")
        for row in rows:
            path = str(row["path"])
            if scope and path != scope and not path.startswith(f"{scope}/"):
                continue
            metadata = json.loads(row["frontmatter_json"])
            if not cls._metadata_matches_query(metadata, query):
                continue
            source_score = (
                -float(row["source_score"])
                if score_from_row
                else float(score or 0.0)
            )
            candidates.append(
                SearchCandidate(
                    chunk_id=str(row["chunk_id"]),
                    path={"value": path},
                    title=str(row["title"]),
                    section=str(row["section"]),
                    content=str(row["content"]),
                    frontmatter=metadata,
                    modified_at_ns=int(row["modified_at_ns"]),
                    score=source_score,
                )
            )
        return candidates

    @classmethod
    def _metadata_matches_query(cls, metadata: dict[str, object], query: ContextQuery) -> bool:
        note_type = str(metadata.get("type", metadata.get("note_type", ""))).casefold()
        if query.note_types and note_type not in {value.casefold() for value in query.note_types}:
            return False
        raw_tags = metadata.get("tags", [])
        note_tags = [raw_tags] if isinstance(raw_tags, str) else raw_tags
        if not isinstance(note_tags, list):
            note_tags = []
        if not all(
            any(
                tag_matches_filter(str(item), requested, query.tag_aliases)
                for item in note_tags
            )
            for requested in query.tags
        ):
            return False
        return all(
            cls._metadata_matches(metadata, key, expected)
            for key, expected in query.metadata_filters.items()
        )

    @staticmethod
    def _json_default(value: object) -> str:
        if isinstance(value, (date, datetime)):
            return value.isoformat()
        raise TypeError(f"Object of type {type(value).__name__} is not JSON serializable")

    @classmethod
    def _metadata_matches(
        cls, metadata: dict[str, object], key: str, expected: object
    ) -> bool:
        actual: object = metadata
        for part in key.split("."):
            if not isinstance(actual, dict) or part not in actual:
                return False
            actual = actual[part]
        if isinstance(expected, dict) and len(expected) == 1:
            operator_name, bound = next(iter(expected.items()))
            if operator_name in {"$gt", "$gte", "$lt", "$lte"}:
                if cls._is_number(actual) and cls._is_number(bound):
                    return cls._compare_numbers(operator_name, float(actual), float(bound))
                return cls._compare_text(operator_name, str(actual), str(bound))
            if operator_name == "$between" and isinstance(bound, list) and len(bound) == 2:
                if (
                    cls._is_number(actual)
                    and cls._is_number(bound[0])
                    and cls._is_number(bound[1])
                ):
                    return float(bound[0]) <= float(actual) <= float(bound[1])
                return str(bound[0]) <= str(actual) <= str(bound[1])
            return False
        if isinstance(actual, list) and isinstance(expected, list):
            return all(item in actual for item in expected)
        return actual == expected

    @staticmethod
    def _is_number(value: object) -> TypeGuard[int | float]:
        return isinstance(value, (int, float)) and not isinstance(value, bool)

    @staticmethod
    def _compare_numbers(operator_name: str, left: float, right: float) -> bool:
        if operator_name == "$gt":
            return left > right
        if operator_name == "$gte":
            return left >= right
        if operator_name == "$lt":
            return left < right
        return left <= right

    @staticmethod
    def _compare_text(operator_name: str, left: str, right: str) -> bool:
        if operator_name == "$gt":
            return left > right
        if operator_name == "$gte":
            return left >= right
        if operator_name == "$lt":
            return left < right
        return left <= right

    async def _invalidate_changed_model(self) -> None:
        connection = self.database.connection
        cursor = await connection.execute(
            "SELECT value FROM wiki_index_meta WHERE key = 'vector_model'"
        )
        row = await cursor.fetchone()
        configured = None if self.embedding_provider is None else self.embedding_provider.model_name
        stored = None if row is None else str(row["value"])
        if stored is not None and stored != configured:
            await connection.execute("DELETE FROM wiki_vectors")
        if configured is not None:
            await connection.execute(
                """INSERT INTO wiki_index_meta(key, value) VALUES ('vector_model', ?)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value""",
                (configured,),
            )
        await connection.commit()

    @staticmethod
    def _like_pattern(text: str) -> str:
        escaped = text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        return f"%{escaped}%"

    @staticmethod
    def _cosine(left: list[float], right: list[float]) -> float:
        if len(left) != len(right):
            return 0.0
        numerator = sum(a * b for a, b in zip(left, right, strict=True))
        left_norm = sum(value * value for value in left) ** 0.5
        right_norm = sum(value * value for value in right) ** 0.5
        return numerator / (left_norm * right_norm) if left_norm and right_norm else 0.0
