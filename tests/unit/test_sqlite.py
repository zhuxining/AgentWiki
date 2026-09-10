import aiosqlite

from agentwiki.repository.sqlite import SQLiteDatabase


async def test_existing_index_schema_is_discarded_and_recreated(tmp_path) -> None:
    index = tmp_path / "index.sqlite3"
    async with aiosqlite.connect(index) as connection:
        await connection.executescript(
            """
            CREATE TABLE wiki_documents (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                modified_at_ns INTEGER NOT NULL,
                size INTEGER NOT NULL
            );
            CREATE TABLE wiki_chunks (
                chunk_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                section TEXT NOT NULL,
                content TEXT NOT NULL,
                source_hash TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE wiki_chunks_fts USING fts5(
                chunk_id UNINDEXED, path UNINDEXED, title, section, content
            );
            CREATE TABLE wiki_vectors (
                chunk_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                model TEXT NOT NULL,
                vector_json TEXT NOT NULL,
                source_hash TEXT NOT NULL
            );
            CREATE TABLE wiki_index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO wiki_documents(path, title, frontmatter_json, modified_at_ns, size)
            VALUES ('stale.md', 'Stale', '{}', 1, 1);
            """
        )
        await connection.commit()

    database = SQLiteDatabase(index)
    await database.initialize()
    try:
        cursor = await database.connection.execute("PRAGMA table_info(wiki_documents)")
        document_columns = {str(row[1]) for row in await cursor.fetchall()}
        assert {
            "document_id",
            "content_hash",
            "sync_state",
            "sync_error",
        } <= document_columns
        cursor = await database.connection.execute(
            "SELECT COUNT(*) FROM wiki_documents WHERE path = 'stale.md'"
        )
        row = await cursor.fetchone()
        assert row is not None
        assert int(row[0]) == 0

        cursor = await database.connection.execute("PRAGMA table_info(wiki_chunks)")
        chunk_columns = {str(row[1]) for row in await cursor.fetchall()}
        assert {"document_id", "embedding_hash"} <= chunk_columns

        cursor = await database.connection.execute(
            "SELECT name FROM sqlite_master WHERE name IN ('wiki_edges', 'wiki_vector_manifest')"
        )
        assert {str(row[0]) for row in await cursor.fetchall()} == {
            "wiki_edges",
            "wiki_vector_manifest",
        }
    finally:
        await database.close()
