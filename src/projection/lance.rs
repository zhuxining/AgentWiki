//! LanceDB projection boundary.
//!
//! Markdown rows, relations and synchronization fingerprints intentionally
//! live in one Lance table.  SQLite is not part of the projection anymore.

use crate::document::types::{
    Edge, EdgeStatus, Fingerprint, Frontmatter, PathScope, RetrievalUnitKind, Slice,
};
use crate::error::{AgentWikiError, Result};
use crate::retrieval::types::{ContextQuery, RankedSlice};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder, ListBuilder, StringBuilder};
use arrow_array::{
    Array, Int32Array, Int64Array, ListArray, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use camino::Utf8Path;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lance_index::scalar::inverted::query::{BooleanQuery, FtsQuery, MatchQuery, Occur};
use lancedb::index::Index;
use lancedb::index::scalar::{
    BTreeIndexBuilder, BitmapIndexBuilder, FtsIndexBuilder, LabelListIndexBuilder,
};
use lancedb::query::{ColumnOrdering, ExecutableQuery, QueryBase, Select};
use lancedb::rerankers::rrf::RRFReranker;
use lancedb::{Connection, Table, connect};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;

const TABLE: &str = "wiki_rows";
const DIMS: usize = 512;

pub struct LanceIndex {
    table: Table,
    vector_dims: Option<usize>,
}

impl LanceIndex {
    pub async fn open(index_dir: &Utf8Path, vector_dims: Option<usize>) -> Result<Self> {
        tokio::fs::create_dir_all(index_dir)
            .await
            .map_err(|source| AgentWikiError::Io {
                path: index_dir.into(),
                source,
            })?;
        let db = connect(index_dir.as_str())
            .read_consistency_interval(std::time::Duration::ZERO)
            .execute()
            .await
            .map_err(index_err)?;
        let table = open_or_create(&db, vector_dims).await.map_err(index_err)?;
        Ok(Self { table, vector_dims })
    }

    pub async fn reset(&self) -> Result<()> {
        self.table
            .delete("true")
            .await
            .map(|_| ())
            .map_err(index_err)
    }

    /// Return document fingerprints stored in the Lance table.
    pub async fn document_fingerprints(
        &self,
    ) -> Result<std::collections::BTreeMap<String, Fingerprint>> {
        let mut stream = self
            .table
            .query()
            .only_if("unit_kind = 'document'")
            .select(Select::Columns(vec![
                "path".into(),
                "content_hash".into(),
                "modified_at_ns".into(),
                "source_size".into(),
            ]))
            .execute()
            .await
            .map_err(index_err)?;
        let mut out = std::collections::BTreeMap::new();
        while let Some(batch) = stream.try_next().await.map_err(index_err)? {
            let paths = string_col(&batch, "path")?;
            let hashes = string_col(&batch, "content_hash")?;
            let mtimes = int64_col(&batch, "modified_at_ns")?;
            let sizes = int64_col(&batch, "source_size")?;
            for i in 0..batch.num_rows() {
                out.insert(
                    paths.value(i).to_owned(),
                    Fingerprint {
                        content_hash: hashes.value(i).to_owned(),
                        mtime_ns: mtimes.value(i),
                        size: sizes.value(i).max(0) as u64,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Return document paths whose semantic vector is still unavailable.
    pub async fn documents_missing_vectors(&self) -> Result<BTreeSet<String>> {
        let mut stream = self
            .table
            .query()
            .only_if("unit_kind = 'document' AND vector IS NULL")
            .select(Select::Columns(vec!["path".into()]))
            .execute()
            .await
            .map_err(index_err)?;
        let mut out = BTreeSet::new();
        while let Some(batch) = stream.try_next().await.map_err(index_err)? {
            let paths = string_col(&batch, "path")?;
            for i in 0..batch.num_rows() {
                out.insert(paths.value(i).to_owned());
            }
        }
        Ok(out)
    }

    /// Update only the source fingerprint when the content is unchanged.
    pub async fn update_document_fingerprint(
        &self,
        path: &PathScope,
        fingerprint: &Fingerprint,
    ) -> Result<()> {
        self.table
            .update()
            .only_if(format!(
                "unit_kind = 'document' AND path = '{}'",
                sql_string(path.0.as_str())
            ))
            .column("modified_at_ns", fingerprint.mtime_ns.to_string())
            .column("source_size", (fingerprint.size as i64).to_string())
            .execute()
            .await
            .map(|_| ())
            .map_err(index_err)
    }

    /// Replace every derived row owned by one Markdown file in one Lance commit.
    pub async fn replace_document(
        &self,
        path: &PathScope,
        slices: &[Slice],
        edges: &[Edge],
        vectors: Option<&[Vec<f32>]>,
        fingerprint: &Fingerprint,
        embedding_identity: Option<&str>,
    ) -> Result<()> {
        if vectors.is_some_and(|vectors| {
            vectors.len() != slices.len()
                || vectors
                    .iter()
                    .any(|v| Some(v.len()) != self.vector_dims || v.iter().any(|x| !x.is_finite()))
        }) {
            return Err(AgentWikiError::Embedding(
                "vector dimensions do not match index schema".into(),
            ));
        }
        let batch = unified_batch(
            path,
            slices,
            edges,
            vectors,
            fingerprint,
            embedding_identity,
        )?;
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(
                vec![Ok(batch)].into_iter(),
                unified_schema(self.vector_dims),
            ));
        let escaped = sql_string(path.0.as_str());
        let mut merge = self.table.merge_insert(&["chunk_id"]);
        merge.when_matched_update_all(None);
        merge.when_not_matched_insert_all();
        merge.when_not_matched_by_source_delete(Some(format!("path = '{escaped}'")));
        merge.execute(reader).await.map_err(index_err)?;
        Ok(())
    }

    pub async fn delete_path(&self, path: &PathScope) -> Result<()> {
        self.table
            .delete(&format!("path = '{}'", sql_string(path.0.as_str())))
            .await
            .map(|_| ())
            .map_err(index_err)
    }

    pub async fn relations_for_path(&self, path: &str) -> Result<Vec<Edge>> {
        let p = sql_string(path);
        let mut stream = self
            .table
            .query()
            .only_if(format!(
                "unit_kind = 'relation' AND (path = '{p}' OR target_path = '{p}')"
            ))
            .select(Select::Columns(vec![
                "path".into(),
                "target_path".into(),
                "relation_type".into(),
                "source_section".into(),
            ]))
            .limit(10)
            .execute()
            .await
            .map_err(index_err)?;
        let mut out = Vec::new();
        while let Some(batch) = stream.try_next().await.map_err(index_err)? {
            let from = string_col(&batch, "path")?;
            let to = string_col(&batch, "target_path")?;
            let ty = string_col(&batch, "relation_type")?;
            let section = string_col(&batch, "source_section")?;
            for i in 0..batch.num_rows() {
                out.push(Edge {
                    from: PathScope(from.value(i).into()),
                    to: PathScope(to.value(i).into()),
                    relation_type: ty.value(i).into(),
                    section_source: section.value(i).into(),
                    status: if self.document_exists(to.value(i)).await? {
                        EdgeStatus::Resolved
                    } else {
                        EdgeStatus::Unresolved
                    },
                });
            }
        }
        Ok(out)
    }

    async fn document_exists(&self, path: &str) -> Result<bool> {
        let mut stream = self
            .table
            .query()
            .only_if(format!(
                "unit_kind = 'document' AND path = '{}'",
                sql_string(path)
            ))
            .limit(1)
            .execute()
            .await
            .map_err(index_err)?;
        Ok(stream.try_next().await.map_err(index_err)?.is_some())
    }

    pub async fn search(
        &self,
        query: &ContextQuery,
        text: &str,
        kind: RetrievalUnitKind,
        limit: usize,
        source: &str,
    ) -> Result<Vec<RankedSlice>> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut stream = self
            .table
            .query()
            .full_text_search(fts_query(query, text))
            .only_if(filter_expression(query, kind))
            .select(Select::Columns(result_columns(false)))
            .limit(limit.clamp(1, 200))
            .execute()
            .await
            .map_err(index_err)?;
        collect_ranked(&mut stream, source).await
    }

    pub async fn hybrid_search(
        &self,
        query: &ContextQuery,
        text: &str,
        vector: &[f32],
        kind: RetrievalUnitKind,
        limit: usize,
    ) -> Result<Vec<RankedSlice>> {
        if Some(vector.len()) != self.vector_dims {
            return Err(AgentWikiError::Embedding("invalid query vector".into()));
        }
        let mut stream = self
            .table
            .query()
            .full_text_search(fts_query(query, text))
            .nearest_to(vector)
            .map_err(index_err)?
            .only_if(filter_expression(query, kind))
            .rerank(Arc::new(RRFReranker::default()))
            .select(Select::Columns(result_columns(false)))
            .limit(limit.clamp(1, 200))
            .execute_hybrid(Default::default())
            .await
            .map_err(index_err)?;
        collect_ranked(&mut stream, "keyword+semantic").await
    }

    pub async fn browse_documents(
        &self,
        query: &ContextQuery,
        limit: usize,
    ) -> Result<Vec<RankedSlice>> {
        let mut stream = self
            .table
            .query()
            .only_if(filter_expression(query, RetrievalUnitKind::Document))
            .order_by(Some(vec![
                ColumnOrdering::desc_nulls_last("modified_at_ns".into()),
                ColumnOrdering::asc_nulls_last("path".into()),
            ]))
            .select(Select::Columns(result_columns(false)))
            .limit(limit.clamp(1, 20))
            .execute()
            .await
            .map_err(index_err)?;
        collect_ranked(&mut stream, "recency").await
    }

    pub async fn exact_documents(
        &self,
        query: &ContextQuery,
        text: &str,
    ) -> Result<Vec<RankedSlice>> {
        let wanted = normalize_lookup(text);
        let mut stream = self
            .table
            .query()
            .only_if(format!(
                "{} AND array_has(lookup_keys, '{}')",
                filter_expression(query, RetrievalUnitKind::Document),
                sql_string(&wanted)
            ))
            .select(Select::Columns(result_columns(false)))
            .limit(20)
            .execute()
            .await
            .map_err(index_err)?;
        collect_ranked(&mut stream, "exact").await
    }

    pub async fn frontmatter_for_paths(
        &self,
        paths: &[String],
    ) -> Result<std::collections::HashMap<String, Frontmatter>> {
        if paths.is_empty() {
            return Ok(Default::default());
        }
        let list = paths
            .iter()
            .map(|p| format!("'{}'", sql_string(p)))
            .collect::<Vec<_>>()
            .join(",");
        let mut stream = self
            .table
            .query()
            .only_if(format!("unit_kind = 'document' AND path IN ({list})"))
            .select(Select::Columns(vec![
                "path".into(),
                "frontmatter_json".into(),
            ]))
            .execute()
            .await
            .map_err(index_err)?;
        let mut out = std::collections::HashMap::new();
        while let Some(batch) = stream.try_next().await.map_err(index_err)? {
            let paths = string_col(&batch, "path")?;
            let values = string_col(&batch, "frontmatter_json")?;
            for i in 0..batch.num_rows() {
                out.insert(
                    paths.value(i).to_owned(),
                    serde_json::from_str(values.value(i))
                        .map_err(|e| AgentWikiError::Index(e.to_string()))?,
                );
            }
        }
        Ok(out)
    }

    pub async fn all_tags(&self) -> Result<std::collections::BTreeMap<String, usize>> {
        let mut stream = self
            .table
            .query()
            .only_if("unit_kind = 'document'")
            .select(Select::Columns(vec!["tags".into()]))
            .execute()
            .await
            .map_err(index_err)?;
        let mut out = std::collections::BTreeMap::new();
        while let Some(batch) = stream.try_next().await.map_err(index_err)? {
            let lists = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<ListArray>())
                .ok_or_else(|| AgentWikiError::Index("missing tags".into()))?;
            for i in 0..batch.num_rows() {
                let vals = lists.value(i);
                let vals = vals
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| AgentWikiError::Index("invalid tags".into()))?;
                for j in 0..vals.len() {
                    *out.entry(vals.value(j).to_owned()).or_insert(0) += 1;
                }
            }
        }
        Ok(out)
    }

    pub async fn slices_for_path(&self, path: &str) -> Result<Vec<RankedSlice>> {
        let mut stream = self
            .table
            .query()
            .only_if(format!(
                "path = '{}' AND unit_kind IN ('document','fragment')",
                sql_string(path)
            ))
            .select(Select::Columns(result_columns(false)))
            .execute()
            .await
            .map_err(index_err)?;
        let mut out = collect_ranked(&mut stream, "recency").await?;
        out.sort_by_key(|h| h.slice.ordinal);
        Ok(out)
    }
}

fn index_err(e: impl std::fmt::Display) -> AgentWikiError {
    AgentWikiError::Index(e.to_string())
}
fn sql_string(s: &str) -> String {
    s.replace('\'', "''")
}
fn normalize_lookup(s: &str) -> String {
    s.trim().to_lowercase()
}

fn fts_query(query: &ContextQuery, fallback: &str) -> FullTextSearchQuery {
    let natural = if query.query.trim().is_empty() {
        fallback.to_owned()
    } else {
        query.query.clone()
    };
    if query.keywords.is_empty() {
        return FullTextSearchQuery::new(natural);
    }
    let terms = query
        .keywords
        .iter()
        .cloned()
        .map(|term| FtsQuery::from(MatchQuery::new(term)))
        .collect::<Vec<_>>();
    let keyword_branch = if query.keyword_mode == crate::retrieval::types::KeywordMode::All {
        FtsQuery::Boolean(BooleanQuery::new(
            terms.into_iter().map(|term| (Occur::Must, term)),
        ))
    } else {
        FtsQuery::Boolean(BooleanQuery::new(
            terms.into_iter().map(|term| (Occur::Should, term)),
        ))
    };
    let combined = if query.query.trim().is_empty() {
        keyword_branch
    } else {
        FtsQuery::Boolean(BooleanQuery::new([
            (Occur::Should, FtsQuery::from(MatchQuery::new(natural))),
            (Occur::Should, keyword_branch),
        ]))
    };
    FullTextSearchQuery::new_query(combined)
}

fn filter_expression(query: &ContextQuery, kind: RetrievalUnitKind) -> String {
    let mut clauses = vec![format!("unit_kind = '{}'", kind.as_str())];
    let scope = query.scope.trim().trim_end_matches('/');
    if !scope.is_empty() {
        let scope = sql_string(scope);
        clauses.push(format!("(path = '{scope}' OR path LIKE '{scope}/%')"));
    }
    if !query.note_types.is_empty() {
        clauses.push(format!(
            "type IN ({})",
            query
                .note_types
                .iter()
                .map(|x| format!("'{}'", sql_string(x)))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if !query.tags.is_empty() {
        clauses.push(format!(
            "array_has_all(tags, [{}])",
            query
                .tags
                .iter()
                .map(|x| format!("'{}'", sql_string(&x.trim().to_lowercase())))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for (key, value) in &query.metadata_filters {
        let facet = format!(
            "{key}={}",
            serde_json::to_string(value).expect("JSON value")
        );
        clauses.push(format!("array_has(facets, '{}')", sql_string(&facet)));
    }
    if let Some(v) = query.modified_after_ns {
        clauses.push(format!("modified_at_ns >= {v}"));
    }
    if let Some(v) = query.modified_before_ns {
        clauses.push(format!("modified_at_ns <= {v}"));
    }
    clauses.join(" AND ")
}

fn result_columns(_distance: bool) -> Vec<String> {
    [
        "path",
        "chunk_id",
        "unit_kind",
        "type",
        "tags",
        "facets",
        "frontmatter_json",
        "modified_at_ns",
        "ordinal",
        "section",
        "content",
        "search_text",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

async fn collect_ranked(
    stream: &mut lancedb::arrow::SendableRecordBatchStream,
    source: &str,
) -> Result<Vec<RankedSlice>> {
    let mut out = Vec::new();
    while let Some(batch) = stream.try_next().await.map_err(index_err)? {
        for i in 0..batch.num_rows() {
            let slice = slice_from_batch(&batch, i)?;
            out.push(RankedSlice {
                modified_at_ns: slice.modified_at_ns,
                slice,
                score: 1.0 / (out.len() as f64 + 1.0),
                sources: vec![source.into()],
                filename: String::new(),
                frontmatter: Frontmatter::new(),
            });
        }
    }
    Ok(out)
}

fn slice_from_batch(batch: &RecordBatch, row: usize) -> Result<Slice> {
    let s = |name: &str| string_col(batch, name).map(|x| x.value(row).to_owned());
    let i = |name: &str| int32_col(batch, name).map(|x| x.value(row) as u32);
    let t = match s("unit_kind")?.as_str() {
        "document" => RetrievalUnitKind::Document,
        _ => RetrievalUnitKind::Fragment,
    };
    let path = PathScope(s("path")?.into());
    let frontmatter: Frontmatter = serde_json::from_str(&s("frontmatter_json")?)
        .map_err(|e| AgentWikiError::Index(e.to_string()))?;
    Ok(Slice {
        path,
        chunk_id: s("chunk_id")?,
        unit_kind: t,
        note_type: s("type")?,
        tags: list_values(batch, "tags", row)?,
        facets: list_values(batch, "facets", row)?,
        frontmatter,
        modified_at_ns: int64_col(batch, "modified_at_ns")?.value(row),
        ordinal: i("ordinal")?,
        section: s("section")?,
        content: s("content")?,
        search_text: s("search_text")?,
    })
}

fn string_col<'a>(b: &'a RecordBatch, n: &str) -> Result<&'a StringArray> {
    b.column_by_name(n)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| AgentWikiError::Index(format!("missing {n}")))
}
fn int64_col<'a>(b: &'a RecordBatch, n: &str) -> Result<&'a Int64Array> {
    b.column_by_name(n)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| AgentWikiError::Index(format!("missing {n}")))
}
fn int32_col<'a>(b: &'a RecordBatch, n: &str) -> Result<&'a Int32Array> {
    b.column_by_name(n)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| AgentWikiError::Index(format!("missing {n}")))
}
fn list_values(b: &RecordBatch, n: &str, row: usize) -> Result<Vec<String>> {
    let list = b
        .column_by_name(n)
        .and_then(|c| c.as_any().downcast_ref::<ListArray>())
        .ok_or_else(|| AgentWikiError::Index(format!("missing {n}")))?;
    let vals = list.value(row);
    let vals = vals
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| AgentWikiError::Index(format!("invalid {n}")))?;
    Ok((0..vals.len()).map(|i| vals.value(i).to_owned()).collect())
}

