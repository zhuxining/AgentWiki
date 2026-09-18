//! LanceDB backend boundary. Backend and Arrow types stay private here.
use crate::document::types::{Edge, EdgeStatus, Frontmatter, PathScope, RetrievalUnitKind, Slice};
use crate::error::Result;
use crate::retrieval::types::{ContextQuery, RankedSlice};
use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::types::Float32Type;
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Int32Array, Int64Array, ListArray,
    RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use camino::Utf8Path;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lance_index::scalar::inverted::Language;
use lancedb::index::Index;
use lancedb::index::scalar::{
    BTreeIndexBuilder, BitmapIndexBuilder, FtsIndexBuilder, LabelListIndexBuilder,
};
use lancedb::query::{ColumnOrdering, ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, Table, connect};
use sha2::Digest;
use std::sync::Arc;

const TABLE: &str = "retrieval_units";
const VECTOR_TABLE: &str = "vector_units";
const RELATIONS_TABLE: &str = "document_relations";

/// Lance FTS tokenizer for Chinese text, segmented with the bundled jieba
/// dictionary lookup (see [`ensure_language_model`]).
const FTS_TOKENIZER: &str = "jieba/default";
/// Dictionary path relative to the Lance language-model home directory.
const JIEBA_DICT_REL: &str = "jieba/default/dict.txt";

pub struct LanceIndex {
    table: Table,
    vector_table: Option<Table>,
    relations: Table,
    vector_dims: Option<usize>,
}

