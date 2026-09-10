"""SQLite connection and schema lifecycle for the rebuildable search index."""

import contextlib
import json
from pathlib import Path
import sqlite3

import aiosqlite
from loguru import logger

try:
    import sqlite_vec
except ImportError:  # pragma: no cover - dependency is optional at runtime
    sqlite_vec = None

_SCHEMA_VERSION = 4


def _now_iso() -> str:
    from datetime import UTC, datetime

    return datetime.now(UTC).isoformat(timespec="seconds")


class SQLiteDatabase:
    def __init__(self, path: Path) -> None:
        self.path = path.expanduser()
        self._connection: aiosqlite.Connection | None = None
        self.sqlite_vec_available = False
        self.vector_dimensions: int | None = None
        # Set when a previous rebuild marker was found at startup.
        self.incomplete_rebuild: str = ""

    @property
    def connection(self) -> aiosqlite.Connection:
        if self._connection is None:
            raise RuntimeError("SQLiteDatabase must be initialized asynchronously")
        return self._connection

    async def initialize(self) -> None:
        if self._connection is not None:
            return
        await self._open_connection()
        if await self._needs_rebuild():
            self.incomplete_rebuild = self._read_rebuild_marker()
            if self.incomplete_rebuild:
                logger.warning(
                    "previous index rebuild did not finish ({}) - rebuilding {}",
                    self.incomplete_rebuild,
                    self.path,
                )
            else:
                logger.info(
                    "index schema changed (found {}, current {}); rebuilding - "
                    "original Markdown is untouched, but a large library takes a while",
                    await self._stored_version(),
                    _SCHEMA_VERSION,
                )
            self._write_rebuild_marker()
            await self._close_connection()
            self._remove_index_files()
            await self._open_connection()
        await self._load_sqlite_vec()
        await self.connection.executescript(
            """
            CREATE TABLE IF NOT EXISTS wiki_documents (
                path TEXT PRIMARY KEY,
                document_id TEXT UNIQUE,
                title TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                content_hash TEXT NOT NULL DEFAULT '',
                modified_at_ns INTEGER NOT NULL,
                size INTEGER NOT NULL,
                sync_state TEXT NOT NULL DEFAULT 'ready',
                sync_error TEXT,
                last_indexed_at_ns INTEGER
            );
            CREATE TABLE IF NOT EXISTS wiki_chunks (
                chunk_id TEXT PRIMARY KEY,
                path TEXT NOT NULL REFERENCES wiki_documents(path) ON DELETE CASCADE,
                document_id TEXT,
                ordinal INTEGER NOT NULL,
                section TEXT NOT NULL,
                content TEXT NOT NULL,
                source_hash TEXT NOT NULL,
                embedding_hash TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_wiki_documents_modified
                ON wiki_documents(modified_at_ns DESC);
            CREATE INDEX IF NOT EXISTS idx_wiki_chunks_path
                ON wiki_chunks(path, ordinal);
            CREATE VIRTUAL TABLE IF NOT EXISTS wiki_chunks_fts USING fts5(
                chunk_id UNINDEXED,
                path UNINDEXED,
                search_chars,
                search_bigrams,
                search_words,
                section UNINDEXED,
                content UNINDEXED,
                tokenize='unicode61 tokenchars 0x2F',
                prefix='1,2,3'
            );
            CREATE TABLE IF NOT EXISTS wiki_edges (
                edge_id TEXT PRIMARY KEY,
                source_document_id TEXT NOT NULL,
                target_document_id TEXT,
                target_path TEXT NOT NULL,
                relation_type TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                anchor TEXT,
                source_section TEXT,
                context TEXT,
                metadata_json TEXT NOT NULL,
                source_hash TEXT NOT NULL,
                resolution_status TEXT NOT NULL DEFAULT 'unresolved',
                FOREIGN KEY (source_document_id) REFERENCES wiki_documents(document_id)
                    ON DELETE CASCADE,
                FOREIGN KEY (target_document_id) REFERENCES wiki_documents(document_id)
                    ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wiki_edges_source
                ON wiki_edges(source_document_id, relation_type);
            CREATE INDEX IF NOT EXISTS idx_wiki_edges_target
                ON wiki_edges(target_document_id);
            CREATE INDEX IF NOT EXISTS idx_wiki_edges_target_path
                ON wiki_edges(target_path);
            CREATE TABLE IF NOT EXISTS wiki_vector_manifest (
                vector_id INTEGER PRIMARY KEY AUTOINCREMENT,
                chunk_id TEXT UNIQUE NOT NULL,
                document_id TEXT NOT NULL,
                model TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                source_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                error TEXT,
                updated_at_ns INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS wiki_index_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            """
        )
        await self.connection.execute(f"PRAGMA user_version = {_SCHEMA_VERSION}")
        cursor = await self.connection.execute(
            """SELECT value FROM wiki_index_meta WHERE key = 'vector_dimensions'"""
        )
        row = await cursor.fetchone()
        if row is not None and self.sqlite_vec_available:
            table_cursor = await self.connection.execute(
                """SELECT 1 FROM sqlite_master WHERE name = 'wiki_vector_embeddings'"""
            )
            if await table_cursor.fetchone() is not None:
                self.vector_dimensions = int(row[0])
        await self.connection.commit()

    async def _open_connection(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._connection = await aiosqlite.connect(self.path)
        self.connection.row_factory = sqlite3.Row
        await self.connection.execute("PRAGMA busy_timeout=5000")
        await self.connection.execute("PRAGMA synchronous=NORMAL")
        await self.connection.execute("PRAGMA journal_mode=WAL")
        await self.connection.execute("PRAGMA foreign_keys=ON")

    async def _needs_rebuild(self) -> bool:
        cursor = await self.connection.execute("PRAGMA user_version")
        row = await cursor.fetchone()
        version = 0 if row is None else int(row[0])
        if version == _SCHEMA_VERSION:
            return False
        cursor = await self.connection.execute(
            """SELECT 1 FROM sqlite_master
            WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
            LIMIT 1"""
        )
        return await cursor.fetchone() is not None

    async def _close_connection(self) -> None:
        if self._connection is not None:
            await self._connection.close()
            self._connection = None

    def finish_rebuild(self) -> None:
        """Clear the rebuild marker once the projection has been repopulated."""
        if self._marker_path().is_file():
            self._marker_path().unlink(missing_ok=True)

    def _marker_path(self) -> Path:
        return self.path.with_name(f"{self.path.name}.rebuild.json")

    async def _stored_version(self) -> int:
        cursor = await self.connection.execute("PRAGMA user_version")
        row = await cursor.fetchone()
        return 0 if row is None else int(row[0])

    def _read_rebuild_marker(self) -> str:
        marker = self._marker_path()
        if not marker.is_file():
            return ""
        try:
            payload = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return "marker unreadable"
        started = payload.get("started_at") if isinstance(payload, dict) else None
        return str(started) if started else "started at an unknown time"

    def _write_rebuild_marker(self) -> None:
        marker = self._marker_path()
        try:
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text(
                json.dumps({"schema_version": _SCHEMA_VERSION, "started_at": _now_iso()}),
                encoding="utf-8",
            )
        except OSError as exc:
            logger.warning("could not record the rebuild marker {}: {}", marker, exc)

    def _remove_index_files(self) -> None:
        for suffix in ("", "-wal", "-shm"):
            try:
                self.path.with_name(self.path.name + suffix).unlink(missing_ok=True)
            except OSError as exc:
                raise RuntimeError(f"unable to recreate derived index: {self.path}") from exc

    async def _load_sqlite_vec(self) -> None:
        if sqlite_vec is None:
            return
        try:
            await self.connection.enable_load_extension(True)
            await self.connection.execute(
                "SELECT load_extension(?)", (sqlite_vec.loadable_path(),)
            )
            self.sqlite_vec_available = True
        except (OSError, sqlite3.Error):
            self.sqlite_vec_available = False
        finally:
            with contextlib.suppress(sqlite3.Error):
                await self.connection.enable_load_extension(False)

    async def ensure_vector_table(self, dimensions: int) -> None:
        if not self.sqlite_vec_available:
            raise RuntimeError("sqlite-vec is unavailable")
        if dimensions < 1:
            raise ValueError("vector dimensions must be positive")
        cursor = await self.connection.execute(
            "SELECT value FROM wiki_index_meta WHERE key = 'vector_dimensions'"
        )
        row = await cursor.fetchone()
        stored = None if row is None else int(row[0])
        if stored is not None and stored != dimensions:
            await self.connection.execute("DROP TABLE IF EXISTS wiki_vector_embeddings")
            await self.connection.execute("DELETE FROM wiki_vector_manifest")
        await self.connection.execute(
            f"CREATE VIRTUAL TABLE IF NOT EXISTS wiki_vector_embeddings "
            f"USING vec0(embedding float[{dimensions}])"
        )
        await self.connection.execute(
            """INSERT INTO wiki_index_meta(key, value) VALUES ('vector_dimensions', ?)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value""",
            (str(dimensions),),
        )
        self.vector_dimensions = dimensions

    async def drop_vector_table(self) -> None:
        if self.sqlite_vec_available:
            await self.connection.execute("DROP TABLE IF EXISTS wiki_vector_embeddings")
        await self.connection.execute("DELETE FROM wiki_vector_manifest")
        # Dropping vectors invalidates the "documents unchanged" shortcut: every chunk
        # needs re-embedding even though the Markdown did not change.
        await self.connection.execute(
            "DELETE FROM wiki_index_meta "
            "WHERE key IN ('vector_model', 'vector_dimensions', 'index_generation')"
        )
        self.vector_dimensions = None

    async def close(self) -> None:
        if self._connection is not None:
            await self._connection.close()
            self._connection = None
