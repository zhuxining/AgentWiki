//! LanceDB backend boundary. Backend and Arrow types stay private here.
use crate::error::Result;
use crate::model::{ContextQuery, PathScope, RankedSlice, Slice};
use arrow_array::types::Float32Type;
use arrow_array::{
    ArrayRef, FixedSizeListArray, Int32Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use camino::Utf8Path;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::Index;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, Table, connect};
use std::sync::Arc;

const TABLE: &str = "chunks";

pub struct LanceIndex {
    runtime: Arc<tokio::runtime::Runtime>,
    table: Table,
    vector_table: Option<Table>,
    vector_dims: Option<usize>,
}

impl LanceIndex {
    pub fn open(index_dir: &Utf8Path, _vector_dims: Option<usize>) -> Result<Self> {
        let path = index_dir.join("lancedb");
        std::fs::create_dir_all(&path).map_err(|e| crate::error::AgentWikiError::Io {
            path: path.clone(),
            source: e,
        })?;
        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(|e| crate::error::AgentWikiError::Other(e.to_string()))?,
        );
        let uri = path.as_str().to_owned();
        let vector_dims_for_db = _vector_dims;
        let (table, vector_table) = runtime.block_on(async move {
            let db = connect(&uri)
                .read_consistency_interval(std::time::Duration::ZERO)
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            let table = open_or_create(&db)
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            let vectors = match vector_dims_for_db {
                Some(dims) => Some(
                    open_or_create_vectors(&db, dims)
                        .await
                        .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?,
                ),
                None => None,
            };
            Ok::<_, crate::error::AgentWikiError>((table, vectors))
        })?;
        Ok(Self {
            runtime,
            table,
            vector_table,
            vector_dims: _vector_dims,
        })
    }
    pub fn reset(&self) -> Result<()> {
        let table = self.table.clone();
        let vector_table = self.vector_table.clone();
        self.runtime.block_on(async move {
            table
                .delete("true")
                .await
                .map(|_| ())
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            if let Some(table) = vector_table {
                table
                    .delete("true")
                    .await
                    .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            }
            Ok(())
        })
    }
    pub fn replace_slices(&self, path: &PathScope, slices: &[Slice]) -> Result<()> {
        let table = self.table.clone();
        let path = path.0.as_str().to_owned();
        let rows = slices.to_owned();
        self.runtime.block_on(async move {
            table
                .delete(&format!("path = '{}'", path.replace('\'', "''")))
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            if !rows.is_empty() {
                table
                    .add(
                        batch_reader(&rows)
                            .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?,
                    )
                    .execute()
                    .await
                    .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            }
            Ok(())
        })
    }
    pub fn delete_path(&self, path: &PathScope) -> Result<()> {
        let table = self.table.clone();
        let vector_table = self.vector_table.clone();
        let path = path.0.as_str().to_owned();
        self.runtime.block_on(async move {
            table
                .delete(&format!("path = '{}'", path.replace('\'', "''")))
                .await
                .map(|_| ())
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            if let Some(table) = vector_table {
                table
                    .delete(&format!("path = '{}'", path.replace('\'', "''")))
                    .await
                    .map(|_| ())
                    .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            }
            Ok(())
        })
    }

    pub fn replace_vectors(
        &self,
        path: &PathScope,
        slices: &[Slice],
        vectors: &[Vec<f32>],
    ) -> Result<()> {
        let Some(table) = self.vector_table.clone() else {
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
        self.runtime.block_on(async move {
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
        })
    }

    pub fn vector_search(&self, query: &ContextQuery, vector: &[f32]) -> Result<Vec<RankedSlice>> {
        let Some(table) = self.vector_table.clone() else {
            return Ok(Vec::new());
        };
        if Some(vector.len()) != self.vector_dims || vector.iter().any(|v| !v.is_finite()) {
            return Err(crate::error::AgentWikiError::Embedding(
                "invalid query vector".into(),
            ));
        }
        let scope_filter = query.scope.trim().trim_end_matches('/').to_owned();
        let limit = query.limit.clamp(1, 20);
        let vector = vector.to_owned();
        self.runtime.block_on(async move {
            let mut builder = table
                .query()
                .nearest_to(vector.as_slice())
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            if !scope_filter.is_empty() {
                let scope = scope_filter.replace('\'', "''");
                builder = builder.only_if(format!("path = '{scope}' OR path LIKE '{scope}/%'"));
            }
            let mut stream = builder
                .select(Select::Columns(vec![
                    "path".into(),
                    "chunk_id".into(),
                    "ordinal".into(),
                    "section".into(),
                    "content".into(),
                    "source_hash".into(),
                ]))
                .limit(limit)
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            let mut out = Vec::new();
            while let Some(batch) = stream
                .try_next()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
            {
                let paths = batch
                    .column_by_name("path")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| crate::error::AgentWikiError::Index("missing path".into()))?;
                let ids = batch
                    .column_by_name("chunk_id")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| {
                        crate::error::AgentWikiError::Index("missing chunk_id".into())
                    })?;
                let sections = batch
                    .column_by_name("section")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| crate::error::AgentWikiError::Index("missing section".into()))?;
                let contents = batch
                    .column_by_name("content")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| crate::error::AgentWikiError::Index("missing content".into()))?;
                let hashes = batch
                    .column_by_name("source_hash")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                    .ok_or_else(|| {
                        crate::error::AgentWikiError::Index("missing source_hash".into())
                    })?;
                for i in 0..batch.num_rows() {
                    out.push(RankedSlice {
                        slice: Slice {
                            path: PathScope(paths.value(i).into()),
                            chunk_id: ids.value(i).into(),
                            ordinal: i as u32,
                            section: sections.value(i).into(),
                            content: contents.value(i).into(),
                            source_hash: hashes.value(i).into(),
                        },
                        score: 1.0 / (i as f64 + 1.0),
                        sources: vec!["semantic".into()],
                    });
                }
            }
            Ok(out)
        })
    }
    pub fn search(
        &self,
        query: &ContextQuery,
        _already_has_pending_vector: bool,
    ) -> Result<Vec<RankedSlice>> {
        if query.query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let table = self.table.clone();
        let text = query.query.clone();
        let scope_filter = query.scope.trim().trim_end_matches('/').to_owned();
        let limit = query.limit.clamp(1, 20);
        self.runtime.block_on(async move {
            let mut query_builder = table
                .query()
                .full_text_search(FullTextSearchQuery::new(text));
            if !scope_filter.is_empty() {
                let scope = scope_filter.replace('\'', "''");
                query_builder =
                    query_builder.only_if(format!("path = '{scope}' OR path LIKE '{scope}/%'"));
            }
            let mut stream = query_builder
                .select(Select::Columns(vec![
                    "path".into(),
                    "chunk_id".into(),
                    "ordinal".into(),
                    "section".into(),
                    "content".into(),
                    "source_hash".into(),
                ]))
                .limit(limit)
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            let mut out = Vec::new();
            while let Some(batch) = stream
                .try_next()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
            {
                let col = |name: &str| {
                    batch
                        .column_by_name(name)
                        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                        .ok_or_else(|| {
                            crate::error::AgentWikiError::Index(format!("missing {name}"))
                        })
                };
                let paths = col("path")?;
                let ids = col("chunk_id")?;
                let sections = col("section")?;
                let contents = col("content")?;
                let hashes = col("source_hash")?;
                for i in 0..batch.num_rows() {
                    let slice = Slice {
                        path: PathScope(paths.value(i).into()),
                        chunk_id: ids.value(i).into(),
                        ordinal: i as u32,
                        section: sections.value(i).into(),
                        content: contents.value(i).into(),
                        source_hash: hashes.value(i).into(),
                    };
                    out.push(RankedSlice {
                        slice,
                        score: 1.0 / (i as f64 + 1.0),
                        sources: vec!["keyword".into()],
                    });
                }
            }
            Ok(out)
        })
    }

    /// Return the indexed slices for one document path in source order.
    pub fn slices_for_path(&self, path: &str) -> Result<Vec<RankedSlice>> {
        let table = self.table.clone();
        let path = path.replace('\'', "''");
        self.runtime.block_on(async move {
            let mut stream = table
                .query()
                .only_if(format!("path = '{path}'"))
                .select(Select::Columns(vec![
                    "path".into(),
                    "chunk_id".into(),
                    "ordinal".into(),
                    "section".into(),
                    "content".into(),
                    "source_hash".into(),
                ]))
                .execute()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?;
            let mut out = Vec::new();
            while let Some(batch) = stream
                .try_next()
                .await
                .map_err(|e| crate::error::AgentWikiError::Index(e.to_string()))?
            {
                let col = |name: &str| {
                    batch
                        .column_by_name(name)
                        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                        .ok_or_else(|| {
                            crate::error::AgentWikiError::Index(format!("missing {name}"))
                        })
                };
                let paths = col("path")?;
                let ids = col("chunk_id")?;
                let sections = col("section")?;
                let contents = col("content")?;
                let hashes = col("source_hash")?;
                for i in 0..batch.num_rows() {
                    out.push(RankedSlice {
                        slice: Slice {
                            path: PathScope(paths.value(i).into()),
                            chunk_id: ids.value(i).into(),
                            ordinal: i as u32,
                            section: sections.value(i).into(),
                            content: contents.value(i).into(),
                            source_hash: hashes.value(i).into(),
                        },
                        score: 0.0,
                        sources: vec!["recency".into()],
                    });
                }
            }
            out.sort_by_key(|hit| hit.slice.ordinal);
            Ok(out)
        })
    }
    pub fn semantic_available(&self) -> bool {
        self.vector_table.is_some()
    }
}

