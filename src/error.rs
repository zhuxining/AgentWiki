//! Unified library error type (`thiserror`-based).
//!
//! The convention mirrors the Python project's reliability contract:
//! - A single document failure must not abort a whole sync or validation round.
//! - Anything that would make retrieval "degraded but usable" is surfaced as a
//!   non-fatal diagnostic, not as a hard error.
//!
//! Application entry points (`main.rs`, `mcp.rs`) convert to `anyhow::Error`.

use camino::Utf8PathBuf;

/// Errors produced by the library.
///
/// The `Variant` column is descriptive:
/// - [`AgentWikiError::Config`] — invalid/absent configuration (hard error).
/// - [`AgentWikiError::Io`] — filesystem access failure (hard, but per-file
///   failures are caught and downgraded into diagnostics by `sync`).
/// - [`AgentWikiError::Parse`] — Markdown/YAML parse failure for one document.
/// - [`AgentWikiError::Storage`] — SQLite metadata store failure.
/// - [`AgentWikiError::Index`] — Tantivy index failure (rebuildable).
/// - [`AgentWikiError::Embedding`] — optional semantic leg unavailable; must
///   downgrade to keyword search, never break retrieval.
#[derive(Debug, thiserror::Error)]
pub enum AgentWikiError {
    #[error("invalid AgentWiki configuration: {0}")]
    Config(String),

    #[error("filesystem error at {path}: {source}")]
    Io {
        /// The offending path, when known.
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {message}")]
    Parse {
        /// Document-relative path that failed to parse.
        path: Utf8PathBuf,
        /// Human-readable reason.
        message: String,
    },

    #[error("metadata store (SQLite) error: {0}")]
    Storage(#[from] rusqlite::Error),

    #[error("search index (Tantivy) error: {0}")]
    Index(String),

    #[error("embedding unavailable: {0}")]
    Embedding(String),

    #[error("path escapes the wiki root: {0:?}")]
    PathOutsideRoot(Utf8PathBuf),

    #[error("unknown error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AgentWikiError>;

/// Build an [`AgentWikiError::Index`] from a Tantivy error.
///
/// Kept as a helper so `tantivy_svc` can map engine errors without depending on
/// this module's naming from each call site.
pub fn index_err(context: impl std::fmt::Display) -> AgentWikiError {
    AgentWikiError::Index(context.to_string())
}
