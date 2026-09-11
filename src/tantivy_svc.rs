//! Search-engine façade over Tantivy.
//!
//! Owns the Tantivy schema for wiki slices and exposes two operations:
//! write (index/delete one document's slices) and query (BM25 + filters + an
//! optional vector leg). This module is deliberately the *only* place that
//! touches the search engine — `sync` and `search` depend on its small API,
//! never on Tantivy types directly.

use crate::error::Result;
use crate::model::{ContextQuery, PathScope, RankedSlice, Slice};

/// An open Tantivy index plus its reader.
pub struct TantalusIndex {
    // Fields are populated when the Tantivy wiring is completed in the search
    // milestone. Kept as a struct now so the API surface is stable.
    path: camino::Utf8PathBuf,
}

impl TantalusIndex {
    /// Create (or open-and-scheme-check) an index rooted at `index_dir`.
    pub fn open(index_dir: &camino::Utf8Path, _vector_dims: Option<usize>) -> Result<Self> {
        let path = index_dir.join("tantivy");
        std::fs::create_dir_all(&path).map_err(|e| crate::error::AgentWikiError::Io {
            path: path.clone(),
            source: e,
        })?;
        // TODO(search-milestone): open / create the Tantivy index, apply schema.
        Ok(TantalusIndex { path })
    }

    /// Drop all projected slices and reopen the index directory.
    ///
    /// Used by `sync::rebuild` so the projection can be fully reconstructed
    /// from Markdown. Tantivy wiring is a stub today, so this only resets the
    /// directory; swap in a real index reset when the engine is wired.
    pub fn reset(&self) -> Result<()> {
        std::fs::remove_dir_all(&self.path).map_err(|e| crate::error::AgentWikiError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        std::fs::create_dir_all(&self.path).map_err(|e| crate::error::AgentWikiError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        Ok(())
    }

    /// Replace the projected slices for one document (delete-then-add by path).
    ///
    /// This is the write path invoked from `sync::replace_document`.
    pub fn replace_slices(&self, _path: &PathScope, _slices: &[Slice]) -> Result<()> {
        // Stub until the search milestone.
        Ok(())
    }

    /// Delete every slice belonging to `path` from the index.
    pub fn delete_path(&self, _path: &PathScope) -> Result<()> {
        Ok(())
    }

    /// Run a keyword(+filters, optionally vector) query and return ranked
    /// candidate slices (already deduplicated and limited by the caller).
    pub fn search(
        &self,
        _query: &ContextQuery,
        _already_has_pending_vector: bool,
    ) -> Result<Vec<RankedSlice>> {
        Ok(Vec::new())
    }

    /// Report vector availability so `search` can degrade cleanly.
    pub fn semantic_available(&self) -> bool {
        false // stub; becomes true when the vector leg is wired + model loads
    }
}