fn unified_schema(dims: Option<usize>) -> Arc<Schema> {
    let dims = dims.unwrap_or(DIMS);
    Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("unit_kind", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, false),
        list_field("tags"),
        list_field("facets"),
        Field::new("frontmatter_json", DataType::Utf8, false),
        Field::new("modified_at_ns", DataType::Int64, false),
        Field::new("ordinal", DataType::Int32, false),
        Field::new("section", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("search_text", DataType::Utf8, false),
        list_field("lookup_keys"),
        Field::new("target_path", DataType::Utf8, false),
        Field::new("relation_type", DataType::Utf8, false),
        Field::new("source_section", DataType::Utf8, false),
        Field::new("source_size", DataType::Int64, false),
        Field::new("content_hash", DataType::Utf8, false),
        Field::new("vector_input_hash", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dims as i32,
            ),
            true,
        ),
    ]))
}
fn list_field(name: &str) -> Field {
    Field::new(
        name,
        DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
        false,
    )
}

fn unified_batch(
    path: &PathScope,
    slices: &[Slice],
    edges: &[Edge],
    vectors: Option<&[Vec<f32>]>,
    fp: &Fingerprint,
    embedding_identity: Option<&str>,
) -> Result<RecordBatch> {
    let schema = unified_schema(
        vectors
            .map(|v| v.first().map_or(DIMS, Vec::len))
            .or(Some(DIMS)),
    );
    let mut paths = StringBuilder::new();
    let mut ids = StringBuilder::new();
    let mut kinds = StringBuilder::new();
    let mut types = StringBuilder::new();
    let mut tags = ListBuilder::new(StringBuilder::new());
    let mut facets = ListBuilder::new(StringBuilder::new());
    let mut fm = StringBuilder::new();
    let mut modified = arrow_array::builder::Int64Builder::new();
    let mut ordinal = arrow_array::builder::Int32Builder::new();
    let mut sections = StringBuilder::new();
    let mut content = StringBuilder::new();
    let mut search = StringBuilder::new();
    let mut lookup = ListBuilder::new(StringBuilder::new());
    let mut target = StringBuilder::new();
    let mut rel_type = StringBuilder::new();
    let mut source_section = StringBuilder::new();
    let mut size = arrow_array::builder::Int64Builder::new();
    let mut content_hash = StringBuilder::new();
    let mut vector_hash = StringBuilder::new();
    let mut vectors_builder = FixedSizeListBuilder::new(Float32Builder::new(), DIMS as i32);
    let add_list = |b: &mut ListBuilder<StringBuilder>, values: &[String]| {
        for v in values {
            b.values().append_value(v);
        }
        b.append(true);
    };
    let mut add_vec = |v: Option<&Vec<f32>>| {
        if let Some(v) = v {
            vectors_builder.values().append_slice(v);
            vectors_builder.append(true);
        } else {
            vectors_builder.values().append_nulls(DIMS);
            vectors_builder.append(false);
        }
    };
    for (n, slice) in slices.iter().enumerate() {
        paths.append_value(path.0.as_str());
        ids.append_value(&slice.chunk_id);
        kinds.append_value(slice.unit_kind.as_str());
        types.append_value(&slice.note_type);
        add_list(&mut tags, &slice.tags);
        add_list(&mut facets, &slice.facets);
        fm.append_value(
            serde_json::to_string(&slice.frontmatter)
                .map_err(|e| AgentWikiError::Index(e.to_string()))?,
        );
        modified.append_value(slice.modified_at_ns);
        ordinal.append_value(slice.ordinal as i32);
        sections.append_value(&slice.section);
        content.append_value(&slice.content);
        search.append_value(&slice.search_text);
        let keys = lookup_keys(slice);
        add_list(&mut lookup, &keys);
        target.append_value("");
        rel_type.append_value("");
        source_section.append_value("");
        size.append_value(if slice.unit_kind == RetrievalUnitKind::Document {
            fp.size as i64
        } else {
            0
        });
        content_hash.append_value(if slice.unit_kind == RetrievalUnitKind::Document {
            &fp.content_hash
        } else {
            ""
        });
        vector_hash.append_value(embedding_input_hash(embedding_identity, &slice.search_text));
        add_vec(vectors.and_then(|vs| vs.get(n)));
    }
    for edge in edges {
        paths.append_value(path.0.as_str());
        ids.append_value(hex::encode(Sha256::digest(
            format!("{}\0{}\0{}", edge.from.0, edge.relation_type, edge.to.0).as_bytes(),
        )));
        kinds.append_value("relation");
        types.append_value("");
        add_list(&mut tags, &[]);
        add_list(&mut facets, &[]);
        fm.append_value("{}");
        modified.append_value(fp.mtime_ns);
        ordinal.append_value(0);
        sections.append_value("");
        content.append_value("");
        search.append_value("");
        add_list(&mut lookup, &[]);
        target.append_value(edge.to.0.as_str());
        rel_type.append_value(&edge.relation_type);
        source_section.append_value(&edge.section_source);
        size.append_value(0);
        content_hash.append_value("");
        vector_hash.append_value("");
        add_vec(None);
    }
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(paths.finish()),
            Arc::new(ids.finish()),
            Arc::new(kinds.finish()),
            Arc::new(types.finish()),
            Arc::new(tags.finish()),
            Arc::new(facets.finish()),
            Arc::new(fm.finish()),
            Arc::new(modified.finish()),
            Arc::new(ordinal.finish()),
            Arc::new(sections.finish()),
            Arc::new(content.finish()),
            Arc::new(search.finish()),
            Arc::new(lookup.finish()),
            Arc::new(target.finish()),
            Arc::new(rel_type.finish()),
            Arc::new(source_section.finish()),
            Arc::new(size.finish()),
            Arc::new(content_hash.finish()),
            Arc::new(vector_hash.finish()),
            Arc::new(vectors_builder.finish()),
        ],
    )
    .map_err(|e| AgentWikiError::Index(e.to_string()))
}

