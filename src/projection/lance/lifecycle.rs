//! Versioned derived-table lifecycle. Callers hold the projection's fs2 write lock.

use super::{PROJECTION_VERSION, PROJECTION_VERSION_KEY, TABLE, unified_schema};
use lancedb::index::scalar::{
    BTreeIndexBuilder, BitmapIndexBuilder, FtsIndexBuilder, LabelListIndexBuilder,
};
use lancedb::index::{Index, IndexType};
use lancedb::table::OptimizeAction;
use lancedb::{Connection, Error, Table};

/// Open the compatible projection, rebuilding only a known incompatible table.
/// The caller must hold the cross-process write lock throughout this operation.
pub(super) async fn open_or_create(db: &Connection, dims: Option<usize>) -> lancedb::Result<Table> {
    let expected = unified_schema(dims);
    match db.open_table(TABLE).execute().await {
        Ok(table) => {
            let actual = table.schema().await?;
            if actual == expected
                && actual
                    .metadata()
                    .get(PROJECTION_VERSION_KEY)
                    .map(String::as_str)
                    == Some(PROJECTION_VERSION)
            {
                return Ok(table);
            }
            // Only this derived table is disposable; open/schema errors never reach here.
            drop(table);
            db.drop_table(TABLE, &[]).await?;
        }
        Err(Error::TableNotFound { .. }) => {}
        Err(error) => return Err(error),
    }
    db.create_empty_table(TABLE, expected).execute().await
}

/// Retry missing index builds without replacing healthy or merely stale indices.
/// Indices are declared on the empty table too, so a query against a fresh or
/// emptied wiki stays a normal empty result instead of a failed search.
/// The caller must hold the cross-process write lock.
pub(super) async fn ensure_indexes(table: &Table) -> lancedb::Result<()> {
    let existing = table.list_indices().await?;
    let groups: [(&[&str], IndexType, Index); 4] = [
        (
            &["path", "chunk_id", "modified_at_ns", "target_path"],
            IndexType::BTree,
            Index::BTree(BTreeIndexBuilder::default()),
        ),
        (
            &["unit_kind", "type", "relation_type"],
            IndexType::Bitmap,
            Index::Bitmap(BitmapIndexBuilder::default()),
        ),
        (
            &["tags", "facets", "lookup_keys"],
            IndexType::LabelList,
            Index::LabelList(LabelListIndexBuilder::default()),
        ),
        (
            &["search_text"],
            IndexType::FTS,
            Index::FTS(FtsIndexBuilder::default()),
        ),
    ];
    for (columns, index_type, index) in groups {
        for column in columns {
            if existing.iter().any(|configured| {
                configured.index_type == index_type
                    && configured.columns.len() == 1
                    && configured.columns[0] == *column
            }) {
                continue;
            }
            // Each successful build commits independently; a later failure is retryable.
            table
                .create_index(&[column], index.clone())
                .execute()
                .await?;
        }
    }
    Ok(())
}

