//! SQLite projection commit ledger.
//!
//! SQLite is deliberately not a retrieval backend. It records only the last
//! fully confirmed Markdown projection and the LanceDB format marker.

use camino::Utf8Path;
use rusqlite::Connection;

use crate::error::Result;

#[derive(Clone)]
pub struct Metadata {
    inner: std::sync::Arc<std::sync::Mutex<MetaStore>>,
}

impl Metadata {
    pub async fn open(index_dir: camino::Utf8PathBuf) -> Result<Self> {
        let store = tokio::task::spawn_blocking(move || MetaStore::open(&index_dir))
            .await
            .map_err(|error| {
                crate::error::AgentWikiError::Other(format!("metadata task failed: {error}"))
            })??;
        Ok(Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(store)),
        })
    }

    pub async fn with<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut MetaStore) -> Result<T> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = inner.lock().map_err(|error| {
                crate::error::AgentWikiError::Other(format!("metadata lock poisoned: {error}"))
            })?;
            work(&mut store)
        })
        .await
        .map_err(|error| {
            crate::error::AgentWikiError::Other(format!("metadata task failed: {error}"))
        })?
    }
}

pub const RETRIEVAL_FORMAT: &str = "lance-unified-read-plane-v4";

pub struct MetaStore {
    pub(crate) needs_rebuild: bool,
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub path: String,
    pub content_hash: String,
    pub mtime_ns: i64,
    pub size: u64,
    pub vector_input_hash: Option<String>,
}

impl MetaStore {
    pub fn open(index_dir: &Utf8Path) -> Result<Self> {
        let conn = Connection::open(index_dir.join("agentwiki.sqlite3"))?;
        const SCHEMA_VERSION: i32 = 7;
        let current = conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
            .unwrap_or(0);
        if current != SCHEMA_VERSION {
            conn.execute_batch(
                "DROP TABLE IF EXISTS document_tags;
                 DROP TABLE IF EXISTS document_aliases;
                 DROP TABLE IF EXISTS document_facets;
                 DROP TABLE IF EXISTS documents;
                 DROP TABLE IF EXISTS edges;
                 DROP TABLE IF EXISTS vector_manifest;
                 DROP TABLE IF EXISTS index_meta;
                 DROP TABLE IF EXISTS sync_ledger;
                 DROP TABLE IF EXISTS projection_meta;",
            )?;
            conn.execute_batch(SCHEMA_SQL)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else {
            conn.execute_batch(SCHEMA_SQL)?;
        }
        let format_matches = conn
            .query_row(
                "SELECT retrieval_format FROM projection_meta WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .as_deref()
            == Some(RETRIEVAL_FORMAT);
        Ok(Self {
            needs_rebuild: current != SCHEMA_VERSION || !format_matches,
            conn,
        })
    }

    pub fn ledger_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, crate::document::types::Fingerprint>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, content_hash, mtime_ns, size FROM sync_ledger")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                crate::document::types::Fingerprint {
                    content_hash: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    size: row.get::<_, i64>(3)? as u64,
                },
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }

    pub fn vector_hash(&self, path: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT vector_input_hash FROM sync_ledger WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn upsert_ledger(&self, row: &LedgerRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_ledger(path, content_hash, mtime_ns, size, vector_input_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
               content_hash = excluded.content_hash,
               mtime_ns = excluded.mtime_ns,
               size = excluded.size,
               vector_input_hash = excluded.vector_input_hash",
            rusqlite::params![
                row.path,
                row.content_hash,
                row.mtime_ns,
                row.size as i64,
                row.vector_input_hash,
            ],
        )?;
        Ok(())
    }

    pub fn update_fingerprint(
        &self,
        path: &str,
        fingerprint: &crate::document::types::Fingerprint,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_ledger SET mtime_ns = ?2, size = ?3 WHERE path = ?1",
            rusqlite::params![path, fingerprint.mtime_ns, fingerprint.size as i64],
        )?;
        Ok(())
    }

    pub fn confirm_vectors(&self, path: &str, hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_ledger SET vector_input_hash = ?2 WHERE path = ?1",
            rusqlite::params![path, hash],
        )?;
        Ok(())
    }

    pub fn delete_ledger(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sync_ledger WHERE path = ?1", [path])?;
        Ok(())
    }

    pub fn set_retrieval_format(&self, format: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO projection_meta(singleton, retrieval_format) VALUES (1, ?1)",
            [format],
        )?;
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM sync_ledger; DELETE FROM projection_meta;")?;
        Ok(())
    }
}

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sync_ledger (
    path              TEXT PRIMARY KEY,
    content_hash      TEXT NOT NULL,
    mtime_ns          INTEGER NOT NULL,
    size              INTEGER NOT NULL,
    vector_input_hash TEXT
);

CREATE TABLE IF NOT EXISTS projection_meta (
    singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
    retrieval_format TEXT NOT NULL
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn index_dir() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    #[test]
    fn schema_contains_only_commit_state() {
        let (_temp, path) = index_dir();
        let store = MetaStore::open(&path).unwrap();
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let tables = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(tables, ["projection_meta", "sync_ledger"]);
    }

    #[test]
    fn vector_confirmation_is_independent() {
        let (_temp, path) = index_dir();
        let store = MetaStore::open(&path).unwrap();
        store
            .upsert_ledger(&LedgerRow {
                path: "a.md".into(),
                content_hash: "content".into(),
                mtime_ns: 1,
                size: 2,
                vector_input_hash: None,
            })
            .unwrap();
        assert_eq!(store.vector_hash("a.md").unwrap(), None);
        store.confirm_vectors("a.md", "vector").unwrap();
        assert_eq!(
            store.vector_hash("a.md").unwrap().as_deref(),
            Some("vector")
        );
    }
}
