//! Explicit resource assembly, index lifecycle and the application facade.
//!
//! `Runtime` is the composition root on which both the CLI (`main.rs`) and the
//! MCP server (`mcp.rs`) sit. It owns the wiki root plus a [`SyncContext`], and
//! exposes the three operations the entry points need: sync, rebuild, and query
//! (also validate). Generally library errors surface as [`crate::error::AgentWikiError`]
//! and are converted to `anyhow` at the boundary.

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::Result;
use crate::model::{ContextQuery, SearchResult, SyncReport};
use crate::sync::SyncContext;

/// A fully assembled, ready-to-run wiki handle.
pub struct Runtime {
    /// Wiki root (absolute). Markdown here is the source of truth.
    pub root: Utf8PathBuf,
    /// Directory holding the Tantivy index and the SQLite metadata store.
    pub index_dir: Utf8PathBuf,
    sync: SyncContext,
}

impl Runtime {
    /// Assemble a runtime from a wiki root and an index directory.
    ///
    /// Creates the index directory if needed, opens (or creates) the metadata
    /// store and the search index, and prepares an AGENTWIKI.md default rule
    /// seed when the wiki has none (never overwrites an existing one).
    pub fn assemble(root: &Utf8Path, index_dir: &Utf8Path) -> Result<Self> {
        let root = root.to_path_buf();
        let index_dir = index_dir.to_path_buf();
        seed_agentwiki(&root)?;
        let sync = SyncContext::assemble(&root, &index_dir)?;
        Ok(Runtime {
            root,
            index_dir,
            sync,
        })
    }

    /// Incrementally reconcile the archive with the derived projections.
    pub fn ensure_fresh(&mut self) -> Result<SyncReport> {
        self.sync.ensure_fresh()
    }

    /// Wipe the derived projections and rebuild them from Markdown.
    pub fn rebuild(&mut self) -> Result<SyncReport> {
        self.sync.rebuild()
    }

    /// Run a context retrieval (keyword + optional semantic, with ranking).
    ///
    /// Keyword may be empty to request the most recent documents.
    pub fn query(&self, q: &ContextQuery) -> Result<SearchResult> {
        crate::search::run_query(&self.sync, q)
    }

    /// Validate one document or the whole wiki. Reports only, never rewrites.
    pub fn validate(
        &self,
        scope: Option<&crate::model::PathScope>,
    ) -> Result<Vec<crate::model::Issue>> {
        crate::validate::validate_wiki(&self.root, scope)
    }
}

/// Ensure `root/AGENTWIKI.md` exists, seeding a default rule file when absent.
///
/// Never overwrites an existing file. This is the only startup write that does
/// not participate in index transactions.
fn seed_agentwiki(root: &Utf8Path) -> Result<()> {
    let path = root.join("AGENTWIKI.md");
    if path.exists() {
        return Ok(());
    }
    let template = crate::markdown::DEFAULT_AGENTWIKI;
    std::fs::write(&path, template)
        .map_err(|e| crate::error::AgentWikiError::Io { path, source: e })
}
