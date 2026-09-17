//! LanceDB backend boundary. Backend and Arrow types stay private here.
use crate::document::types::{PathScope, Slice};
use crate::error::Result;
use crate::retrieval::types::{ContextQuery, RankedSlice};
use arrow_array::types::Float32Type;
use arrow_array::{
    ArrayRef, FixedSizeListArray, Float32Array, Int32Array, RecordBatch, RecordBatchIterator,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use camino::Utf8Path;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lance_index::scalar::inverted::Language;
use lancedb::index::Index;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, Table, connect};
use std::sync::Arc;

const TABLE: &str = "chunks";

/// Lance FTS tokenizer for Chinese text, segmented with the bundled jieba
/// dictionary lookup (see [`ensure_language_model`]).
const FTS_TOKENIZER: &str = "jieba/default";
/// Dictionary path relative to the Lance language-model home directory.
const JIEBA_DICT_REL: &str = "jieba/default/dict.txt";

pub struct LanceIndex {
    table: Table,
    vector_table: Option<Table>,
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
        Ok(Self {
            table,
            vector_table,
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
    ) -> Result<Vec<RankedSlice>> {
        let Some(table) = &self.vector_table else {
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
                "_distance".into(),
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
                .ok_or_else(|| crate::error::AgentWikiError::Index("missing chunk_id".into()))?;
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
                .ok_or_else(|| crate::error::AgentWikiError::Index("missing source_hash".into()))?;
            // LanceDB reports raw (L2) distance in `_distance`; convert to
            // a bounded similarity and apply the calibrated semantic floor
            // so unrelated queries are not reported as hits (GAP-12).
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .ok_or_else(|| crate::error::AgentWikiError::Index("missing _distance".into()))?;
            for i in 0..batch.num_rows() {
                let score = 1.0 / (1.0 + f64::from(distances.value(i)));
                if score >= query.min_similarity {
                    out.push(RankedSlice {
                        slice: Slice {
                            path: PathScope(paths.value(i).into()),
                            chunk_id: ids.value(i).into(),
                            ordinal: i as u32,
                            section: sections.value(i).into(),
                            content: contents.value(i).into(),
                            source_hash: hashes.value(i).into(),
                        },
                        score,
                        sources: vec!["semantic".into()],
                        title: String::new(),
                        modified_at_ns: 0,
                        frontmatter: Default::default(),
                    });
                }
            }
        }
        Ok(out)
    }
    pub async fn search(
        &self,
        query: &ContextQuery,
        _already_has_pending_vector: bool,
    ) -> Result<Vec<RankedSlice>> {
        if query.query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let text = query.query.clone();
        let scope_filter = query.scope.trim().trim_end_matches('/').to_owned();
        let limit = query.limit.clamp(1, 20);
        let mut query_builder = self
            .table
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
                    .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
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
                    title: String::new(),
                    modified_at_ns: 0,
                    frontmatter: Default::default(),
                });
            }
        }
        Ok(out)
    }

    /// Return the indexed slices for one document path in source order.
    pub async fn slices_for_path(&self, path: &str) -> Result<Vec<RankedSlice>> {
        let path = path.replace('\'', "''");
        let mut stream = self
            .table
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
                    .ok_or_else(|| crate::error::AgentWikiError::Index(format!("missing {name}")))
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
                    title: String::new(),
                    modified_at_ns: 0,
                    frontmatter: Default::default(),
                });
            }
        }
        out.sort_by_key(|hit| hit.slice.ordinal);
        Ok(out)
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
            // Chinese text must be segmented, not split on whitespace: the
            // default `simple` tokenizer treats a whole sentence as one token
            // and query terms never match. `jieba` is dictionary-backed and
            // works offline via the language-model directory.
            // `Language` only drives stemming/stop-words and is ignored for
            // jieba; the default `English` is harmless here.
            let fts = FtsIndexBuilder::new(FTS_TOKENIZER.to_string(), Language::English);
            table
                .create_index(&["content"], Index::FTS(fts))
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
                    ordinal: 0,
                    section: "刷新令牌".into(),
                    content: "认证方案采用OAuth2协议，刷新令牌轮换策略30天。".into(),
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
                    false,
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
            ordinal: 0,
            section: String::new(),
            content: "semantic evidence".into(),
            source_hash: "hash".into(),
        }];
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
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sources, vec!["semantic"]);
    }
}