/// Incorporate unindexed rows without compaction or pruning historical versions.
/// Invoke under the write lock after initial sync/rebuild or explicit maintenance,
/// not on every query. Missing indices are handled separately by `ensure_indexes`.
pub(super) async fn optimize_indexes(table: &Table) -> lancedb::Result<()> {
    table
        .optimize(OptimizeAction::Index(Default::default()))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::builder::{ListBuilder, StringBuilder};
    use arrow_array::{ArrayRef, Int32Array, Int64Array, RecordBatch, StringArray, new_null_array};
    use arrow_schema::{DataType, SchemaRef};
    use std::sync::Arc;

    fn sample_batch(schema: SchemaRef) -> RecordBatch {
        let columns: Vec<ArrayRef> = schema
            .fields()
            .iter()
            .map(|field| match field.data_type() {
                DataType::Utf8 => {
                    Arc::new(StringArray::from(vec!["searchable sample"])) as ArrayRef
                }
                DataType::Int32 => Arc::new(Int32Array::from(vec![0])),
                DataType::Int64 => Arc::new(Int64Array::from(vec![0])),
                DataType::List(_) => {
                    let mut list = ListBuilder::new(StringBuilder::new());
                    list.values().append_value("sample");
                    list.append(true);
                    Arc::new(list.finish())
                }
                DataType::FixedSizeList(_, _) => new_null_array(field.data_type(), 1),
                unexpected => panic!("unsupported fixture field: {unexpected:?}"),
            })
            .collect();
        RecordBatch::try_new(schema, columns).unwrap()
    }

    async fn append_row(table: &Table) {
        let schema = table.schema().await.unwrap();
        table.add(sample_batch(schema)).execute().await.unwrap();
    }

    #[tokio::test]
    async fn incompatible_version_rebuilds_only_the_projection() {
        for old_version in [None, Some("1")] {
            let dir = tempfile::tempdir().unwrap();
            let db = lancedb::connect(dir.path().to_str().unwrap())
                .execute()
                .await
                .unwrap();
            let mut old_schema = unified_schema(None).as_ref().clone();
            old_schema.metadata.remove(PROJECTION_VERSION_KEY);
            if let Some(version) = old_version {
                old_schema
                    .metadata
                    .insert(PROJECTION_VERSION_KEY.into(), version.into());
            }
            let old = db
                .create_empty_table(TABLE, Arc::new(old_schema))
                .execute()
                .await
                .unwrap();
            append_row(&old).await;
            let unrelated = db
                .create_empty_table("unrelated", unified_schema(None))
                .execute()
                .await
                .unwrap();
            append_row(&unrelated).await;
            drop(old);

            let rebuilt = open_or_create(&db, None).await.unwrap();
            assert_eq!(rebuilt.count_rows(None).await.unwrap(), 0);
            assert_eq!(rebuilt.schema().await.unwrap(), unified_schema(None));
            assert_eq!(
                rebuilt.schema().await.unwrap().metadata()[PROJECTION_VERSION_KEY],
                PROJECTION_VERSION
            );
            assert_eq!(
                db.open_table("unrelated")
                    .execute()
                    .await
                    .unwrap()
                    .count_rows(None)
                    .await
                    .unwrap(),
                1
            );
            ensure_indexes(&rebuilt).await.unwrap();
            assert_eq!(rebuilt.list_indices().await.unwrap().len(), 11);
        }
    }

    #[tokio::test]
    async fn missing_indexes_are_repaired_without_refreshing_existing_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let db = lancedb::connect(dir.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let table = open_or_create(&db, None).await.unwrap();
        append_row(&table).await;
        // Simulate an interrupted build with only the first index committed.
        table
            .create_index(&["path"], Index::BTree(BTreeIndexBuilder::default()))
            .execute()
            .await
            .unwrap();
        ensure_indexes(&table).await.unwrap();
        let indexes = table.list_indices().await.unwrap();
        assert_eq!(indexes.len(), 11);
        assert!(!indexes.iter().any(|index| index.columns == ["source_size"]));
        let fts = indexes
            .iter()
            .find(|index| index.index_type == IndexType::FTS)
            .unwrap();
        table.drop_index(&fts.name).await.unwrap();
        ensure_indexes(&table).await.unwrap();
        assert_eq!(table.list_indices().await.unwrap().len(), 11);

        append_row(&table).await;
        let version = table.version().await.unwrap();
        ensure_indexes(&table).await.unwrap();
        assert_eq!(table.version().await.unwrap(), version);
        let reopened = open_or_create(&db, None).await.unwrap();
        assert_eq!(reopened.count_rows(None).await.unwrap(), 2);
        assert_eq!(reopened.version().await.unwrap(), version);

        let versions = table.list_versions().await.unwrap();
        optimize_indexes(&table).await.unwrap();
        let after = table.list_versions().await.unwrap();
        assert!(
            versions
                .iter()
                .all(|old| after.iter().any(|new| new.version == old.version))
        );
        assert_eq!(table.count_rows(None).await.unwrap(), 2);
    }
}
