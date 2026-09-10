"""SQLite connection and schema lifecycle for the rebuildable search index."""

from pathlib import Path
import sqlite3

import aiosqlite


class SQLiteDatabase:
    def __init__(self, path: Path) -> None:
        self.path = path.expanduser()
        self._connection: aiosqlite.Connection | None = None

    @property
    def connection(self) -> aiosqlite.Connection:
        if self._connection is None:
            raise RuntimeError("SQLiteDatabase must be initialized asynchronously")
        return self._connection

    async def initialize(self) -> None:
        if self._connection is not None:
            return
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._connection = await aiosqlite.connect(self.path)
        self.connection.row_factory = sqlite3.Row
        await self.connection.execute("PRAGMA busy_timeout=5000")
        await self.connection.execute("PRAGMA synchronous=NORMAL")
        await self.connection.execute("PRAGMA journal_mode=WAL")
        await self.connection.execute("PRAGMA foreign_keys=ON")
        await self.connection.executescript(
            """
            CREATE TABLE IF NOT EXISTS wiki_documents (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                modified_at_ns INTEGER NOT NULL,
                size INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS wiki_chunks (
                chunk_id TEXT PRIMARY KEY,
                path TEXT NOT NULL REFERENCES wiki_documents(path) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                section TEXT NOT NULL,
                content TEXT NOT NULL,
                source_hash TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wiki_documents_modified
                ON wiki_documents(modified_at_ns DESC);
            CREATE INDEX IF NOT EXISTS idx_wiki_chunks_path
                ON wiki_chunks(path, ordinal);
            CREATE VIRTUAL TABLE IF NOT EXISTS wiki_chunks_fts USING fts5(
                chunk_id UNINDEXED,
                path UNINDEXED,
                title,
                section,
                content
            );
            CREATE TABLE IF NOT EXISTS wiki_vectors (
                chunk_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                model TEXT NOT NULL,
                vector_json TEXT NOT NULL,
                source_hash TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS wiki_index_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            """
        )
        await self.connection.commit()

    async def close(self) -> None:
        if self._connection is not None:
            await self._connection.close()
            self._connection = None
