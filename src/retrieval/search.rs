//! Retrieval orchestration: fuse keyword, semantic and exact-match candidates
//! with reciprocal rank fusion (RRF, k=60, Cormack et al.), apply scope /
//! frontmatter filters and attach one-hop related documents.
//!
//! `run_query` is deliberately the only query entry point; it reads candidates
//! from [`crate::retrieval::LanceIndex`] and related documents from the
//! metadata store, and reports non-fatal degradation rather than failing the
//! whole request when a single source is unavailable.
//!
//! RRF is computed here (not via `lancedb::rerankers::RRFReranker`) because
//! the keyword and vector legs live in separate Lance tables, and the engine
//! reranker aligns results by table-local row ids, which are not comparable
//! across tables. The formula itself is the standard k=60 scoring.

use crate::document::types::PathScope;
use crate::error::Result;
use crate::projection::Projection;
use crate::retrieval::types::{
    ContextQuery, RankedSlice, RelatedDocument, SearchResult, SearchStrategy,
};

/// RRF constant (Cormack et al., 2009; k=60 near-optimal).
const RRF_K: f64 = 60.0;

/// Run a context retrieval against an assembled sync context.
///
/// Guarantees:
/// - never fails on a single-source problem; it surfaces in `degraded` instead;
/// - an absent or unavailable semantic leg downgrades to keyword only;
/// - exact filename/path matches are promoted and marked `exact`;
/// - related documents stop at one hop.
pub async fn run_query(ctx: &Projection, q: &ContextQuery) -> Result<SearchResult> {
    let mut degraded = Vec::new();

    // Empty queries are served from the ledger's real mtime ordering.
    let mut recent: Vec<RankedSlice> = Vec::new();
    if q.is_recent_request() {
        let scope = q.scope.clone();
        let limit = q.limit.clamp(1, 20);
        let paths = ctx
            .meta
            .with(move |store| store.recent_paths(&scope, limit))
            .await
            .unwrap_or_else(|e| {
                degraded.push(format!("recent metadata unavailable: {e}"));
                Vec::new()
            });
        for path in paths {
            recent.extend(ctx.index.slices_for_path(&path).await.unwrap_or_default());
        }
    }

    // Non-empty queries: collect candidates per leg, then RRF-fuse.
    let mut keyword: Vec<RankedSlice> = Vec::new();
    let mut semantic: Vec<RankedSlice> = Vec::new();
    let mut exact: Vec<RankedSlice> = Vec::new();
    if !q.is_recent_request() {
        let semantic_expected = ctx.index.semantic_available();
        keyword = ctx
            .index
            .search(q, semantic_expected)
            .await
            .unwrap_or_else(|e| {
                degraded.push(format!("search index unavailable: {e}"));
                Vec::new()
            });
        exact = exact_matches(ctx, q).await;
        if let Some(embedder) = &ctx.embedder {
            let embedder = embedder.clone();
            let input = q.query.clone();
            let embedded = tokio::task::spawn_blocking(move || match embedder.lock() {
                Ok(mut embedder) => embedder
                    .embed(vec![input])
                    .map_err(|error| format!("embedding unavailable: {error}")),
                Err(error) => Err(format!("embedding lock unavailable: {error}")),
            })
            .await
            .unwrap_or_else(|error| Err(format!("embedding task failed: {error}")));
            match embedded {
                Ok(vectors) if !vectors.is_empty() => {
                    match ctx.index.vector_search(q, &vectors[0]).await {
                        Ok(mut found) => semantic.append(&mut found),
                        Err(error) => degraded.push(format!("vector search unavailable: {error}")),
                    }
                }
                Ok(_) => degraded.push("embedding returned no vector".into()),
                Err(error) => degraded.push(error),
            }
        }
    }

    // Fuse each leg's ranking with RRF; `exact` matches are ranked first.
    let mut rrf: std::collections::HashMap<String, (f64, Vec<String>)> =
        std::collections::HashMap::new();
    let mut add_leg = |leg: &[RankedSlice], source: &str| {
        for (rank, hit) in leg.iter().enumerate() {
            let entry = rrf
                .entry(hit.slice.chunk_id.clone())
                .or_insert_with(|| (0.0, Vec::new()));
            entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
            if !entry.1.contains(&source.to_string()) {
                entry.1.push(source.to_string());
            }
        }
    };
    add_leg(&exact, "exact");
    add_leg(&keyword, "keyword");
    add_leg(&semantic, "semantic");

    let mut candidates: Vec<RankedSlice> = if q.is_recent_request() {
        recent
    } else {
        // Rebuild ranked slices in RRF order, keeping the first slice text.
        let mut by_id: std::collections::HashMap<String, &RankedSlice> =
            std::collections::HashMap::new();
        for hit in exact.iter().chain(keyword.iter()).chain(semantic.iter()) {
            by_id.entry(hit.slice.chunk_id.clone()).or_insert(hit);
        }
        let mut out: Vec<RankedSlice> = rrf
            .into_iter()
            .map(|(chunk_id, (score, sources))| {
                let hit = *by_id.get(&chunk_id).expect("rrf key came from a leg");
                let mut ranked = hit.clone();
                ranked.score = score;
                ranked.sources = sources;
                ranked
            })
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out
    };

    if !q.tags.is_empty() || !q.note_types.is_empty() || !q.metadata_filters.is_empty() {
        let mut filtered = Vec::with_capacity(candidates.len());
        for hit in candidates {
            let path = hit.slice.path.0.to_string();
            let Ok(frontmatter) = ctx
                .meta
                .with(move |store| store.frontmatter_for_path(&path))
                .await
            else {
                continue;
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
            if tags_match && types_match && metadata_match {
                filtered.push(hit);
            }
        }
        candidates = filtered;
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
        let rel = related_for(ctx, &path_str).await;
        if !rel.is_empty() {
            related.extend(rel);
            break; // only enrich the primary hit with its relations
        }
    }

    // Cap candidates at the requested limit.
    candidates.truncate(q.limit);

    for hit in &mut candidates {
        let path = hit.slice.path.0.to_string();
        let lookup_path = path.clone();
        let (fingerprint, frontmatter) = ctx
            .meta
            .with(move |store| {
                Ok((
                    store.ledger_fingerprint(&lookup_path)?,
                    store.frontmatter_for_path(&lookup_path)?,
                ))
            })
            .await?;
        hit.filename = display_name(&path);
        hit.modified_at_ns = fingerprint.mtime_ns;
        hit.frontmatter = frontmatter;
    }

    let strategy = if q.is_recent_request() {
        SearchStrategy::Recent
    } else if candidates
        .iter()
        .any(|hit| hit.sources.iter().any(|source| source == "semantic"))
    {
        SearchStrategy::Hybrid
    } else {
        SearchStrategy::Keyword
    };

    Ok(SearchResult {
        strategy,
        slices: candidates,
        related,
        degraded,
    })
}

/// Exact-match candidates: indexed paths whose filename stem or relative path
/// equals the query text (case-insensitive, trimmed). They rank at the top of
/// the RRF fusion and carry the `exact` source.
async fn exact_matches(ctx: &Projection, q: &ContextQuery) -> Vec<RankedSlice> {
    if q.query.trim().is_empty() || q.query.chars().count() > 64 {
        return Vec::new();
    }
    let wanted = q.query.trim().to_lowercase();
    let Ok(paths) = ctx.meta.with(|store| store.all_paths()).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in paths {
        let filename = display_name(&path).to_lowercase();
        if (path.to_lowercase() == wanted || filename == wanted)
            && let Ok(slices) = ctx.index.slices_for_path(&path).await
        {
            out.extend(slices.into_iter().take(q.limit.clamp(1, 20)));
        }
    }
    out
}

/// One-hop related documents for a path, read from the metadata edge store
/// (outgoing first, then incoming, at most five). Best-effort: misses degrade
/// to an empty list, never an error.
async fn related_for(ctx: &Projection, path: &str) -> Vec<RelatedDocument> {
    let mut out = Vec::new();
    let path_owned = path.to_owned();
    let outgoing = ctx
        .meta
        .with(move |store| store.edges_for_path(&path_owned))
        .await
        .unwrap_or_default();
    let path_owned = path.to_owned();
    let incoming = ctx
        .meta
        .with(move |store| store.edges_to_path(&path_owned))
        .await
        .unwrap_or_default();
    for edge in outgoing.into_iter().chain(incoming) {
        let from = edge.from.0.as_str();
        let direction = if from == path {
            crate::retrieval::types::RelationDirection::Outgoing
        } else {
            crate::retrieval::types::RelationDirection::Incoming
        };
        let target = edge.to.0.to_string();
        let title = display_name(&target);
        let context = section_context(ctx, &edge.from, &edge.section_source).await;
        out.push(RelatedDocument {
            path: edge.to,
            filename: title,
            relation_type: edge.relation_type,
            direction,
            status: edge.status,
            section_source: edge.section_source,
            context,
        });
        if out.len() == 5 {
            break;
        }
    }
    out
}

fn display_name(path: &str) -> String {
    camino::Utf8Path::new(path)
        .file_stem()
        .unwrap_or(path)
        .to_owned()
}

/// Recover the source text of the section that declared a relation, from the
/// indexed slices of the declaring document (bounded; best-effort).
async fn section_context(ctx: &Projection, from: &PathScope, section: &str) -> String {
    let Ok(slices) = ctx.index.slices_for_path(from.0.as_str()).await else {
        return String::new();
    };
    slices
        .iter()
        .find(|hit| hit.slice.section == section)
        .or_else(|| slices.first())
        .map(|hit| hit.slice.content.chars().take(120).collect::<String>())
        .unwrap_or_default()
}