impl LanceIndex {
    pub async fn open(index_dir: &Utf8Path, vector_dims: Option<usize>) -> Result<Self> {
        ensure_language_model()?;
        let path = index_dir.join("lancedb");
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| crate::error::AgentWikiError::Io {
                path: path.clone(),
                source: e,
            })?;
        let uri = path.as_str().to_owned();
        let db = connect(&uri)
            .read_consistency_interval(std::time::Duration::ZERO)
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let table = open_or_create(&db)
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let vector_table = match vector_dims {
            Some(dims) => Some(
                open_or_create_vectors(&db, dims)
                    .await
                    .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?,
            ),
            None => None,
        };
        let relations = open_or_create_relations(&db)
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        Ok(Self {
            table,
            vector_table,
            relations,
            vector_dims,
        })
    }
    pub async fn reset(&self) -> Result<()> {
        self.table
            .delete("true")
            .await
            .map(|_| ())
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        if let Some(table) = &self.vector_table {
            table
                .delete("true")
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        }
        self.relations
            .delete("true")
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        Ok(())
    }
    pub async fn replace_slices(&self, path: &PathScope, slices: &[Slice]) -> Result<()> {
        let path = path.0.as_str().to_owned();
        let rows = slices.to_owned();
        self.table
            .delete(&format!("path = '{}'", path.replace('\'', "''")))
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        if !rows.is_empty() {
            self.table
                .add(
                    batch_reader(&rows)
                        .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?,
                )
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn replace_relations(&self, path: &PathScope, edges: &[Edge]) -> Result<()> {
        let escaped = path.0.as_str().replace('\'', "''");
        self.relations
            .delete(&format!("source_path = '{escaped}'"))
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        if !edges.is_empty() {
            self.relations
                .add(
                    relation_batch_reader(edges)
                        .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?,
                )
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn relations_for_path(&self, path: &str) -> Result<Vec<Edge>> {
        let escaped = path.replace('\'', "''");
        let mut stream = self
            .relations
            .query()
            .only_if(format!(
                "source_path = '{escaped}' OR target_path = '{escaped}'"
            ))
            .limit(10)
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
        {
            let strings = |name: &str| {
                batch
                    .column_by_name(name)
                    .and_then(|column| column.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
            };
            for row in 0..batch.num_rows() {
                let target = strings("target_path")?.value(row);
                out.push(Edge {
                    from: PathScope(strings("source_path")?.value(row).into()),
                    to: PathScope(target.into()),
                    relation_type: strings("relation_type")?.value(row).into(),
                    section_source: strings("source_section")?.value(row).into(),
                    status: if self.document_exists(target).await? {
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
        let escaped = path.replace('\'', "''");
        let mut stream = self
            .table
            .query()
            .only_if(format!("unit_kind = 'document' AND path = '{escaped}'"))
            .select(Select::Columns(vec!["unit_kind".into()]))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        Ok(stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
            .is_some())
    }
    pub async fn delete_path(&self, path: &PathScope) -> Result<()> {
        let path = path.0.as_str().to_owned();
        self.table
            .delete(&format!("path = '{}'", path.replace('\'', "''")))
            .await
            .map(|_| ())
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        if let Some(table) = &self.vector_table {
            table
                .delete(&format!("path = '{}'", path.replace('\'', "''")))
                .await
                .map(|_| ())
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        }
        self.relations
            .delete(&format!("source_path = '{}'", path.replace('\'', "''")))
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        Ok(())
    }

    pub async fn replace_vectors(
        &self,
        path: &PathScope,
        slices: &[Slice],
        vectors: &[Vec<f32>],
    ) -> Result<()> {
        let Some(table) = &self.vector_table else {
            return Ok(());
        };
        if slices.len() != vectors.len()
            || vectors.iter().any(|v| {
                Some(v.len()) != self.vector_dims || v.iter().any(|value| !value.is_finite())
            })
        {
            return Err(crate::error::AgentWikiError::Embedding(
                "vector dimensions do not match index schema".into(),
            ));
        }
        let path = path.0.as_str().to_owned();
        let rows = slices.to_owned();
        let vectors = vectors.to_owned();
        table
            .delete(&format!("path = '{}'", path.replace('\'', "''")))
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        if !rows.is_empty() {
            table
                .add(
                    vector_batch_reader(&rows, &vectors)
                        .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?,
                )
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn vector_search(
        &self,
        query: &ContextQuery,
        vector: &[f32],
        kind: RetrievalUnitKind,
        limit: usize,
    ) -> Result<Vec<RankedSlice>> {
        let Some(table) = &self.vector_table else {
            return Ok(Vec::new());
        };
        if Some(vector.len()) != self.vector_dims || vector.iter().any(|v| !v.is_finite()) {
            return Err(crate::error::AgentWikiError::Embedding(
                "invalid query vector".into(),
            ));
        }
        let vector = vector.to_owned();
        let builder = table
            .query()
            .nearest_to(vector.as_slice())
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
            .only_if(filter_expression(query, kind));
        let mut stream = builder
            .select(Select::Columns(vec!["chunk_id".into(), "_distance".into()]))
            .limit(limit.clamp(1, 200))
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut scores = std::collections::HashMap::new();
        let mut ids = Vec::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
        {
            // LanceDB reports raw (L2) distance in `_distance`; convert to
            // a bounded similarity and apply the calibrated semantic floor
            // so unrelated queries are not reported as hits (GAP-12).
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .ok_or_else(|| crate::error::AgentWikiError::Index("missing _distance".into()))?;
            let chunk_ids = batch
                .column_by_name("chunk_id")
                .and_then(|column| column.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| crate::error::AgentWikiError::Index("missing chunk_id".into()))?;
            for i in 0..batch.num_rows() {
                let score = 1.0 / (1.0 + f64::from(distances.value(i)));
                if score >= query.min_similarity {
                    let id = chunk_ids.value(i).to_owned();
                    scores.insert(id.clone(), score);
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let expression = ids
            .iter()
            .map(|id| format!("'{}'", sql_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut units = self
            .table
            .query()
            .only_if(format!("chunk_id IN ({expression})"))
            .select(Select::Columns(result_columns(false)))
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut by_id = collect_ranked(&mut units, "semantic")
            .await?
            .into_iter()
            .map(|hit| (hit.slice.chunk_id.clone(), hit))
            .collect::<std::collections::HashMap<_, _>>();
        let mut out = Vec::new();
        for id in ids {
            if let Some(mut hit) = by_id.remove(&id) {
                hit.score = scores[&id];
                out.push(hit);
            }
        }
        Ok(out)
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
        let query_builder = self
            .table
            .query()
            .full_text_search(FullTextSearchQuery::new(text.to_owned()))
            .only_if(filter_expression(query, kind));
        let mut stream = query_builder
            .select(Select::Columns(result_columns(false)))
            .limit(limit.clamp(1, 200))
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
        {
            for i in 0..batch.num_rows() {
                let slice = slice_from_batch(&batch, i)?;
                out.push(RankedSlice {
                    modified_at_ns: slice.modified_at_ns,
                    slice,
                    score: 1.0 / (i as f64 + 1.0),
                    sources: vec![source.into()],
                    filename: String::new(),
                    frontmatter: Default::default(),
                });
            }
        }
        Ok(out)
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
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        collect_ranked(&mut stream, "recency").await
    }

    pub async fn exact_documents(
        &self,
        query: &ContextQuery,
        text: &str,
    ) -> Result<Vec<RankedSlice>> {
        let wanted = text.trim().to_lowercase();
        let mut stream = self
            .table
            .query()
            .only_if(filter_expression(query, RetrievalUnitKind::Document))
            .select(Select::Columns(result_columns(false)))
            .limit(10_000)
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut hits = collect_ranked(&mut stream, "exact").await?;
        hits.retain(|hit| {
            let path = hit.slice.path.0.as_str();
            let filename = hit.slice.path.0.file_stem().unwrap_or(path);
            path.to_lowercase() == wanted
                || filename.to_lowercase() == wanted
                || hit.slice.title.to_lowercase() == wanted
                || hit.slice.aliases.iter().any(|alias| alias == &wanted)
        });
        Ok(hits)
    }

    pub async fn frontmatter_for_paths(
        &self,
        paths: &[String],
    ) -> Result<std::collections::HashMap<String, Frontmatter>> {
        if paths.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let values = paths
            .iter()
            .map(|path| format!("'{}'", sql_string(path)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stream = self
            .table
            .query()
            .only_if(format!("unit_kind = 'document' AND path IN ({values})"))
            .select(Select::Columns(vec![
                "path".into(),
                "frontmatter_json".into(),
            ]))
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut out = std::collections::HashMap::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
        {
            let strings = |name: &str| {
                batch
                    .column_by_name(name)
                    .and_then(|column| column.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
            };
            for row in 0..batch.num_rows() {
                out.insert(
                    strings("path")?.value(row).to_owned(),
                    serde_json::from_str(strings("frontmatter_json")?.value(row))
                        .map_err(|error| crate::error::AgentWikiError::Index(error.to_string()))?,
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
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut counts = std::collections::BTreeMap::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
        {
            let lists = batch
                .column_by_name("tags")
                .and_then(|column| column.as_any().downcast_ref::<ListArray>())
                .ok_or_else(|| crate::error::AgentWikiError::Index("missing tags".into()))?;
            for row in 0..batch.num_rows() {
                let values = lists.value(row);
                let strings = values
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| crate::error::AgentWikiError::Index("invalid tags".into()))?;
                for index in 0..strings.len() {
                    *counts.entry(strings.value(index).to_owned()).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }

    /// Return the indexed slices for one document path in source order.
    pub async fn slices_for_path(&self, path: &str) -> Result<Vec<RankedSlice>> {
        let path = path.replace('\'', "''");
        let mut stream = self
            .table
            .query()
            .only_if(format!("path = '{path}'"))
            .select(Select::Columns(result_columns(false)))
            .execute()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
        {
            for i in 0..batch.num_rows() {
                let slice = slice_from_batch(&batch, i)?;
                out.push(RankedSlice {
                    modified_at_ns: slice.modified_at_ns,
                    slice,
                    score: 0.0,
                    sources: vec!["recency".into()],
                    filename: String::new(),
                    frontmatter: Default::default(),
                });
            }
        }
        out.sort_by_key(|hit| hit.slice.ordinal);
        Ok(out)
    }
}

fn result_columns(include_distance: bool) -> Vec<String> {
    let mut columns = vec![
        "path".into(),
        "chunk_id".into(),
        "unit_kind".into(),
        "type".into(),
        "tags".into(),
        "facets".into(),
        "title".into(),
        "aliases".into(),
        "frontmatter_json".into(),
        "modified_at_ns".into(),
        "ordinal".into(),
        "section".into(),
        "content".into(),
        "search_text".into(),
        "source_hash".into(),
    ];
    if include_distance {
        columns.push("_distance".into());
    }
    columns
}

async fn collect_ranked(
    stream: &mut lancedb::arrow::SendableRecordBatchStream,
    source: &str,
) -> Result<Vec<RankedSlice>> {
    let mut out = Vec::new();
    while let Some(batch) = stream
        .try_next()
        .await
        .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
    {
        for row in 0..batch.num_rows() {
            let slice = slice_from_batch(&batch, row)?;
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
                .map(|value| format!("'{}'", sql_string(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !query.tags.is_empty() {
        clauses.push(format!(
            "array_has_all(tags, [{}])",
            query
                .tags
                .iter()
                .map(|tag| format!("'{}'", sql_string(&tag.trim().to_lowercase())))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (key, value) in &query.metadata_filters {
        let facet = format!(
            "{key}={}",
            serde_json::to_string(value).expect("query JSON value serializes")
        );
        clauses.push(format!("array_has(facets, '{}')", sql_string(&facet)));
    }
    if let Some(after) = query.modified_after_ns {
        clauses.push(format!("modified_at_ns >= {after}"));
    }
    if let Some(before) = query.modified_before_ns {
        clauses.push(format!("modified_at_ns <= {before}"));
    }
    clauses.join(" AND ")
}

fn sql_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn slice_from_batch(batch: &RecordBatch, row: usize) -> Result<Slice> {
    let strings = |name: &str| {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
    };
    let ints = |name: &str| {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
    };
    let longs = |name: &str| {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<Int64Array>())
            .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
    };
    let lists = |name: &str| {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<ListArray>())
            .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
    };
    let list_values = |name: &str| -> Result<Vec<String>> {
        let values = lists(name)?.value(row);
        let strings = values
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| crate::error::AgentWikiError::Index(format!("invalid {name}")))?;
        Ok((0..strings.len())
            .map(|index| strings.value(index).to_owned())
            .collect())
    };
    let unit_kind = match strings("unit_kind")?.value(row) {
        "document" => RetrievalUnitKind::Document,
        _ => RetrievalUnitKind::Fragment,
    };
    Ok(Slice {
        path: PathScope(strings("path")?.value(row).into()),
        chunk_id: strings("chunk_id")?.value(row).into(),
        unit_kind,
        note_type: strings("type")?.value(row).into(),
        tags: list_values("tags")?,
        facets: list_values("facets")?,
        title: strings("title")?.value(row).into(),
        aliases: list_values("aliases")?,
        frontmatter: serde_json::from_str(strings("frontmatter_json")?.value(row))
            .map_err(|error| crate::error::AgentWikiError::Index(error.to_string()))?,
        modified_at_ns: longs("modified_at_ns")?.value(row),
        ordinal: ints("ordinal")?.value(row) as u32,
        section: strings("section")?.value(row).into(),
        content: strings("content")?.value(row).into(),
        search_text: strings("search_text")?.value(row).into(),
        source_hash: strings("source_hash")?.value(row).into(),
    })
}

async fn open_or_create(db: &Connection) -> lancedb::Result<Table> {
    match db.open_table(TABLE).execute().await {
        Ok(table) => Ok(table),
        Err(_) => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("path", DataType::Utf8, false),
                Field::new("chunk_id", DataType::Utf8, false),
                Field::new("unit_kind", DataType::Utf8, false),
                Field::new("type", DataType::Utf8, false),
                string_list_field("tags"),
                string_list_field("facets"),
                Field::new("title", DataType::Utf8, false),
                string_list_field("aliases"),
                Field::new("frontmatter_json", DataType::Utf8, false),
                Field::new("modified_at_ns", DataType::Int64, false),
                Field::new("ordinal", DataType::Int32, false),
                Field::new("section", DataType::Utf8, false),
                Field::new("content", DataType::Utf8, false),
                Field::new("search_text", DataType::Utf8, false),
                Field::new("source_hash", DataType::Utf8, false),
            ]));
            let empty = RecordBatch::new_empty(schema.clone());
            let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
                RecordBatchIterator::new(vec![Ok(empty)].into_iter(), schema.clone()),
            );
            let table = db.create_table(TABLE, reader).execute().await?;
            // Chinese text must be segmented, not split on whitespace: the
            // default `simple` tokenizer treats a whole sentence as one token
            // and query terms never match. `jieba` is dictionary-backed and
            // works offline via the language-model directory.
            // `Language` only drives stemming/stop-words and is ignored for
            // jieba; the default `English` is harmless here.
            let fts = FtsIndexBuilder::new(FTS_TOKENIZER.to_string(), Language::English);
            table
                .create_index(&["search_text"], Index::FTS(fts))
                .execute()
                .await?;
            create_filter_scalar_indices(&table).await?;
            table
                .create_index(
                    &["aliases"],
                    Index::LabelList(LabelListIndexBuilder::default()),
                )
                .execute()
                .await?;
            Ok(table)
        }
    }
}

/// The Lance FTS jieba tokenizer loads its dictionary from a language-model
/// directory: `$LANCE_LANGUAGE_MODEL_HOME`, or the platform data directory
/// `lance/language_models` by default. The dictionary is not bundled with the
/// crates, so verify it is present before creating any FTS index and report a
/// precise, actionable error instead of a cryptic index-build failure.
fn ensure_language_model() -> Result<()> {
    let home = match std::env::var_os("LANCE_LANGUAGE_MODEL_HOME") {
        Some(home) => camino::Utf8PathBuf::from_path_buf(home.into()).map_err(|p| {
            crate::error::AgentWikiError::Config(format!(
                "LANCE_LANGUAGE_MODEL_HOME is not valid UTF-8: {p:?}"
            ))
        })?,
        None => match dirs::data_local_dir() {
            Some(dir) => {
                let buf = dir.join("lance").join("language_models");
                camino::Utf8PathBuf::from_path_buf(buf).map_err(|p| {
                    crate::error::AgentWikiError::Config(format!(
                        "data directory is not valid UTF-8: {p:?}"
                    ))
                })?
            }
            None => {
                return Err(crate::error::AgentWikiError::Config(
                    "no platform data directory; set LANCE_LANGUAGE_MODEL_HOME".into(),
                ));
            }
        },
    };
    let dict = home.join(JIEBA_DICT_REL);
    if dict.exists() {
        return Ok(());
    }
    Err(crate::error::AgentWikiError::Config(format!(
        "jieba dictionary not found at {dict}; download it from \
         https://cdn.jsdelivr.net/gh/fxsjy/jieba@master/jieba/dict.txt and place it \
         there (or point LANCE_LANGUAGE_MODEL_HOME at a directory containing \
         {JIEBA_DICT_REL})"
    )))
}

async fn open_or_create_vectors(db: &Connection, dims: usize) -> lancedb::Result<Table> {
    let name = VECTOR_TABLE;
    match db.open_table(name).execute().await {
        Ok(table) => Ok(table),
        Err(_) => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("path", DataType::Utf8, false),
                Field::new("chunk_id", DataType::Utf8, false),
                Field::new("unit_kind", DataType::Utf8, false),
                Field::new("type", DataType::Utf8, false),
                string_list_field("tags"),
                string_list_field("facets"),
                Field::new("modified_at_ns", DataType::Int64, false),
                Field::new(
                    "vector",
                    DataType::FixedSizeList(
                        Arc::new(Field::new("item", DataType::Float32, true)),
                        dims as i32,
                    ),
                    false,
                ),
            ]));
            let empty = RecordBatch::new_empty(schema.clone());
            let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
                RecordBatchIterator::new(vec![Ok(empty)].into_iter(), schema),
            );
            let table = db.create_table(name, reader).execute().await?;
            create_filter_scalar_indices(&table).await?;
            Ok(table)
        }
    }
}

async fn create_filter_scalar_indices(table: &Table) -> lancedb::Result<()> {
    table
        .create_index(&["path"], Index::BTree(BTreeIndexBuilder::default()))
        .execute()
        .await?;
    table
        .create_index(&["unit_kind"], Index::Bitmap(BitmapIndexBuilder::default()))
        .execute()
        .await?;
    table
        .create_index(&["type"], Index::Bitmap(BitmapIndexBuilder::default()))
        .execute()
        .await?;
    table
        .create_index(
            &["tags"],
            Index::LabelList(LabelListIndexBuilder::default()),
        )
        .execute()
        .await?;
    table
        .create_index(
            &["facets"],
            Index::LabelList(LabelListIndexBuilder::default()),
        )
        .execute()
        .await?;
    table
        .create_index(
            &["modified_at_ns"],
            Index::BTree(BTreeIndexBuilder::default()),
        )
        .execute()
        .await?;
    Ok(())
}

async fn open_or_create_relations(db: &Connection) -> lancedb::Result<Table> {
    match db.open_table(RELATIONS_TABLE).execute().await {
        Ok(table) => Ok(table),
        Err(_) => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("relation_id", DataType::Utf8, false),
                Field::new("source_path", DataType::Utf8, false),
                Field::new("relation_type", DataType::Utf8, false),
                Field::new("target_path", DataType::Utf8, false),
                Field::new("source_section", DataType::Utf8, false),
            ]));
            let empty = RecordBatch::new_empty(schema.clone());
            let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
                RecordBatchIterator::new(vec![Ok(empty)].into_iter(), schema),
            );
            let table = db.create_table(RELATIONS_TABLE, reader).execute().await?;
            for column in ["source_path", "target_path"] {
                table
                    .create_index(&[column], Index::BTree(BTreeIndexBuilder::default()))
                    .execute()
                    .await?;
            }
            table
                .create_index(
                    &["relation_type"],
                    Index::Bitmap(BitmapIndexBuilder::default()),
                )
                .execute()
                .await?;
            Ok(table)
        }
    }
}

fn relation_batch_reader(
    edges: &[Edge],
) -> lancedb::Result<Box<dyn arrow_array::RecordBatchReader + Send>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("relation_id", DataType::Utf8, false),
        Field::new("source_path", DataType::Utf8, false),
        Field::new("relation_type", DataType::Utf8, false),
        Field::new("target_path", DataType::Utf8, false),
        Field::new("source_section", DataType::Utf8, false),
    ]));
    let ids = edges.iter().map(|edge| {
        hex::encode(sha2::Sha256::digest(
            format!("{}\0{}\0{}", edge.from.0, edge.relation_type, edge.to.0).as_bytes(),
        ))
    });
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from_iter_values(ids)) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                edges.iter().map(|edge| edge.from.0.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                edges.iter().map(|edge| edge.relation_type.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                edges.iter().map(|edge| edge.to.0.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                edges.iter().map(|edge| edge.section_source.as_str()),
            )) as ArrayRef,
        ],
    )?;
    Ok(Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema,
    )))
}

fn string_list_field(name: &str) -> Field {
    Field::new(
        name,
        DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
        false,
    )
}

fn list_array<'a>(rows: impl Iterator<Item = &'a [String]>) -> ListArray {
    let mut builder = ListBuilder::new(StringBuilder::new());
    for values in rows {
        for value in values {
            builder.values().append_value(value);
        }
        builder.append(true);
    }
    builder.finish()
}

fn batch_reader(rows: &[Slice]) -> lancedb::Result<Box<dyn arrow_array::RecordBatchReader + Send>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("unit_kind", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, false),
        string_list_field("tags"),
        string_list_field("facets"),
        Field::new("title", DataType::Utf8, false),
        string_list_field("aliases"),
        Field::new("frontmatter_json", DataType::Utf8, false),
        Field::new("modified_at_ns", DataType::Int64, false),
        Field::new("ordinal", DataType::Int32, false),
        Field::new("section", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("search_text", DataType::Utf8, false),
        Field::new("source_hash", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.path.0.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.chunk_id.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.unit_kind.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.note_type.as_str()),
            )) as ArrayRef,
            Arc::new(list_array(rows.iter().map(|row| row.tags.as_slice()))) as ArrayRef,
            Arc::new(list_array(rows.iter().map(|row| row.facets.as_slice()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.title.as_str()),
            )) as ArrayRef,
            Arc::new(list_array(rows.iter().map(|row| row.aliases.as_slice()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(rows.iter().map(|row| {
                serde_json::to_string(&row.frontmatter)
                    .expect("frontmatter JSON value always serializes")
            }))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|r| r.modified_at_ns),
            )) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(
                rows.iter().map(|r| r.ordinal as i32),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.section.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.content.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.search_text.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.source_hash.as_str()),
            )) as ArrayRef,
        ],
    )?;
    Ok(Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema,
    )))
}

