//! SQLite metadata store — the sync ledger, document graph and persistent state.
//!
//! This is **not** a search backend. It never executes full-text or vector queries
//! (that belongs to `retrieval`). It holds:
//! - the per-document fingerprint ledger used by incremental sync;
//! - the one-hop document graph;
//! - index generation markers and vector manifest state.

use camino::Utf8Path;
use rusqlite::Connection;

use crate::document::types::{Edge, EdgeStatus, PathScope};
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

/// Retrieval projection format version, stored under `retrieval_format` in
/// `index_meta`. Bump whenever the LanceDB index layout or tokenizer
/// configuration changes: a mismatch marks the projection for full rebuild
/// (derived projections are wiped, never migrated).
pub const RETRIEVAL_FORMAT: &str = "fts-jieba-v1";

/// Thin wrapper over a `rusqlite::Connection` for wiki metadata.
///
/// `Connection` is `Send` but not `Sync`; callers that need sharing across
/// tasks can wrap it in a `tokio::sync::Mutex` or use `spawn_blocking`.
pub struct MetaStore {
    pub(crate) needs_rebuild: bool,
    conn: Connection,
}

/// Ledger row for one document.
#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub embedding_hash: Option<String>,
    pub path: String,
    pub content_hash: String,
    pub mtime_ns: i64,
    pub size: u64,
    pub stable_identity: String,
    pub sync_error: Option<String>,
    pub frontmatter_json: String,
}

