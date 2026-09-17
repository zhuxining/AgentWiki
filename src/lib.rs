//! AgentWiki — local-first Markdown knowledge base context retrieval.
//!
//! Reads a directory of Markdown files (the "wiki"), projects an incremental
//! search index (keywords + optional vectors) into an embedded LanceDB engine
//! plus a small metadata store (`rusqlite`) for the sync ledger, document
//! graph and persistent state, and exposes retrieval/governance through a CLI
//! and an MCP server.
//!
//! Notable constraints:
//! - Markdown files are the source of truth. All indexes are derived and must
//!   be fully rebuildable from the wiki.
//! - The SQLite store is metadata-only (ledger / graph / state). It never does
//!   full-text search or vector search — that belongs to the retrieval backend.
//! - Domain types must stay dependency-free of third-party I/O crates.

#![forbid(unsafe_code)]

mod app;
pub mod config;
mod document;
pub mod error;
mod governance;
mod projection;
mod retrieval;

pub use app::{AgentWiki, OpenOptions};
pub use document::types::PathScope;
pub use governance::types::{
    Issue, RulesRequest, RulesResult, Severity, ValidationRequest, ValidationResult,
    ValidationScope,
};
pub use projection::types::SyncReport;
pub use retrieval::types::{ContextQuery, SearchResult};