fn embedding_input_hash(identity: Option<&str>, input: &str) -> String {
    identity
        .map(|identity| hex::encode(Sha256::digest(format!("{identity}\0{input}").as_bytes())))
        .unwrap_or_default()
}

fn lookup_keys(slice: &Slice) -> Vec<String> {
    let path = slice.path.0.as_str().to_lowercase();
    let filename = slice
        .path
        .0
        .file_stem()
        .unwrap_or(path.as_str())
        .to_lowercase();
    let mut out = vec![path, filename];
    if let Some(title) = slice.frontmatter.get("title").and_then(|v| v.as_str()) {
        let title = title.trim().to_lowercase();
        if !title.is_empty() {
            out.push(title);
        }
    }
    if let Some(aliases) = slice.frontmatter.get("aliases").and_then(|v| v.as_array()) {
        out.extend(
            aliases
                .iter()
                .filter_map(|value| value.as_str())
                .map(|value| value.trim().to_lowercase())
                .filter(|value| !value.is_empty()),
        );
    }
    out
}

async fn open_or_create(db: &Connection, dims: Option<usize>) -> lancedb::Result<Table> {
    if let Ok(table) = db.open_table(TABLE).execute().await {
        return Ok(table);
    }
    let schema = unified_schema(dims);
    let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(
            vec![Ok(RecordBatch::new_empty(schema.clone()))].into_iter(),
            schema.clone(),
        ));
    let table = db.create_table(TABLE, reader).execute().await?;
    for col in [
        "path",
        "chunk_id",
        "modified_at_ns",
        "source_size",
        "target_path",
    ] {
        table
            .create_index(&[col], Index::BTree(BTreeIndexBuilder::default()))
            .execute()
            .await?;
    }
    for col in ["unit_kind", "type", "relation_type"] {
        table
            .create_index(&[col], Index::Bitmap(BitmapIndexBuilder::default()))
            .execute()
            .await?;
    }
    for col in ["tags", "facets", "lookup_keys"] {
        table
            .create_index(&[col], Index::LabelList(LabelListIndexBuilder::default()))
            .execute()
            .await?;
    }
    let fts = FtsIndexBuilder::default();
    table
        .create_index(&["search_text"], Index::FTS(fts))
        .execute()
        .await?;
    Ok(table)
}