fn vector_batch_reader(
    rows: &[Slice],
    vectors: &[Vec<f32>],
) -> lancedb::Result<Box<dyn arrow_array::RecordBatchReader + Send>> {
    let dims = vectors.first().map_or(0, Vec::len);
    let schema = Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("unit_kind", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, false),
        string_list_field("tags"),
        string_list_field("facets"),
        Field::new("modified_at_ns", DataType::Int64, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dims as i32,
            ),
            false,
        ),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.path.0.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.chunk_id.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.unit_kind.as_str()),
            )) as ArrayRef,
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.note_type.as_str()),
            )) as ArrayRef,
            Arc::new(list_array(rows.iter().map(|row| row.tags.as_slice()))) as ArrayRef,
            Arc::new(list_array(rows.iter().map(|row| row.facets.as_slice()))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|r| r.modified_at_ns),
            )) as ArrayRef,
            Arc::new(
                FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                    vectors
                        .iter()
                        .map(|v| Some(v.iter().copied().map(Some).collect::<Vec<_>>())),
                    dims as i32,
                ),
            ) as ArrayRef,
        ],
    )?;
    Ok(Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::types::ContextQuery;

    #[tokio::test]
    async fn indexes_and_queries_chinese_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let index = LanceIndex::open(root, None).await.unwrap();
        let path = PathScope("notes/认证.md".into());
        index
            .replace_slices(
                &path,
                &[Slice {
                    path: path.clone(),
                    chunk_id: "chunk-zh".into(),
                    unit_kind: RetrievalUnitKind::Fragment,
                    note_type: "note".into(),
                    tags: vec![],
                    facets: vec![],
                    title: "认证".into(),
                    aliases: vec![],
                    frontmatter: Frontmatter::new(),
                    modified_at_ns: 0,
                    ordinal: 0,
                    section: "刷新令牌".into(),
                    content: "认证方案采用OAuth2协议，刷新令牌轮换策略30天。".into(),
                    search_text: "认证\n认证方案采用OAuth2协议，刷新令牌轮换策略30天。".into(),
                    source_hash: "hash-zh".into(),
                }],
            )
            .await
            .unwrap();
        for text in ["认证", "令牌", "轮换", "刷新令牌"] {
            let hits = index
                .search(
                    &ContextQuery {
                        query: text.into(),
                        ..Default::default()
                    },
                    text,
                    RetrievalUnitKind::Fragment,
                    10,
                    "keyword",
                )
                .await
                .unwrap();
            assert_eq!(hits.len(), 1, "query `{text}` should hit");
        }
    }

    #[tokio::test]
    async fn supports_optional_vector_projection() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let index = LanceIndex::open(root, Some(2)).await.unwrap();
        let path = PathScope("notes/vector.md".into());
        let slices = vec![Slice {
            path: path.clone(),
            chunk_id: "v1".into(),
            unit_kind: RetrievalUnitKind::Fragment,
            note_type: "note".into(),
            tags: vec![],
            facets: vec![],
            title: "Vector".into(),
            aliases: vec![],
            frontmatter: Frontmatter::new(),
            modified_at_ns: 0,
            ordinal: 0,
            section: String::new(),
            content: "semantic evidence".into(),
            search_text: "semantic evidence".into(),
            source_hash: "hash".into(),
        }];
        index.replace_slices(&path, &slices).await.unwrap();
        index
            .replace_vectors(&path, &slices, &[vec![1.0, 0.0]])
            .await
            .unwrap();
        let hits = index
            .vector_search(
                &ContextQuery {
                    query: "semantic".into(),
                    ..Default::default()
                },
                &[1.0, 0.0],
                RetrievalUnitKind::Fragment,
                10,
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sources, vec!["semantic"]);
    }
}
