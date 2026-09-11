//! Retrieval orchestration: fuse keyword (and optionally semantic) candidates
//! with entity/section scope filters and attach one-hop related documents.
//!
//! `run_query` is deliberately the only query entry point; it reads candidates
//! from [`crate::retrieval::LanceIndex`] and related documents from the
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

    // Empty queries are served from the ledger's real mtime ordering.
    let mut candidates: Vec<RankedSlice> = if q.is_recent_request() {
        let paths = ctx
            .meta
            .recent_paths(&q.scope, q.limit.clamp(1, 20))
            .unwrap_or_else(|e| {
                degraded.push(format!("recent metadata unavailable: {e}"));
                Vec::new()
            });
        paths
            .iter()
            .flat_map(|path| ctx.index.slices_for_path(path).unwrap_or_default())
            .collect()
    } else {
        let semantic_expected = ctx.index.semantic_available();
        ctx.index.search(q, semantic_expected).unwrap_or_else(|e| {
            degraded.push(format!("search index unavailable: {e}"));
            Vec::new()
        })
    };

    if !q.is_recent_request()
        && let Some(embedder) = &ctx.embedder
    {
        match embedder.lock() {
            Ok(mut embedder) => match embedder.embed(vec![q.query.clone()]) {
                Ok(vectors) if !vectors.is_empty() => {
                    match ctx.index.vector_search(q, &vectors[0]) {
                        Ok(mut semantic) => candidates.append(&mut semantic),
                        Err(error) => degraded.push(format!("vector search unavailable: {error}")),
                    }
                }
                Ok(_) => degraded.push("embedding returned no vector".into()),
                Err(e) => degraded.push(format!("embedding unavailable: {e}")),
            },
            Err(e) => degraded.push(format!("embedding lock unavailable: {e}")),
        }
    }

    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut fused: Vec<RankedSlice> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if let Some(existing) = fused
            .iter_mut()
            .find(|hit| hit.slice.chunk_id == candidate.slice.chunk_id)
        {
            existing.score = existing.score.max(candidate.score);
            for source in candidate.sources {
                if !existing.sources.contains(&source) {
                    existing.sources.push(source);
                }
            }
        } else {
            fused.push(candidate);
        }
    }
    let mut candidates = fused;

    if !q.tags.is_empty() || !q.note_types.is_empty() || !q.metadata_filters.is_empty() {
        candidates.retain(|hit| {
            let Ok(frontmatter) = ctx.meta.frontmatter_for_path(hit.slice.path.0.as_str()) else {
                return false;
            };
            let tags_match = q.tags.iter().all(|wanted| {
                frontmatter
                    .get("tags")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(wanted)))
            });
            let types_match = q.note_types.is_empty()
                || frontmatter
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| q.note_types.iter().any(|wanted| wanted == kind));
            let metadata_match = q
                .metadata_filters
                .iter()
                .all(|(key, expected)| frontmatter.get(key) == Some(expected));
            tags_match && types_match && metadata_match
        });
    }

    let mut per_document = std::collections::HashMap::new();
    candidates.retain(|hit| {
        let count = per_document.entry(hit.slice.path.clone()).or_insert(0usize);
        *count += 1;
        *count <= 2
    });

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
        for e in edges.into_iter().take(5) {
            out.push(RelatedDocument {
                path: e.to,
                relation_type: e.relation_type,
                status: e.status,
            });
        }
    }
    out
}
