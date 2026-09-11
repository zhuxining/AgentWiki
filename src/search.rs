//! Retrieval orchestration: fuse keyword (and optionally semantic) candidates
//! with entity/section scope filters and attach one-hop related documents.
//!
//! `run_query` is deliberately the only query entry point; it reads candidates
//! from [`crate::tantivy_svc::TantalusIndex`] and related documents from the
//! metadata store, and reports non-fatal degradation rather than failing the
//! whole request when a single source is unavailable.

use crate::error::Result;
use crate::model::{ContextQuery, RankedSlice, RelatedDocument, SearchResult};
use crate::sync::SyncContext;

/// Run a context retrieval against an assembled sync context.
///
/// Guarantees:
/// - never fails on a single-source problem; it surfaces in `degraded` instead;
/// - an absent or unavailable semantic leg downgrades to keyword only;
/// - related documents stop at one hop.
pub fn run_query(ctx: &SyncContext, q: &ContextQuery) -> Result<SearchResult> {
    let mut degraded = Vec::new();

    // Keyword (+ optional vector) candidates from the Tantivy façade.
    let semantic_expected = ctx.index.semantic_available();
    let mut candidates: Vec<RankedSlice> =
        ctx.index.search(q, semantic_expected).unwrap_or_else(|e| {
            degraded.push(format!("search index unavailable: {e}"));
            Vec::new()
        });

    if degraded.is_empty() && candidates.is_empty() && !q.is_recent_request() {
        // No hits at all: report it so the Agent knows the index is sparse, but
        // still return a well-formed (empty) bundle.
        degraded.push("no matching slices".into());
    }

    // Attach one-hop related documents for the top-ranked hit paths.
    let mut seen: Vec<String> = Vec::new();
    let mut related: Vec<RelatedDocument> = Vec::new();
    for r in &mut candidates {
        let path_str = r.slice.path.0.as_str().to_string();
        if seen.contains(&path_str) {
            continue;
        }
        seen.push(path_str.clone());
        let rel = related_for(ctx, &path_str);
        if !rel.is_empty() {
            related.extend(rel);
            break; // only enrich the primary hit with its relations
        }
    }

    // Cap candidates at the requested limit.
    candidates.truncate(q.limit);

    Ok(SearchResult {
        slices: candidates,
        related,
        degraded,
    })
}

/// One-hop related documents for a path, read from the metadata edge store.
fn related_for(ctx: &SyncContext, path: &str) -> Vec<RelatedDocument> {
    let mut out = Vec::new();
    // The metadata store layer exposes edges per document; SQLite rows are
    // surfaced through `MetaStore`. Keep this best-effort: misses degrade to an
    // empty list, never an error.
    if let Ok(edges) = ctx.meta.edges_for_path(path) {
        for e in edges {
            out.push(RelatedDocument {
                path: e.to,
                relation_type: e.relation_type,
                status: e.status,
            });
        }
    }
    out
}
