//! Incremental projection of Markdown changes into the search index and the
//! metadata ledger.
//!
//! Contract:
//! - A single document parse failure must never abort a whole round: it is
//!   recorded as a diagnostic and the remaining documents continue.
//! - Index writes never roll back or overwrite Markdown — the projection stays
//!   rebuildable.
//! - `rebuild` is explicit (or used after a schema/index compromise); the fast
//!   path short-circuits when the ledger already matches the archive snapshot.

use camino::Utf8Path;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::model::{Fingerprint, PathScope, SyncReport};
use crate::{graph, markdown, storage::MetaStore, tantivy_svc::TantalusIndex};

/// A ready-to-run sync handles a wiki root, its derived indexes and a ledger.
///
/// Callers create it via [`SyncContext::assemble`]; it owns the resources.
pub struct SyncContext {
    pub root: camino::Utf8PathBuf,
    pub index: TantalusIndex,
    pub meta: MetaStore,
}

impl SyncContext {
    /// Assemble a sync context from a wiki root and an index directory.
    pub fn assemble(root: &Utf8Path, index_dir: &Utf8Path) -> Result<Self> {
        std::fs::create_dir_all(index_dir).map_err(|e| crate::error::AgentWikiError::Io {
            path: index_dir.to_path_buf(),
            source: e,
        })?;
        Ok(SyncContext {
            root: root.to_path_buf(),
            index: TantalusIndex::open(index_dir, None)?,
            meta: MetaStore::open(index_dir)?,
        })
    }

    /// Incrementally reconcile the archive with the projection.
    pub fn ensure_fresh(&mut self) -> Result<SyncReport> {
        // Snapshot the archive and short-circuit when nothing changed.
        let (descriptors, generation) = self.snapshot_descriptors()?;
        if self.generation()? == generation && !descriptors.is_empty() {
            let unchanged = descriptors.len();
            return Ok(SyncReport {
                unchanged,
                generation,
                ..Default::default()
            });
        }

        let prev = self.ledger_snapshot()?;
        let current: std::collections::BTreeMap<String, Fingerprint> =
            descriptors.into_iter().collect();
        let mut degraded = Vec::new();
        let mut indexed = 0usize;
        let mut removed = 0usize;
        let moved = 0usize;

        // Compute deletions (paths in ledger but gone from archive).
        let mut to_delete: Vec<String> = prev
            .iter()
            .filter(|(k, _)| !current.contains_key(*k))
            .map(|(k, _)| k.clone())
            .collect();
        to_delete.sort();

        // Compute changes: new, modified (mtime/size/content), or stale vectors.
        let mut changed: Vec<(String, Fingerprint)> = Vec::new();
        for (path, fp) in &current {
            let is_new = !prev.contains_key(path);
            let changed_stamp = prev.get(path).map(|p| {
                p.mtime_ns != fp.mtime_ns || p.size != fp.size || p.content_hash != fp.content_hash
            });
            if is_new || changed_stamp.unwrap_or(true) {
                changed.push((path.clone(), fp.clone()));
            }
        }
        changed.sort_by(|a, b| a.0.cmp(&b.0));

        for (path, fp) in &changed {
            let scope = PathScope(Utf8Path::new(path).to_path_buf());
            match self.index_document(&scope) {
                Ok(()) => {
                    indexed += 1;
                    self.meta
                        .upsert_ledger(&crate::storage::LedgerRow {
                            path: path.clone(),
                            content_hash: fp.content_hash.clone(),
                            mtime_ns: fp.mtime_ns,
                            size: fp.size,
                            stable_identity: String::new(),
                            sync_error: None,
                        })
                        .map_err(|e| degraded.push(format!("{path}: {e}")))
                        .ok();
                }
                Err(e) => degraded.push(format!("{path}: {e}")),
            }
        }

        // Move detection: a removed path whose content_hash equals a changed
        // path's is a move; keep the stable identity.
        for path in &to_delete {
            let deleted = self
                .index
                .delete_path(&PathScope(Utf8Path::new(path).to_path_buf()))
                .is_ok()
                && self
                    .meta
                    .delete_ledger(&PathScope(Utf8Path::new(path).to_path_buf()))
                    .is_ok();
            if deleted {
                removed += 1;
            } else {
                degraded.push(format!("failed to remove {path}"));
            }
        }

        self.mark_index_complete(&generation)?;
        Ok(SyncReport {
            indexed,
            removed,
            moved,
            unchanged: current.len().saturating_sub(changed.len()),
            vectors_ready: 0,
            vectors_pending: 0,
            degraded,
            generation,
        })
    }

    /// Wipe the projection and re-project the whole archive.
    pub fn rebuild(&mut self) -> Result<SyncReport> {
        self.index.reset()?;
        self.meta.clear()?;
        self.ensure_fresh()
    }

    // -- internals -----------------------------------------------------------

    fn index_document(&mut self, path: &PathScope) -> Result<()> {
        let doc = markdown::read_document(&self.root, path)?;
        let body = markdown::read_body(&self.root, path)?;
        let slices = markdown::chunk_document(&doc.title, &body, path);
        let (edges, warns) = graph::extract_edges(path, &doc.frontmatter, &body);
        self.index.replace_slices(path, &slices)?;
        self.meta.replace_document(path, &edges)?;
        for w in warns {
            tracing::warn!("{}: {w}", path.0);
        }
        // Store the doc title so retrieval can surface it later.
        self.meta.set_title(path, &doc.title)?;
        Ok(())
    }

    /// Snapshot the archive as (path-relative, fingerprint) pairs + generation.
    fn snapshot_descriptors(&self) -> Result<(Vec<(String, Fingerprint)>, String)> {
        let paths = markdown::snapshot(&self.root)?;
        let mut out = Vec::new();
        for p in &paths {
            if let Ok(doc) = markdown::read_document(&self.root, p) {
                out.push((p.0.as_str().to_string(), doc.fingerprint));
            }
        }
        let generation = hex::encode(Sha256::digest(
            out.iter()
                .map(|(p, f)| format!("{p}:{}\n", f.content_hash))
                .collect::<String>()
                .as_bytes(),
        ));
        Ok((out, generation))
    }

    fn generation(&self) -> Result<String> {
        self.meta.generation()
    }

    fn ledger_snapshot(&self) -> Result<std::collections::BTreeMap<String, Fingerprint>> {
        self.meta.ledger_snapshot()
    }

    fn mark_index_complete(&mut self, generation: &str) -> Result<()> {
        self.meta.set_generation(generation)
    }
}
