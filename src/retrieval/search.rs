//! Query-faithful retrieval orchestration.

use std::collections::HashSet;

use crate::document::types::{PathScope, RetrievalUnitKind};
use crate::error::Result;
use crate::projection::Projection;
use crate::retrieval::types::{
    ContextQuery, RankedSlice, RelatedDocument, SearchOrder, SearchResult, SearchStrategy,
};

pub async fn run_query(ctx: &Projection, q: &ContextQuery) -> Result<SearchResult> {
    let mut degraded = Vec::new();
    if q.is_recent_request() {
        return recent_result(ctx, q, &mut degraded).await;
    }

    let query_vector = embed_query(ctx, q, &mut degraded).await;
    let mut documents = retrieve_kind(
        ctx,
        q,
        RetrievalUnitKind::Document,
        q.document_limit,
        query_vector.as_deref(),
        true,
        &mut degraded,
    )
    .await;
    let mut fragments = retrieve_kind(
        ctx,
        q,
        RetrievalUnitKind::Fragment,
        q.fragment_limit,
        query_vector.as_deref(),
        false,
        &mut degraded,
    )
    .await;

    hydrate(ctx, &mut documents).await?;
    hydrate(ctx, &mut fragments).await?;
    let related = if q.include_relations {
        if let Some(primary) = documents.first().or_else(|| fragments.first()) {
            related_for(ctx, primary.slice.path.0.as_str()).await
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let has_semantic = documents
        .iter()
        .chain(&fragments)
        .any(|hit| hit.sources.iter().any(|source| source == "semantic"));
    Ok(SearchResult {
        strategy: if has_semantic {
            SearchStrategy::Hybrid
        } else {
            SearchStrategy::Keyword
        },
        documents,
        fragments,
        related,
        degraded,
    })
}

async fn recent_result(
    ctx: &Projection,
    q: &ContextQuery,
    degraded: &mut Vec<String>,
) -> Result<SearchResult> {
    let limit = q.document_limit.clamp(1, 20);
    let mut documents = ctx.index.browse_documents(q, limit).await?;
    hydrate(ctx, &mut documents).await?;
    let related = if q.include_relations {
        if let Some(primary) = documents.first() {
            related_for(ctx, primary.slice.path.0.as_str()).await
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    Ok(SearchResult {
        strategy: SearchStrategy::Recent,
        documents,
        fragments: Vec::new(),
        related,
        degraded: std::mem::take(degraded),
    })
}

async fn embed_query(
    ctx: &Projection,
    q: &ContextQuery,
    degraded: &mut Vec<String>,
) -> Option<Vec<f32>> {
    let Some(embedder) = &ctx.embedder else {
        return None;
    };
    if q.query.trim().is_empty() {
        return None;
    }
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
        Ok(mut vectors) if !vectors.is_empty() => Some(vectors.remove(0)),
        Ok(_) => {
            degraded.push("embedding returned no vector".into());
            None
        }
        Err(error) => {
            degraded.push(error);
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn retrieve_kind(
    ctx: &Projection,
    q: &ContextQuery,
    kind: RetrievalUnitKind,
    result_limit: usize,
    query_vector: Option<&[f32]>,
    include_exact: bool,
    degraded: &mut Vec<String>,
) -> Vec<RankedSlice> {
    let candidate_limit = result_limit.saturating_mul(5).clamp(20, 100);
    let keyword_text = match q.keyword_mode {
        crate::retrieval::types::KeywordMode::All => q.keywords.join(" AND "),
        crate::retrieval::types::KeywordMode::Any => q.keywords.join(" OR "),
    };
    let text = if q.keywords.is_empty() {
        q.query.clone()
    } else if q.query.trim().is_empty() {
        keyword_text
    } else {
        format!("{} {}", q.query, keyword_text)
    };
    let mut fused = if let Some(vector) = query_vector {
        match ctx
            .index
            .hybrid_search(q, &text, vector, kind, candidate_limit)
            .await
        {
            Ok(hits) => hits,
            Err(error) => {
                degraded.push(format!("hybrid search unavailable: {error}"));
                ctx.index
                    .search(q, &text, kind, candidate_limit, "keyword")
                    .await
                    .unwrap_or_default()
            }
        }
    } else {
        match ctx
            .index
            .search(q, &text, kind, candidate_limit, "keyword")
            .await
        {
            Ok(hits) => hits,
            Err(error) => {
                degraded.push(format!("keyword search unavailable: {error}"));
                Vec::new()
            }
        }
    };
    if include_exact {
        let exact = exact_matches(ctx, q).await;
        let mut seen = exact
            .iter()
            .map(|h| h.slice.chunk_id.clone())
            .collect::<HashSet<_>>();
        let mut prioritized = exact;
        prioritized.extend(
            fused
                .into_iter()
                .filter(|h| seen.insert(h.slice.chunk_id.clone())),
        );
        fused = prioritized;
    }
    if q.order == SearchOrder::ModifiedDesc {
        fused.sort_by(|a, b| {
            b.slice
                .modified_at_ns
                .cmp(&a.slice.modified_at_ns)
                .then_with(|| b.score.total_cmp(&a.score))
        });
    }
    fused.truncate(result_limit.clamp(1, 20));
    fused
}

async fn exact_matches(ctx: &Projection, q: &ContextQuery) -> Vec<RankedSlice> {
    if q.query.trim().is_empty() || q.query.chars().count() > 128 {
        return Vec::new();
    }
    ctx.index
        .exact_documents(q, q.query.trim())
        .await
        .unwrap_or_default()
}

async fn hydrate(ctx: &Projection, hits: &mut [RankedSlice]) -> Result<()> {
    let paths = hits
        .iter()
        .filter(|hit| hit.slice.frontmatter.is_empty())
        .map(|hit| hit.slice.path.0.to_string())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let frontmatter = ctx.index.frontmatter_for_paths(&paths).await?;
    for hit in hits {
        let path = hit.slice.path.0.to_string();
        hit.frontmatter = if hit.slice.frontmatter.is_empty() {
            frontmatter.get(&path).cloned().unwrap_or_default()
        } else {
            hit.slice.frontmatter.clone()
        };
        hit.filename = display_name(&path);
        hit.modified_at_ns = hit.slice.modified_at_ns;
    }
    Ok(())
}

async fn related_for(ctx: &Projection, path: &str) -> Vec<RelatedDocument> {
    let edges = ctx.index.relations_for_path(path).await.unwrap_or_default();
    let mut out = Vec::new();
    for edge in edges {
        let direction = if edge.from.0.as_str() == path {
            crate::retrieval::types::RelationDirection::Outgoing
        } else {
            crate::retrieval::types::RelationDirection::Incoming
        };
        let related_path = if direction == crate::retrieval::types::RelationDirection::Outgoing {
            edge.to.clone()
        } else {
            edge.from.clone()
        };
        let context = section_context(ctx, &edge.from, &edge.section_source).await;
        out.push(RelatedDocument {
            filename: display_name(related_path.0.as_str()),
            path: related_path,
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

async fn section_context(ctx: &Projection, from: &PathScope, section: &str) -> String {
    let Ok(slices) = ctx.index.slices_for_path(from.0.as_str()).await else {
        return String::new();
    };
    slices
        .iter()
        .filter(|hit| hit.slice.unit_kind == RetrievalUnitKind::Fragment)
        .find(|hit| hit.slice.section == section)
        .or_else(|| {
            slices
                .iter()
                .find(|hit| hit.slice.unit_kind == RetrievalUnitKind::Fragment)
        })
        .map(|hit| hit.slice.content.chars().take(120).collect())
        .unwrap_or_default()
}