impl MetaStore {
    /// Open (or create) the metadata store at `index_dir/agentwiki.sqlite3`.
    ///
    /// The DB is treated as a fully rebuildable projection: an incompatible
    /// schema (checked via `PRAGMA user_version`) triggers destruction and
    /// re-creation, mirroring the Python `PRAGMA user_version` policy without
    /// column migration.
    pub fn open(index_dir: &Utf8Path) -> Result<MetaStore> {
        let db_path = index_dir.join("agentwiki.sqlite3");
        let conn = Connection::open(&db_path)?;

        // Version-gate the schema: bump `SCHEMA_VERSION` when the schema text
        // changes. On mismatch we drop and recreate; Markdown is untouched.
        const SCHEMA_VERSION: i32 = 3;
        let cur: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if cur != SCHEMA_VERSION {
            conn.execute_batch("DROP TABLE IF EXISTS documents; DROP TABLE IF EXISTS edges; DROP TABLE IF EXISTS vector_manifest; DROP TABLE IF EXISTS index_meta; DROP TABLE IF EXISTS titles;")?;
            conn.execute_batch(SCHEMA_SQL)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else {
            conn.execute_batch(SCHEMA_SQL)?;
        }

        // Retrieval projection format: a tokenizer/index layout change must
        // rebuild the whole LanceDB projection, not just delete rows (the FTS
        // index parameters live inside the index). Absent marker also counts
        // as stale — every pre-marker projection is rebuilt once.
        let format_matches = conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'retrieval_format'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .as_deref()
            == Some(RETRIEVAL_FORMAT);

        // Mirrors the Python write-serialization discipline: a single write
        // mutex serializes every mutation regardless of task scheduling.
        Ok(MetaStore {
            needs_rebuild: cur != SCHEMA_VERSION || !format_matches,
            conn,
        })
    }

    /// List the current ledger as (path -> fingerprint) map.
    pub fn ledger_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, crate::document::types::Fingerprint>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, content_hash, mtime_ns, size FROM documents")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                crate::document::types::Fingerprint {
                    content_hash: r.get(1)?,
                    mtime_ns: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                },
            ))
        })?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let (p, fp) = row?;
            out.insert(p, fp);
        }
        Ok(out)
    }

    /// Return paths ordered by real file modification time, newest first.
    pub fn recent_paths(&self, scope: &str, limit: usize) -> Result<Vec<String>> {
        let mut sql = String::from("SELECT path FROM documents");
        let scoped = !scope.trim().is_empty();
        if scoped {
            sql.push_str(" WHERE path = ?1 OR path LIKE ?2");
        }
        sql.push_str(" ORDER BY mtime_ns DESC, path ASC LIMIT ?3");
        let mut stmt = self.conn.prepare(&sql)?;
        if scoped {
            let scope = scope.trim().trim_end_matches('/');
            let prefix = format!("{scope}/%");
            let rows = stmt.query_map(rusqlite::params![scope, prefix, limit as i64], |row| {
                row.get(0)
            })?;
            rows.collect::<std::result::Result<Vec<String>, _>>()
                .map_err(Into::into)
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT path FROM documents ORDER BY mtime_ns DESC, path ASC LIMIT ?1")?;
            let rows = stmt.query_map(rusqlite::params![limit as i64], |row| row.get(0))?;
            rows.collect::<std::result::Result<Vec<String>, _>>()
                .map_err(Into::into)
        }
    }

    /// Persist one document's ledger fingerprint.
    pub fn upsert_ledger(&self, row: &LedgerRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO documents
                (path, content_hash, mtime_ns, size, stable_identity, sync_error, frontmatter_json, embedding_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                row.path,
                row.content_hash,
                row.mtime_ns,
                row.size as i64,
                row.stable_identity,
                row.sync_error,
                row.frontmatter_json,
                row.embedding_hash,
            ],
        )?;
        Ok(())
    }

    /// Delete one document's ledger row.
    pub fn delete_ledger(&self, path: &PathScope) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE from_path = ?1", [path.0.as_str()])?;
        self.conn
            .execute("DELETE FROM titles WHERE path = ?1", [path.0.as_str()])?;
        self.conn
            .execute("DELETE FROM documents WHERE path = ?1", [path.0.as_str()])?;
        Ok(())
    }

    /// Read one document's frontmatter projection for query-time filters.
    pub fn frontmatter_for_path(&self, path: &str) -> Result<crate::document::types::Frontmatter> {
        let raw: String = self.conn.query_row(
            "SELECT frontmatter_json FROM documents WHERE path = ?1",
            [path],
            |row| row.get(0),
        )?;
        serde_json::from_str(&raw).map_err(|e| crate::error::AgentWikiError::Other(e.to_string()))
    }

    /// Aggregate tag usage across the ledger projection (dynamic `known_tags`
    /// source; updated with every sync, so it must not be cached with the
    /// rule fingerprint alone).
    pub fn all_tags(&self) -> Result<std::collections::BTreeMap<String, usize>> {
        let mut stmt = self
            .conn
            .prepare("SELECT frontmatter_json FROM documents")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut counts = std::collections::BTreeMap::new();
        for row in rows {
            let raw: String = row?;
            let Ok(fm) = serde_json::from_str::<crate::document::types::Frontmatter>(&raw) else {
                continue;
            };
            if let Some(serde_json::Value::Array(tags)) = fm.get("tags") {
                for tag in tags.iter().filter_map(|v| v.as_str()) {
                    *counts.entry(tag.to_string()).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }

    /// Ledger fingerprint of one document (for display timestamps).
    pub fn ledger_fingerprint(&self, path: &str) -> Result<crate::document::types::Fingerprint> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT content_hash, mtime_ns, size FROM documents WHERE path = ?1",
                [path],
                |r| {
                    Ok(crate::document::types::Fingerprint {
                        content_hash: r.get(0)?,
                        mtime_ns: r.get(1)?,
                        size: r.get::<_, i64>(2)? as u64,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Replace the one-hop edge projection for a document.
    pub fn replace_document(&self, path: &PathScope, edges: &[Edge]) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE from_path = ?1", [path.0.as_str()])?;
        for e in edges {
            self.conn.execute(
                "INSERT OR REPLACE INTO edges
                    (from_path, relation_type, to_path, section_source, status)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    e.from.0.as_str(),
                    e.relation_type,
                    e.to.0.as_str(),
                    e.section_source,
                    e.status.as_str(),
                ],
            )?;
        }
        Ok(())
    }

    /// Set the persisted `index_generation` marker.
    pub fn set_generation(&self, generation: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('index_generation', ?1)",
            [generation],
        )?;
        Ok(())
    }

    /// Persist the retrieval projection format version. Only written after a
    /// sync/rebuild round confirms the projection, so a stale marker keeps
    /// forcing the full rebuild (idempotent).
    pub fn set_retrieval_format(&self, version: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('retrieval_format', ?1)",
            [version],
        )?;
        Ok(())
    }

    /// Clear every derived projection (document ledger + edges + vectors).
    pub fn clear(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM documents; DELETE FROM edges; DELETE FROM titles; DELETE FROM index_meta;",
        )?;
        Ok(())
    }

    pub fn update_fingerprint(
        &self,
        path: &str,
        fp: &crate::document::types::Fingerprint,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE documents SET mtime_ns = ?2, size = ?3 WHERE path = ?1",
            rusqlite::params![path, fp.mtime_ns, fp.size as i64],
        )?;
        Ok(())
    }

    pub fn resolve_edges(&self) -> Result<()> {
        self.conn.execute("UPDATE edges SET status = CASE WHEN EXISTS (SELECT 1 FROM documents WHERE path = edges.to_path) THEN 'resolved' ELSE 'unresolved' END", [])?;
        Ok(())
    }

    pub fn sync_state(&self, path: &str) -> Result<(Option<String>, Option<String>, String)> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT sync_error, embedding_hash, stable_identity FROM documents WHERE path = ?1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn confirm_vectors(&self, path: &str, hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE documents SET embedding_hash = ?2, sync_error = NULL WHERE path = ?1",
            rusqlite::params![path, hash],
        )?;
        Ok(())
    }

    /// Persist the display title of a document for retrieval/listing.
    pub fn set_title(&self, path: &PathScope, title: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO titles (path, title) VALUES (?1, ?2)",
            rusqlite::params![path.0.as_str(), title],
        )?;
        Ok(())
    }

    /// Read the outgoing edges declared by one document (one hop).
    pub fn edges_for_path(&self, path: &str) -> Result<Vec<Edge>> {
        self.edges_like(
            "SELECT from_path, relation_type, to_path, section_source, status FROM edges WHERE from_path = ?1",
            path,
        )
    }

    /// Read the incoming edges pointing at one document (one hop, inverse).
    pub fn edges_to_path(&self, path: &str) -> Result<Vec<Edge>> {
        self.edges_like(
            "SELECT from_path, relation_type, to_path, section_source, status FROM edges WHERE to_path = ?1",
            path,
        )
    }

    fn edges_like(&self, sql: &str, path: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([path], |r| {
            Ok(Edge {
                from: PathScope(camino::Utf8PathBuf::from(r.get::<_, String>(0)?)),
                relation_type: r.get(1)?,
                to: PathScope(camino::Utf8PathBuf::from(r.get::<_, String>(2)?)),
                section_source: r.get(3)?,
                status: r
                    .get::<_, String>(4)?
                    .parse()
                    .expect("EdgeStatus FromStr is infallible"),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Display title of one document from the titles projection.
    pub fn title_for(&self, path: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT title FROM titles WHERE path = ?1", [path], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// All (path, title) pairs from the titles projection (exact-match leg).
    pub fn all_titles(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, title FROM titles")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Schema (declared as the authoritative definition)
// ---------------------------------------------------------------------------

/// SQL used to create the metadata schema. Kept as a constant so tests and the
/// `open` implementation share it.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS documents (
    path            TEXT PRIMARY KEY,
    content_hash    TEXT NOT NULL,
    mtime_ns        INTEGER NOT NULL,
    size            INTEGER NOT NULL,
    stable_identity TEXT NOT NULL DEFAULT '',
    sync_error      TEXT,
    frontmatter_json TEXT NOT NULL DEFAULT '{}'
    ,embedding_hash TEXT
);

CREATE TABLE IF NOT EXISTS edges (
    from_path      TEXT NOT NULL,
    relation_type  TEXT NOT NULL,
    to_path        TEXT NOT NULL,
    section_source TEXT NOT NULL DEFAULT '',
    status         TEXT NOT NULL DEFAULT 'unresolved',
    PRIMARY KEY (from_path, relation_type, to_path)
);

CREATE TABLE IF NOT EXISTS index_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS titles (
    path  TEXT PRIMARY KEY,
    title TEXT NOT NULL
);
"#;

impl EdgeStatus {
    /// `<->` string form stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeStatus::Resolved => "resolved",
            EdgeStatus::Unresolved => "unresolved",
        }
    }
}

impl std::str::FromStr for EdgeStatus {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "resolved" => Ok(EdgeStatus::Resolved),
            _ => Ok(EdgeStatus::Unresolved),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx_dir() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, path)
    }

    #[test]
    fn retrieval_format_mismatch_marks_full_rebuild() {
        let (_t, path) = idx_dir();
        // Fresh store has no format marker yet -> one rebuild round expected.
        let store = MetaStore::open(&path).unwrap();
        assert!(store.needs_rebuild);

        // Confirming the current format clears the flag for the next open.
        store.set_retrieval_format(RETRIEVAL_FORMAT).unwrap();
        assert!(!MetaStore::open(&path).unwrap().needs_rebuild);

        // A stale/different format (e.g. tokenizer change) forces rebuild.
        let store = MetaStore::open(&path).unwrap();
        store.set_retrieval_format("fts-simple-v0").unwrap();
        assert!(MetaStore::open(&path).unwrap().needs_rebuild);
    }
}
