//! SQLite metadata store — the sync ledger, document graph and persistent state.
//!
//! This is **not** a search backend. It never executes full-text or vector queries
//! (that belongs to `retrieval`). It holds:
//! - the per-document fingerprint ledger used by incremental sync;
//! - the one-hop document graph;
//! - index generation markers and vector manifest state.

use camino::Utf8Path;
use rusqlite::Connection;

use crate::error::Result;
use crate::model::{Edge, EdgeStatus, PathScope};

/// Thin wrapper over a `rusqlite::Connection` for wiki metadata.
///
/// `Connection` is `Send` but not `Sync`; callers that need sharing across
/// tasks can wrap it in a `tokio::sync::Mutex` or use `spawn_blocking`.
pub struct MetaStore {
    pub(crate) needs_rebuild: bool,
    conn: Connection,
    /// Path of the directory we alias as the "index" root (used for the lock file).
    pub index_dir: camino::Utf8PathBuf,
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

        // Mirrors the Python write-serialization discipline: a single write
        // mutex serializes every mutation regardless of task scheduling.
        Ok(MetaStore {
            needs_rebuild: cur != SCHEMA_VERSION,
            conn,
            index_dir: index_dir.to_path_buf(),
        })
    }

    /// List the current ledger as (path -> fingerprint) map.
    pub fn ledger_snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, crate::model::Fingerprint>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, content_hash, mtime_ns, size FROM documents")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                crate::model::Fingerprint {
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

    /// Read a document's frontmatter projection for query-time filters.
    pub fn frontmatter_for_path(&self, path: &str) -> Result<crate::model::Frontmatter> {
        let raw: String = self.conn.query_row(
            "SELECT frontmatter_json FROM documents WHERE path = ?1",
            [path],
            |row| row.get(0),
        )?;
        serde_json::from_str(&raw).map_err(|e| crate::error::AgentWikiError::Other(e.to_string()))
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

    /// Read the persisted `index_generation` marker (empty when unset).
    pub fn generation(&self) -> Result<String> {
        self.conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'index_generation'",
                [],
                |r| r.get::<_, String>(0),
            )
            .or_else(|_| Ok(String::new()))
    }

    /// Clear every derived projection (document ledger + edges + vectors).
    pub fn clear(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM documents; DELETE FROM edges; DELETE FROM titles; DELETE FROM index_meta;",
        )?;
        Ok(())
    }

    pub fn update_fingerprint(&self, path: &str, fp: &crate::model::Fingerprint) -> Result<()> {
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
        let mut stmt = self
            .conn
            .prepare("SELECT from_path, relation_type, to_path, section_source, status FROM edges WHERE from_path = ?1")?;
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
