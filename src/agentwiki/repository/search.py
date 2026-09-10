"""SQLite-backed document projection and retrieval candidates."""

import asyncio
from datetime import date, datetime
import json
import math
import sqlite3
from time import time_ns
from typing import Literal, TypeGuard, cast
from uuid import uuid4

from agentwiki.domain.documents import DocumentFingerprint, WikiDocument
from agentwiki.domain.retrieval import (
    ContextQuery,
    IndexedChunk,
    RelatedDocument,
    SearchCandidate,
)
from agentwiki.domain.text import (
    analyze,
    query_expression,
    query_terms,
    relaxed_query_expression,
    simple_tokens,
)
from agentwiki.indexing.graph import GraphEdgeDraft
from agentwiki.repository.embeddings import EmbeddingProvider
from agentwiki.repository.sqlite import SQLiteDatabase

VectorSyncState = Literal["ready", "pending", "error", "unavailable", "none"]


class SQLiteSearchRepository:
    def __init__(
        self,
        database: SQLiteDatabase,
        embedding_provider: EmbeddingProvider | None = None,
    ) -> None:
        self.database = database
        self.embedding_provider = embedding_provider
        self._vector_tasks: set[asyncio.Task[None]] = set()
        self._vector_tasks_by_document: dict[str, asyncio.Task[None]] = {}
        self._write_lock = asyncio.Lock()
        self._task_lock = asyncio.Lock()
        self._vector_closed = False
        self._initial_vector_sync_required = False

    @property
    def semantic_available(self) -> bool:
        return (
            self.embedding_provider is not None
            and self.database.sqlite_vec_available
            and self.database.vector_dimensions is not None
        )

    async def initialize(self) -> None:
        await self._invalidate_changed_model()
        await self._recover_pending_vectors()
        self._initial_vector_sync_required = (
            self.embedding_provider is not None
            and self.database.sqlite_vec_available
            and self.database.vector_dimensions is None
        )

    async def _recover_pending_vectors(self) -> None:
        """Make pending work from a previous process eligible for retry.

        Reads the updated row count first so a startup with nothing to recover does not
        open a write transaction.
        """
        cursor = await self.database.connection.execute(
            """UPDATE wiki_vector_manifest
            SET status = 'error',
                error = COALESCE(error, 'vector sync was interrupted'),
                updated_at_ns = ?
            WHERE status = 'pending'""",
            (time_ns(),),
        )
        if cursor.rowcount:
            # Recovered work is outstanding; force the next sync past the fast path.
            await self.database.connection.execute(
                "DELETE FROM wiki_index_meta WHERE key = 'index_generation'"
            )
            await self.database.connection.commit()
        else:
            await self.database.connection.rollback()

    async def vector_stale_paths(self) -> set[str]:
        """Return documents whose current chunks lack ready vectors."""
        if self.embedding_provider is None or not self.database.sqlite_vec_available:
            return set()
        connection = self.database.connection
        if self.database.vector_dimensions is None:
            cursor = await connection.execute(
                """SELECT DISTINCT d.path FROM wiki_documents AS d
                JOIN wiki_chunks AS c ON c.path = d.path
                WHERE d.content_hash != ''
                  AND NOT EXISTS (
                      SELECT 1 FROM wiki_vector_manifest AS v
                      WHERE v.document_id = d.document_id AND v.status = 'pending'
                  )"""
            )
            return {str(row[0]) for row in await cursor.fetchall()}
        cursor = await connection.execute(
            """
            SELECT DISTINCT d.path
            FROM wiki_documents AS d
            JOIN wiki_chunks AS c ON c.path = d.path
            WHERE NOT EXISTS (
                SELECT 1 FROM wiki_vector_manifest AS v
                WHERE v.chunk_id = c.chunk_id
                  AND v.model = ?
                  AND v.status = 'ready'
                  AND v.source_hash = c.embedding_hash
            )
              AND NOT EXISTS (
                  SELECT 1 FROM wiki_vector_manifest AS pending
                  WHERE pending.document_id = d.document_id AND pending.status = 'pending'
              )
            """,
            (self.embedding_provider.model_name,),
        )
        return {str(row[0]) for row in await cursor.fetchall()}

    async def vector_state(self, path: str) -> VectorSyncState:
        """Return the persisted semantic-index state for one document."""
        if self.embedding_provider is None or not self.database.sqlite_vec_available:
            return "unavailable"
        cursor = await self.database.connection.execute(
            "SELECT COUNT(*) FROM wiki_chunks WHERE path = ?", (path,)
        )
        row = await cursor.fetchone()
        if row is None or int(row[0]) == 0:
            return "none"
        cursor = await self.database.connection.execute(
            """SELECT status FROM wiki_vector_manifest AS v
            JOIN wiki_chunks AS c ON c.chunk_id = v.chunk_id
            WHERE c.path = ? AND v.model = ?""",
            (path, self.embedding_provider.model_name),
        )
        statuses = {str(item[0]) for item in await cursor.fetchall()}
        if "pending" in statuses:
            return "pending"
        if "error" in statuses:
            return "error"
        if statuses and statuses == {"ready"}:
            return "ready"
        return "pending"

    async def mark_document_error(self, path: str, error: str) -> None:
        await self._cancel_vector_tasks_for_paths((path,))
        async with self._write_lock:
            connection = self.database.connection
            cursor = await connection.execute(
                "SELECT document_id FROM wiki_documents WHERE path = ?", (path,)
            )
            row = await cursor.fetchone()
            if row is None:
                await connection.execute(
                    """INSERT INTO wiki_documents(
                        path, document_id, title, frontmatter_json, content_hash,
                        modified_at_ns, size, sync_state, sync_error, last_indexed_at_ns
                    ) VALUES (?, ?, ?, '{}', '', 0, 0, 'error', ?, NULL)""",
                    (
                        path,
                        uuid4().hex,
                        path.rsplit("/", 1)[-1].removesuffix(".md"),
                        error,
                    ),
                )
            else:
                await connection.execute(
                    """UPDATE wiki_documents
                    SET sync_state = 'error', sync_error = ?
                    WHERE path = ?""",
                    (error, path),
                )
            await connection.commit()

    async def index_generation(self) -> str:
        """Return the fingerprint recorded when the projection was last written."""
        cursor = await self.database.connection.execute(
            "SELECT value FROM wiki_index_meta WHERE key = 'index_generation'"
        )
        row = await cursor.fetchone()
        return str(row[0]) if row is not None else ""

    async def mark_index_complete(self, generation: str) -> None:
        """Record the document-set fingerprint and that a full pass has completed.

        Distinguishing "indexed and empty" from "never indexed" needs a persistent flag:
        a zero row count cannot tell those apart on its own.
        """
        connection = self.database.connection
        await connection.execute(
            """INSERT INTO wiki_index_meta(key, value) VALUES ('index_generation', ?)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value""",
            (generation,),
        )
        await connection.execute(
            """INSERT INTO wiki_index_meta(key, value) VALUES ('index_completed', '1')
            ON CONFLICT(key) DO UPDATE SET value = '1'"""
        )
        await connection.commit()

    def finish_rebuild(self) -> None:
        """Delegate to the connection layer that owns the rebuild marker."""
        self.database.finish_rebuild()

    async def indexed_once(self) -> bool:
        """Return True when this projection has completed at least one full pass."""
        cursor = await self.database.connection.execute(
            "SELECT value FROM wiki_index_meta WHERE key = 'index_completed'"
        )
        return await cursor.fetchone() is not None

    async def fingerprints(self) -> dict[str, DocumentFingerprint]:
        cursor = await self.database.connection.execute(
            """SELECT path, modified_at_ns, size, content_hash, document_id
            FROM wiki_documents"""
        )
        return {
            str(row["path"]): DocumentFingerprint(
                modified_at_ns=int(row["modified_at_ns"]),
                size=int(row["size"]),
                content_hash=str(row["content_hash"]),
                document_id=str(row["document_id"]) if row["document_id"] else None,
            )
            for row in await cursor.fetchall()
        }

    async def replace_document(
        self,
        document: WikiDocument,
        chunks: tuple[IndexedChunk, ...],
        edges: tuple[GraphEdgeDraft, ...] = (),
        moved_from: str | None = None,
    ) -> str | None:
        async with self._task_lock:
            await self._cancel_vector_tasks_for_paths(
                tuple(path for path in (document.path.value, moved_from) if path is not None)
            )
            async with self._write_lock:
                return await self._replace_document(
                    document, chunks, edges, moved_from=moved_from
                )

    async def _replace_document(
        self,
        document: WikiDocument,
        chunks: tuple[IndexedChunk, ...],
        edges: tuple[GraphEdgeDraft, ...] = (),
        moved_from: str | None = None,
    ) -> str | None:
        tags = self._tag_text(document.frontmatter.get("tags", []))
        reusable_vectors = await self._reusable_vectors(document.path.value, moved_from)
        vector_warning = None
        vector_pending = (
            self.embedding_provider is not None
            and bool(chunks)
            and self.database.sqlite_vec_available
        )
        needs_vector_task = vector_pending and any(
            self._vector_hash(chunk) not in reusable_vectors for chunk in chunks
        )
        if self.embedding_provider is not None and chunks and not vector_pending:
            vector_warning = "semantic indexing unavailable: sqlite-vec is unavailable"
        connection = self.database.connection
        metadata = json.dumps(
            document.frontmatter,
            default=self._json_default,
            ensure_ascii=False,
            sort_keys=True,
        )
        try:
            document_id, stale_path = await self._prepare_document_identity(document, moved_from)
            # Release the previous projection before inserting the new one so a concurrent
            # reader observes all-old or all-new, never a missing document row.
            await self._delete_document_projection(document_id, document.path.value)
            if stale_path is not None and stale_path != document.path.value:
                await self._delete_document_projection(document_id, stale_path)
            await connection.execute(
                """
                INSERT INTO wiki_documents(
                    path, document_id, title, frontmatter_json, content_hash,
                    modified_at_ns, size, sync_state, sync_error, last_indexed_at_ns
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    document.path.value,
                    document_id,
                    document.title,
                    metadata,
                    document.content_hash,
                    document.modified_at_ns,
                    document.size,
                    "ready",
                    vector_warning,
                    time_ns(),
                ),
            )
            for chunk in chunks:
                await connection.execute(
                    """INSERT INTO wiki_chunks(
                        chunk_id, path, document_id, ordinal, section, content,
                        source_hash, embedding_hash
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
                    (
                        chunk.chunk_id,
                        document.path.value,
                        document_id,
                        chunk.ordinal,
                        chunk.section,
                        chunk.content,
                        chunk.source_hash,
                        chunk.embedding_hash or chunk.source_hash,
                    ),
                )
                characters, bigrams, words = analyze(
                    f"{document.title} {tags} {chunk.section} {chunk.content}"
                )
                await connection.execute(
                    """INSERT INTO wiki_chunks_fts(
                        chunk_id, path, search_chars, search_bigrams, search_words,
                        section, content
                    ) VALUES (?, ?, ?, ?, ?, ?, ?)""",
                    (
                        chunk.chunk_id,
                        document.path.value,
                        characters,
                        bigrams,
                        words,
                        chunk.section,
                        chunk.content,
                    ),
                )
            for edge in edges:
                edge_id = uuid4().hex
                await connection.execute(
                    """INSERT INTO wiki_edges(
                        edge_id, source_document_id, target_path, relation_type,
                        source_kind, anchor, source_section, context, metadata_json, source_hash
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                    (
                        edge_id,
                        document_id,
                        edge.target_path,
                        edge.relation_type,
                        edge.source_kind,
                        edge.anchor,
                        edge.source_section,
                        edge.context,
                        "{}",
                        document.content_hash,
                    ),
                )
            if vector_pending:
                await self._write_reused_vectors(document_id, chunks, reusable_vectors)
                await self._insert_pending_vectors(
                    document_id,
                    tuple(
                        chunk
                        for chunk in chunks
                        if self._vector_hash(chunk) not in reusable_vectors
                    ),
                )
            await connection.commit()
        except BaseException:
            # BaseException (not Exception) so a CancelledError still releases the open
            # transaction before propagating.
            await connection.rollback()
            raise
        if needs_vector_task:
            self._schedule_vector_sync(document_id, document, chunks, reusable_vectors)
        return vector_warning

    async def _prepare_document_identity(
        self, document: WikiDocument, moved_from: str | None
    ) -> tuple[str, str | None]:
        """Return the document identity to use and any stale path that must be cleared.

        The stale path is the previous location of a moved document, or the path of a
        different document that used to occupy the target path. Both are cleared with
        the *stale* path so the FTS projection is released correctly; the caller then
        inserts the new projection in the same transaction.
        """
        connection = self.database.connection
        cursor = await connection.execute(
            "SELECT document_id FROM wiki_documents WHERE path = ?",
            (document.path.value,),
        )
        row = await cursor.fetchone()
        if row is not None and row[0]:
            # A different document already occupies this path; the incoming content wins.
            return str(row[0]), document.path.value
        document_id = uuid4().hex
        stale_path: str | None = None
        if moved_from is not None:
            cursor = await connection.execute(
                "SELECT document_id FROM wiki_documents WHERE path = ?", (moved_from,)
            )
            moved_row = await cursor.fetchone()
            if moved_row is not None and moved_row[0]:
                document_id = str(moved_row[0])
                stale_path = moved_from
                # Keep incoming edges pointing at the moved document.
                await connection.execute(
                    "UPDATE wiki_edges SET target_path = ? WHERE target_document_id = ?",
                    (document.path.value, document_id),
                )
        return document_id, stale_path

    async def _delete_document_projection(self, document_id: str, path: str) -> None:
        connection = self.database.connection
        if self.database.sqlite_vec_available and self.database.vector_dimensions is not None:
            await connection.execute(
                """DELETE FROM wiki_vector_embeddings
                WHERE rowid IN (
                    SELECT vector_id FROM wiki_vector_manifest WHERE document_id = ?
                )""",
                (document_id,),
            )
        await connection.execute(
            "DELETE FROM wiki_vector_manifest WHERE document_id = ?", (document_id,)
        )
        await connection.execute("DELETE FROM wiki_chunks_fts WHERE path = ?", (path,))
        await connection.execute(
            "DELETE FROM wiki_edges WHERE source_document_id = ?", (document_id,)
        )
        # Delete chunks explicitly instead of relying on the FK cascade, and keep the
        # FTS table (which has no foreign keys) in step with it.
        await connection.execute("DELETE FROM wiki_chunks WHERE document_id = ?", (document_id,))
        await connection.execute("DELETE FROM wiki_chunks WHERE path = ?", (path,))
        await connection.execute("DELETE FROM wiki_documents WHERE document_id = ?", (document_id,))

    async def _reusable_vectors(
        self, path: str, moved_from: str | None
    ) -> dict[str, bytes]:
        if (
            self.embedding_provider is None
            or not self.database.sqlite_vec_available
            or self.database.vector_dimensions is None
        ):
            return {}
        source_path = moved_from or path
        rows = await self._rows(
            """
            SELECT v.source_hash, e.embedding
            FROM wiki_vector_manifest AS v
            JOIN wiki_documents AS d ON d.document_id = v.document_id
            JOIN wiki_vector_embeddings AS e ON e.rowid = v.vector_id
            WHERE d.path = ? AND v.model = ? AND v.status = 'ready'
            """,
            (source_path, self.embedding_provider.model_name),
        )
        return {
            str(row["source_hash"]): bytes(row["embedding"])
            for row in rows
            if row["embedding"] is not None
        }

    async def _write_reused_vectors(
        self, document_id: str, chunks: tuple[IndexedChunk, ...], reusable: dict[str, bytes]
    ) -> None:
        if not reusable or self.database.vector_dimensions is None:
            return
        await self.database.ensure_vector_table(self.database.vector_dimensions)
        connection = self.database.connection
        for chunk in chunks:
            embedding = reusable.get(self._vector_hash(chunk))
            if embedding is None:
                continue
            cursor = await connection.execute(
                """INSERT INTO wiki_vector_manifest(
                    chunk_id, document_id, model, dimensions, source_hash,
                    status, error, updated_at_ns
                ) VALUES (?, ?, ?, ?, ?, 'ready', NULL, ?)""",
                (
                    chunk.chunk_id,
                    document_id,
                    self.embedding_provider.model_name if self.embedding_provider else "",
                    self.database.vector_dimensions,
                    chunk.embedding_hash or chunk.source_hash,
                    time_ns(),
                ),
            )
            await connection.execute(
                "INSERT INTO wiki_vector_embeddings(rowid, embedding) VALUES (?, ?)",
                (cursor.lastrowid, embedding),
            )

    async def _insert_pending_vectors(
        self, document_id: str, chunks: tuple[IndexedChunk, ...]
    ) -> None:
        if self.embedding_provider is None:
            return
        connection = self.database.connection
        for chunk in chunks:
            await connection.execute(
                """INSERT INTO wiki_vector_manifest(
                    chunk_id, document_id, model, dimensions, source_hash,
                    status, error, updated_at_ns
                ) VALUES (?, ?, ?, 0, ?, 'pending', NULL, ?)""",
                (
                    chunk.chunk_id,
                    document_id,
                    self.embedding_provider.model_name,
                    self._vector_hash(chunk),
                    time_ns(),
                ),
            )

    def _schedule_vector_sync(
        self,
        document_id: str,
        document: WikiDocument,
        chunks: tuple[IndexedChunk, ...],
        reusable: dict[str, bytes],
    ) -> None:
        """Register a background vector task until the repository closes.

        Callers hold ``_task_lock`` (see :meth:`replace_document`), so registration
        and the ``_vector_closed`` check are atomic with respect to ``close()`` and
        to a concurrent cancellation for the same document. A task can therefore
        never escape both cancellation and the ``close()`` snapshot.
        """
        if self._vector_closed:
            return
        task = asyncio.create_task(
            self._synchronize_vectors(document_id, document, chunks, reusable)
        )
        self._vector_tasks.add(task)
        self._vector_tasks_by_document[document_id] = task

        def forget(completed: asyncio.Task[None]) -> None:
            self._vector_tasks.discard(completed)
            if self._vector_tasks_by_document.get(document_id) is completed:
                self._vector_tasks_by_document.pop(document_id, None)

        task.add_done_callback(forget)

    async def _synchronize_vectors(
        self,
        document_id: str,
        document: WikiDocument,
        chunks: tuple[IndexedChunk, ...],
        reusable: dict[str, bytes],
    ) -> None:
        if self.embedding_provider is None:
            return
        pending = tuple(chunk for chunk in chunks if self._vector_hash(chunk) not in reusable)
        new_vectors: tuple[tuple[IndexedChunk, ...], list[list[float]]] = ((), [])
        if pending:
            tags = self._tag_text(document.frontmatter.get("tags", []))
            texts = tuple(
                f"{document.title}\n{tags}\n{chunk.section}\n{chunk.content}"
                for chunk in pending
            )
            try:
                vectors = await asyncio.to_thread(
                    self.embedding_provider.embed_documents, texts
                )
                if len(vectors) != len(pending):
                    raise ValueError("embedding provider returned an unexpected vector count")
                if any(
                    not vector or not all(math.isfinite(value) for value in vector)
                    for vector in vectors
                ):
                    raise ValueError("embedding provider returned an invalid vector")
                dimensions = len(vectors[0])
                if any(len(vector) != dimensions for vector in vectors):
                    raise ValueError("embedding provider returned inconsistent vector dimensions")
                new_vectors = pending, vectors
            except (ImportError, OSError, RuntimeError, TypeError, ValueError) as exc:
                await self._mark_vector_error(
                    document_id,
                    document.content_hash,
                    tuple(self._vector_hash(chunk) for chunk in pending),
                    str(exc),
                )
                return

        async with self._write_lock:
            connection = self.database.connection
            current = await connection.execute(
                "SELECT content_hash FROM wiki_documents WHERE document_id = ?", (document_id,)
            )
            row = await current.fetchone()
            if row is None or str(row[0]) != document.content_hash:
                return
            try:
                await self._delete_pending_vector_entries(document_id)
                entries = [
                    (chunk, json.dumps(vector))
                    for chunk, vector in zip(new_vectors[0], new_vectors[1], strict=True)
                ]
                if entries:
                    dimensions = (
                        len(new_vectors[1][0])
                        if new_vectors[1]
                        else self.database.vector_dimensions
                    )
                    if dimensions is None:
                        return
                    await self.database.ensure_vector_table(dimensions)
                    for chunk, embedding in entries:
                        cursor = await connection.execute(
                            """INSERT INTO wiki_vector_manifest(
                                chunk_id, document_id, model, dimensions, source_hash,
                                status, error, updated_at_ns
                            ) VALUES (?, ?, ?, ?, ?, 'ready', NULL, ?)""",
                            (
                                chunk.chunk_id,
                                document_id,
                                self.embedding_provider.model_name,
                                dimensions,
                                self._vector_hash(chunk),
                                time_ns(),
                            ),
                        )
                        await connection.execute(
                            "INSERT INTO wiki_vector_embeddings(rowid, embedding) VALUES (?, ?)",
                            (cursor.lastrowid, embedding),
                        )
                await connection.execute(
                    """UPDATE wiki_documents
                    SET sync_error = NULL, sync_state = 'ready'
                    WHERE document_id = ? AND content_hash = ?""",
                    (document_id, document.content_hash),
                )
                await connection.commit()
            except (OSError, RuntimeError, sqlite3.Error) as exc:
                await connection.rollback()
                await self._mark_vector_error_locked(
                    document_id,
                    document.content_hash,
                    tuple(self._vector_hash(chunk) for chunk in pending),
                    str(exc),
                )
            except BaseException:
                await connection.rollback()
                raise

    async def _delete_pending_vector_entries(self, document_id: str) -> None:
        connection = self.database.connection
        if self.database.vector_dimensions is not None:
            await connection.execute(
                """DELETE FROM wiki_vector_embeddings
                WHERE rowid IN (
                    SELECT vector_id FROM wiki_vector_manifest
                    WHERE document_id = ? AND status != 'ready'
                )""",
                (document_id,),
            )
        await connection.execute(
            """DELETE FROM wiki_vector_manifest
            WHERE document_id = ? AND status != 'ready'""",
            (document_id,),
        )

    async def _mark_vector_error(
        self,
        document_id: str,
        content_hash: str,
        vector_hashes: tuple[str, ...],
        error: str,
    ) -> None:
        async with self._write_lock:
            await self._mark_vector_error_locked(
                document_id, content_hash, vector_hashes, error
            )

    async def _mark_vector_error_locked(
        self,
        document_id: str,
        content_hash: str,
        vector_hashes: tuple[str, ...],
        error: str,
    ) -> None:
        connection = self.database.connection
        try:
            placeholders = ", ".join("?" for _ in vector_hashes)
            await connection.execute(
                f"""UPDATE wiki_vector_manifest
                SET status = 'error', error = ?, updated_at_ns = ?
                WHERE document_id = ? AND status = 'pending'
                  AND source_hash IN ({placeholders})""",
                (error, time_ns(), document_id, *vector_hashes),
            )
            await connection.execute(
                """UPDATE wiki_documents
                SET sync_state = 'ready', sync_error = ?
                WHERE document_id = ? AND content_hash = ?""",
                (f"semantic indexing unavailable: {error}", document_id, content_hash),
            )
            await connection.commit()
        except BaseException:
            await connection.rollback()
            raise

    async def wait_for_initial_vector_sync(self) -> None:
        """Wait only when no vector table exists yet, keeping later updates asynchronous."""
        if not self._initial_vector_sync_required:
            return
        await self.wait_for_vector_sync()
        self._initial_vector_sync_required = False

    async def wait_for_vector_sync(self) -> None:
        if self._vector_tasks:
            await asyncio.gather(*tuple(self._vector_tasks), return_exceptions=True)

    async def close(self) -> None:
        """Stop background vector work and leave every pending row retryable.

        Background embedding runs in a thread that cannot be interrupted, so the
        coroutine is cancelled and the connection is only closed by the caller
        afterwards. Pending manifest rows are explicitly marked ``error`` because a
        cancelled task never reaches its own error handler.
        """
        self._vector_closed = True
        tasks = tuple(self._vector_tasks)
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._vector_tasks_by_document.clear()
        try:
            connection = self.database.connection
            cursor = await connection.execute(
                """UPDATE wiki_vector_manifest
                SET status = 'error',
                    error = COALESCE(error, 'vector sync cancelled at shutdown'),
                    updated_at_ns = ?
                WHERE status = 'pending'""",
                (time_ns(),),
            )
            if cursor.rowcount:
                # Work is outstanding, so the "documents unchanged" shortcut must not
                # hide it: the next startup has to retry those chunks.
                await connection.execute(
                    "DELETE FROM wiki_index_meta WHERE key = 'index_generation'"
                )
            await connection.commit()
        except (OSError, sqlite3.Error):
            # The connection may already be unusable; the next startup re-owns this state.
            pass

    async def delete_paths(self, paths: tuple[str, ...]) -> None:
        async with self._task_lock:
            await self._cancel_vector_tasks_for_paths(paths)
            async with self._write_lock:
                connection = self.database.connection
                for path in paths:
                    cursor = await connection.execute(
                        "SELECT document_id FROM wiki_documents WHERE path = ?", (path,)
                    )
                    row = await cursor.fetchone()
                    if row is not None:
                        await self._delete_document_projection(str(row[0]), path)
                await connection.commit()

    async def clear(self) -> None:
        async with self._task_lock:
            await self._cancel_vector_tasks()
            async with self._write_lock:
                connection = self.database.connection
                await connection.execute("DELETE FROM wiki_chunks_fts")
                await connection.execute("DELETE FROM wiki_edges")
                await connection.execute("DELETE FROM wiki_vector_manifest")
                if (
                    self.database.sqlite_vec_available
                    and self.database.vector_dimensions is not None
                ):
                    await connection.execute("DELETE FROM wiki_vector_embeddings")
                await connection.execute("DELETE FROM wiki_documents")
                await connection.execute(
                    "DELETE FROM wiki_index_meta "
                    "WHERE key IN ('index_generation', 'index_completed')"
                )
                await connection.commit()

    async def _cancel_vector_tasks(self) -> None:
        tasks = tuple(self._vector_tasks)
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._vector_tasks_by_document.clear()

    async def _cancel_vector_tasks_for_paths(self, paths: tuple[str, ...]) -> None:
        if not paths:
            return
        placeholders = ", ".join("?" for _ in paths)
        cursor = await self.database.connection.execute(
            f"SELECT document_id FROM wiki_documents WHERE path IN ({placeholders})", paths
        )
        document_ids = {str(row[0]) for row in await cursor.fetchall() if row[0]}
        tasks = tuple(
            self._vector_tasks_by_document.pop(document_id, None)
            for document_id in document_ids
        )
        active_tasks = tuple(task for task in tasks if task is not None)
        for task in active_tasks:
            task.cancel()
        if active_tasks:
            await asyncio.gather(*active_tasks, return_exceptions=True)

    async def resolve_edges(self) -> None:
        async with self._write_lock:
            await self.database.connection.execute(
                """
                UPDATE wiki_edges
                SET target_document_id = (
                        SELECT document_id FROM wiki_documents AS d
                        WHERE d.path = wiki_edges.target_path
                    ),
                    resolution_status = CASE WHEN EXISTS (
                        SELECT 1 FROM wiki_documents AS d WHERE d.path = wiki_edges.target_path
                    ) THEN 'resolved' ELSE 'unresolved' END
                """
            )
            await self.database.connection.commit()

    async def related_documents(
        self, paths: tuple[str, ...], *, limit: int = 5
    ) -> dict[str, tuple[RelatedDocument, ...]]:
        if not paths:
            return {}
        placeholders = ", ".join("?" for _ in paths)
        rows = await self._rows(
            f"""
            SELECT source.path AS source_path, e.target_path AS target_path,
                   COALESCE(target.title, e.target_path) AS target_title,
                   e.relation_type, e.resolution_status,
                   e.anchor, e.source_section, e.context,
                   'outgoing' AS direction
            FROM wiki_edges AS e
            JOIN wiki_documents AS source ON source.document_id = e.source_document_id
            LEFT JOIN wiki_documents AS target ON target.document_id = e.target_document_id
            WHERE source.path IN ({placeholders})
            UNION ALL
            SELECT target.path AS source_path, source.path AS target_path,
                   source.title AS target_title, e.relation_type, e.resolution_status,
                   e.anchor, e.source_section, e.context,
                   'incoming' AS direction
            FROM wiki_edges AS e
            JOIN wiki_documents AS source ON source.document_id = e.source_document_id
            JOIN wiki_documents AS target ON target.document_id = e.target_document_id
            WHERE target.path IN ({placeholders})
            """,
            paths + paths,
        )
        related: dict[str, list[RelatedDocument]] = {path: [] for path in paths}
        for row in rows:
            key = str(row["source_path"])
            direction = cast(Literal["outgoing", "incoming"], str(row["direction"]))
            path = row["target_path"]
            title = row["target_title"]
            if path is None or title is None:
                continue
            resolution_status = str(row["resolution_status"])
            if resolution_status not in {"resolved", "unresolved"}:
                resolution_status = "unresolved"
            values = related.setdefault(key, [])
            item = RelatedDocument(
                path=str(path),
                title=str(title),
                relation_type=str(row["relation_type"]),
                direction=direction,
                resolution_status=resolution_status,
                anchor=str(row["anchor"]) if row["anchor"] is not None else None,
                source_section=(
                    str(row["source_section"]) if row["source_section"] is not None else None
                ),
                context=str(row["context"]) if row["context"] is not None else None,
            )
            if item not in values and len(values) < limit:
                values.append(item)
        return {path: tuple(values) for path, values in related.items()}

    async def _rows(self, sql: str, parameters: tuple[object, ...]) -> list[sqlite3.Row]:
        cursor = await self.database.connection.execute(sql, parameters)
        return list(await cursor.fetchall())

    async def _candidate_rows(
        self,
        sql: str,
        parameters: tuple[object, ...],
        query: ContextQuery,
        *,
        similarity_key: bool = False,
    ) -> list[SearchCandidate]:
        """Run one candidate query and apply the checks SQL cannot express.

        ``scope``/``type``/``tags`` are pushed into SQL so ``LIMIT`` applies after
        filtering, and the keyword leg's phrase semantics are enforced by FTS5 itself,
        so only ``metadata_filters`` operators remain to be evaluated here (they are
        cheap on the bounded row set). ``similarity_key`` says the leg's rank key is a
        negated cosine similarity rather than bm25 or a timestamp.
        """
        rows = await self._rows(sql, parameters)
        candidates: list[SearchCandidate] = []
        for row in rows:
            path = str(row["path"])
            section = str(row["section"])
            content = str(row["content"])
            frontmatter = json.loads(row["frontmatter_json"])
            if not self._metadata_matches_query(frontmatter, query):
                continue
            candidates.append(
                SearchCandidate(
                    chunk_id=str(row["chunk_id"]),
                    path={"value": path},
                    title=str(row["title"]),
                    section=section,
                    content=content,
                    frontmatter=frontmatter,
                    modified_at_ns=int(row["modified_at_ns"]),
                    rank_score=self._rank_score(row, similarity_key=similarity_key),
                )
            )
        return candidates

    @staticmethod
    def _rank_score(row: sqlite3.Row, *, similarity_key: bool) -> float:
        """Return the source's own comparable rank key.

        Only the vector leg's key is a similarity (``-(cosine)``, negated so its
        ascending order still means "better first"); bm25 and timestamps are not
        comparable, so they are reported as 0.0.
        """
        if not similarity_key:
            return 0.0
        try:
            selected = row["selected_rank"]
        except (IndexError, KeyError):
            return 0.0
        return float(selected) if selected is not None else 0.0

    @classmethod
    def _filters_sql(cls, query: ContextQuery) -> tuple[list[str], list[object]]:
        """Push scope/type/tags filters into SQL so LIMIT applies after filtering."""
        conditions: list[str] = []
        parameters: list[object] = []
        scope = query.scope.strip("/")
        if scope:
            conditions.append("(c.path = ? OR c.path LIKE ? ESCAPE '\\')")
            parameters.extend((scope, f"{cls._like_pattern(scope)}%"))
        if query.note_types:
            placeholders = ", ".join("?" for _ in query.note_types)
            conditions.append(
                f"lower(COALESCE(json_extract(d.frontmatter_json, '$.type'), "
                f"json_extract(d.frontmatter_json, '$.note_type'), '')) IN ({placeholders})"
            )
            parameters.extend(value.casefold() for value in query.note_types)
        for requested in query.tags:
            spellings = (requested, *query.tag_aliases.get(requested, ()))
            parts: list[str] = []
            for spelling in spellings:
                canonical = spelling.casefold()
                parts.append("EXISTS (SELECT 1 FROM json_each(d.frontmatter_json, '$.tags') "
                             "AS t WHERE lower(CAST(t.value AS TEXT)) = ? OR "
                             "lower(CAST(t.value AS TEXT)) LIKE ? ESCAPE '\\')")
                parameters.extend((canonical, f"{cls._like_pattern(canonical)}%"))
            conditions.append(f"({' OR '.join(parts)})")
        return conditions, parameters

    @staticmethod
    def _candidate_sql(
        *,
        rows_from: str,
        select: str,
        extra_conditions: tuple[str, ...] = (),
        extra_parameters: tuple[object, ...] = (),
        per_document: int,
        limit: int,
        order: str,
        filters: tuple[list[str], list[object]],
    ) -> tuple[str, tuple[object, ...]]:
        filter_conditions, filter_parameters = filters
        conditions = [*filter_conditions, *extra_conditions]
        where = f"WHERE {' AND '.join(conditions)}" if conditions else ""
        sql = f"""
            WITH ranked AS (
                SELECT {select}
                FROM {rows_from}
                {where}
            ),
            chosen AS (
                SELECT *, ROW_NUMBER() OVER (
                    PARTITION BY path ORDER BY selected_rank, ordinal
                ) AS document_rank
                FROM ranked
            )
            SELECT chunk_id, path, title, section, content, frontmatter_json,
                   modified_at_ns, selected_rank
            FROM chosen
            WHERE document_rank <= ?
            ORDER BY selected_rank {order}, ordinal
            LIMIT ?
        """
        parameters = (*filter_parameters, *extra_parameters, per_document, limit)
        return sql, parameters

    def _query_candidates(
        self,
        query: ContextQuery,
        *,
        rows_from: str,
        select: str,
        extra_conditions: tuple[str, ...] = (),
        extra_parameters: tuple[object, ...] = (),
        per_document: int = 4,
        sql_limit: int | None = None,
        order: str = "ASC",
        candidate_limit: int,
    ) -> tuple[str, tuple[object, ...]]:
        return self._candidate_sql(
            rows_from=rows_from,
            select=select,
            extra_conditions=extra_conditions,
            extra_parameters=extra_parameters,
            per_document=per_document,
            limit=sql_limit if sql_limit is not None else candidate_limit,
            order=order,
            filters=self._filters_sql(query),
        )

    async def exact_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        """Match path/title substrings per query token, not the whole query string."""
        terms = query_terms(query.query)
        if not terms:
            return []
        conditions: list[str] = []
        parameters: list[object] = []
        for term in terms:
            pattern = self._like_pattern(term)
            conditions.append(
                "(d.path LIKE ? ESCAPE '\\' COLLATE NOCASE "
                "OR d.title LIKE ? ESCAPE '\\' COLLATE NOCASE)"
            )
            parameters.extend((pattern, pattern))
        sql, params = self._query_candidates(
            query,
            rows_from="wiki_chunks AS c JOIN wiki_documents AS d ON d.path = c.path",
            select=(
                "c.chunk_id AS chunk_id, c.path AS path, d.title AS title, c.section AS section, "
                "c.content AS content, d.frontmatter_json AS frontmatter_json, "
                "d.modified_at_ns AS modified_at_ns, d.modified_at_ns AS selected_rank, "
                "c.ordinal AS ordinal"
            ),
            extra_conditions=tuple(conditions),
            extra_parameters=tuple(parameters),
            order="DESC",
            candidate_limit=candidate_limit,
        )
        return await self._candidate_rows(sql, params, query)

    async def keyword_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        if not query.query.strip():
            return []
        match = query_expression(query.query)
        if not match:
            return []
        candidates = await self._keyword_rows(query, match, candidate_limit)
        if candidates:
            return candidates
        # Strict phrases found nothing; retry once with OR-joined terms so a question-shaped
        # query still contributes lexical candidates. bm25 keeps the ranking sane.
        relaxed = relaxed_query_expression(query.query)
        if not relaxed or relaxed == match:
            return []
        return await self._keyword_rows(query, relaxed, candidate_limit)

    async def _keyword_rows(
        self, query: ContextQuery, match: str, candidate_limit: int
    ) -> list[SearchCandidate]:
        sql, params = self._query_candidates(
            query,
            rows_from=(
                "wiki_chunks_fts JOIN wiki_chunks AS c ON c.chunk_id = wiki_chunks_fts.chunk_id "
                "JOIN wiki_documents AS d ON d.path = c.path"
            ),
            select=(
                "c.chunk_id AS chunk_id, c.path AS path, d.title AS title, c.section AS section, "
                "c.content AS content, d.frontmatter_json AS frontmatter_json, "
                "d.modified_at_ns AS modified_at_ns, bm25(wiki_chunks_fts) AS selected_rank, "
                "c.ordinal AS ordinal"
            ),
            extra_conditions=("wiki_chunks_fts MATCH ?",),
            extra_parameters=(match,),
            candidate_limit=candidate_limit,
        )
        return await self._candidate_rows(sql, params, query)

    async def graph_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        """Find source documents through explicit relation type or target metadata."""
        tokens = simple_tokens(query.query)
        if not tokens:
            return []
        conditions: list[str] = []
        parameters: list[object] = []
        for token in tokens:
            pattern = self._like_pattern(token.casefold())
            conditions.append(
                "(lower(e.relation_type) LIKE ? ESCAPE '\\' "
                "OR lower(e.target_path) LIKE ? ESCAPE '\\' "
                "OR lower(coalesce(target.title, '')) LIKE ? ESCAPE '\\')"
            )
            parameters.extend((pattern, pattern, pattern))
        sql, params = self._query_candidates(
            query,
            rows_from=(
                "wiki_edges AS e "
                "JOIN wiki_documents AS d ON d.document_id = e.source_document_id "
                "JOIN wiki_chunks AS c ON c.document_id = d.document_id AND c.ordinal = 0 "
                "LEFT JOIN wiki_documents AS target ON target.document_id = e.target_document_id"
            ),
            select=(
                "c.chunk_id AS chunk_id, c.path AS path, d.title AS title, c.section AS section, "
                "c.content AS content, d.frontmatter_json AS frontmatter_json, "
                "d.modified_at_ns AS modified_at_ns, d.modified_at_ns AS selected_rank, "
                "c.ordinal AS ordinal"
            ),
            extra_conditions=(f"({' OR '.join(conditions)})",),
            extra_parameters=tuple(parameters),
            order="DESC",
            candidate_limit=candidate_limit,
        )
        return await self._candidate_rows(sql, params, query)

    async def semantic_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        if (
            self.embedding_provider is None
            or not self.database.sqlite_vec_available
            or self.database.vector_dimensions is None
            or not query.query.strip()
        ):
            return []
        query_vector = await asyncio.to_thread(self.embedding_provider.embed_query, query.query)
        if (
            not query_vector
            or len(query_vector) != self.database.vector_dimensions
            or not all(math.isfinite(value) for value in query_vector)
        ):
            raise ValueError("embedding provider returned an invalid query vector")
        sql, params = self._query_candidates(
            query,
            rows_from=(
                "wiki_vector_embeddings AS knn "
                "JOIN wiki_vector_manifest AS v ON v.vector_id = knn.rowid "
                "JOIN wiki_chunks AS c ON c.chunk_id = v.chunk_id "
                "JOIN wiki_documents AS d ON d.path = c.path"
            ),
            select=(
                "c.chunk_id AS chunk_id, c.path AS path, d.title AS title, c.section AS section, "
                "c.content AS content, d.frontmatter_json AS frontmatter_json, "
                "d.modified_at_ns AS modified_at_ns, "
                # sqlite-vec reports L2 distance; vectors are unit-normalized, so this
                # is the cosine similarity negated to keep "lower rank is better".
                "-(1.0 - (knn.distance * knn.distance) / 2.0) AS selected_rank, "
                "c.ordinal AS ordinal"
            ),
            extra_conditions=(
                "knn.embedding MATCH ?",
                "knn.k = ?",
                "v.model = ?",
                "v.status = 'ready'",
                "v.source_hash = c.embedding_hash",
            ),
            extra_parameters=(
                json.dumps(query_vector),
                candidate_limit * 4,
                self.embedding_provider.model_name,
            ),
            candidate_limit=candidate_limit,
        )
        candidates = await self._candidate_rows(sql, params, query, similarity_key=True)
        threshold = query.min_similarity
        if threshold > 0:
            # Drop hits that are not close enough; an empty vector leg is a real answer
            # ("not found"), not a reason to fall back to weak matches.
            return [item for item in candidates if -item.rank_score >= threshold]
        return candidates

    async def recent_candidates(
        self, query: ContextQuery, *, candidate_limit: int
    ) -> list[SearchCandidate]:
        """Return the most recently modified chunks, newest first.

        Every chunk of a document shares ``modified_at_ns``, and the section a user
        just changed is not necessarily the document's first chunk, so this ranks all
        chunks instead of only ``ordinal = 0``.
        """
        sql, params = self._query_candidates(
            query,
            rows_from="wiki_documents AS d JOIN wiki_chunks AS c ON c.path = d.path",
            select=(
                "c.chunk_id AS chunk_id, c.path AS path, d.title AS title, c.section AS section, "
                "c.content AS content, d.frontmatter_json AS frontmatter_json, "
                "d.modified_at_ns AS modified_at_ns, d.modified_at_ns AS selected_rank, "
                "c.ordinal AS ordinal"
            ),
            per_document=1,
            order="DESC",
            candidate_limit=candidate_limit,
        )
        return await self._candidate_rows(sql, params, query)

    @classmethod
    def _metadata_matches_query(cls, metadata: dict[str, object], query: ContextQuery) -> bool:
        return all(
            cls._metadata_matches(metadata, key, expected)
            for key, expected in query.metadata_filters.items()
        )

    @staticmethod
    def _tag_text(value: object) -> str:
        if isinstance(value, (list, tuple)):
            return " ".join(str(item) for item in value)
        return str(value) if value else ""

    @staticmethod
    def _vector_hash(chunk: IndexedChunk) -> str:
        return chunk.embedding_hash or chunk.source_hash

    @staticmethod
    def _json_default(value: object) -> str:
        if isinstance(value, (date, datetime)):
            return value.isoformat()
        raise TypeError(f"Object of type {type(value).__name__} is not JSON serializable")

    @classmethod
    def _metadata_matches(
        cls, metadata: dict[str, object], key: str, expected: object
    ) -> bool:
        """Compare one Frontmatter value against a filter.

        ``key`` is a dotted path (``owner.name``) resolved against the whole frontmatter
        mapping. An expected mapping with a single key is an operator:

        - ``$gt`` / ``$gte`` / ``$lt`` / ``$lte``: numeric when both sides are numbers,
          otherwise lexicographic; if the two sides are of different kinds (number vs
          text/bool) the filter does not match at all.
        - ``$between``: inclusive range over ``[low, high]``, same kind rule as above.
        - ``$ne``: not equal.
        - ``$in``: the actual value equals one of the listed values.

        Any other operator shape does not match. Booleans are deliberately not numbers.
        """
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
                if cls._is_text(actual) and cls._is_text(bound):
                    return cls._compare_text(operator_name, str(actual), str(bound))
                return False
            if operator_name == "$between" and isinstance(bound, list) and len(bound) == 2:
                low, high = bound
                if cls._is_number(actual) and cls._is_number(low) and cls._is_number(high):
                    return float(low) <= float(actual) <= float(high)
                if cls._is_text(actual) and cls._is_text(low) and cls._is_text(high):
                    return str(low) <= str(actual) <= str(high)
                return False
            if operator_name == "$ne":
                return actual != bound
            if operator_name == "$in" and isinstance(bound, list):
                return any(actual == item for item in bound)
            return False
        if isinstance(actual, list) and isinstance(expected, list):
            return all(item in actual for item in expected)
        return actual == expected

    @staticmethod
    def _is_number(value: object) -> TypeGuard[int | float]:
        return isinstance(value, (int, float)) and not isinstance(value, bool)

    @staticmethod
    def _is_text(value: object) -> TypeGuard[str]:
        return isinstance(value, str)

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
        """Drop stored vectors when the configured embedding model changed.

        Only writes when the model is new or different, so a startup that changes nothing
        does not open a write transaction.
        """
        connection = self.database.connection
        cursor = await connection.execute(
            "SELECT value FROM wiki_index_meta WHERE key = 'vector_model'"
        )
        row = await cursor.fetchone()
        configured = None if self.embedding_provider is None else self.embedding_provider.model_name
        stored = None if row is None else str(row["value"])
        if stored == configured:
            return
        if stored is not None:
            # drop_vector_table also clears 'index_generation', which is what forces the
            # next sync to re-embed instead of taking the unchanged-documents fast path.
            await self.database.drop_vector_table()
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
