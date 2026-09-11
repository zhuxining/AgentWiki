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

pub mod config;
pub mod document;
pub mod error;
pub mod governance;
pub mod graph;
pub mod model;
pub mod retrieval;
pub mod runtime;
pub mod search;
pub mod storage;
pub mod sync;

/// Compatibility re-export for callers migrating to `governance::validate`.
pub mod validate {
    pub use crate::governance::validate::*;
}

pub use model::*;

// Re-export the primary entry point used by both CLI and MCP composition roots.
pub use runtime::Runtime;