async fn open_or_create(db: &Connection) -> lancedb::Result<Table> {
    match db.open_table(TABLE).execute().await {
        Ok(table) => Ok(table),
        Err(_) => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("path", DataType::Utf8, false),
                Field::new("chunk_id", DataType::Utf8, false),
                Field::new("ordinal", DataType::Int32, false),
                Field::new("section", DataType::Utf8, false),
                Field::new("content", DataType::Utf8, false),
                Field::new("source_hash", DataType::Utf8, false),
            ]));
            let empty = RecordBatch::new_empty(schema.clone());
            let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
                RecordBatchIterator::new(vec![Ok(empty)].into_iter(), schema.clone()),
            );
            let table = db.create_table(TABLE, reader).execute().await?;
            table
                .create_index(&["content"], Index::FTS(FtsIndexBuilder::default()))
                .execute()
                .await?;
            Ok(table)
        }
    }
}

async fn open_or_create_vectors(db: &Connection, dims: usize) -> lancedb::Result<Table> {
    let name = "vectors";
    match db.open_table(name).execute().await {
        Ok(table) => Ok(table),
        Err(_) => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("path", DataType::Utf8, false),
                Field::new("chunk_id", DataType::Utf8, false),
                Field::new("ordinal", DataType::Int32, false),
                Field::new("section", DataType::Utf8, false),
                Field::new("content", DataType::Utf8, false),
                Field::new("source_hash", DataType::Utf8, false),
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
            Ok(table)
        }
    }
}

fn batch_reader(rows: &[Slice]) -> lancedb::Result<Box<dyn arrow_array::RecordBatchReader + Send>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("ordinal", DataType::Int32, false),
        Field::new("section", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
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
        Field::new("ordinal", DataType::Int32, false),
        Field::new("section", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("source_hash", DataType::Utf8, false),
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
                rows.iter().map(|r| r.source_hash.as_str()),
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
