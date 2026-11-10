//! A stable, bounded document scan over one immutable table version.
use super::{batches_to_results, sql_escape, VectorStore, TABLE_NAME};
use anyhow::{anyhow, ensure, Context, Result};
use arrow_array::{Array, StringArray, TimestampMillisecondArray};
use crisp_index_protocol::{SyncCursor, SyncPage, SYNC_CURSOR_VERSION};
use futures_util::TryStreamExt;
use lance::dataset::scanner::ColumnOrdering;

#[derive(Debug)]
pub(crate) struct SyncSnapshotUnavailable;

impl std::fmt::Display for SyncSnapshotUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "The sync snapshot is no longer available; restart from the last completed timestamp",
        )
    }
}

impl std::error::Error for SyncSnapshotUnavailable {}

impl VectorStore {
    pub(super) async fn read_sync_page(
        &self,
        since_ms: i64,
        cursor: Option<&SyncCursor>,
        limit: usize,
    ) -> Result<SyncPage> {
        ensure!(
            (1..=1000).contains(&limit),
            "Sync page limit must be between 1 and 1000"
        );
        ensure!(since_ms >= 0, "Sync timestamp cannot be negative");
        if let Some(cursor) = cursor {
            ensure!(
                cursor.is_valid() && cursor.since_ms == since_ms,
                "Invalid or mismatched sync cursor"
            );
        }

        // Opening a new handle is intentional: Table::clone shares its mutable
        // checkout state and would make the live writer read-only.
        let table = self._db.open_table(TABLE_NAME).execute().await?;
        let snapshot_version = match cursor {
            Some(cursor) => cursor.snapshot_version,
            None => self.table.version().await?,
        };
        table
            .checkout(snapshot_version)
            .await
            .map_err(|error| anyhow!(SyncSnapshotUnavailable).context(error))?;
        let mut scanner = {
            let dataset = table
                .dataset()
                .ok_or_else(|| anyhow!("Sync requires a native table"))?
                .get()
                .await
                .context("Opening sync snapshot")?;
            dataset.scan()
        };
        let mut filter =
            format!("chunk_index = 0 AND indexed_at >= to_timestamp_millis({since_ms})");
        if let Some(cursor) = cursor {
            let time = cursor.indexed_at;
            let owner = sql_escape(&cursor.owner_id);
            let doc = sql_escape(&cursor.doc_id);
            filter.push_str(&format!(
                " AND (indexed_at > to_timestamp_millis({time}) OR (indexed_at = to_timestamp_millis({time}) AND (owner_id > '{owner}' OR (owner_id = '{owner}' AND doc_id > '{doc}'))))"
            ));
        }
        scanner.filter(&filter)?;
        scanner.order_by(Some(vec![
            ColumnOrdering::asc_nulls_last("indexed_at".into()),
            ColumnOrdering::asc_nulls_last("owner_id".into()),
            ColumnOrdering::asc_nulls_last("doc_id".into()),
        ]))?;
        scanner.limit(Some((limit + 1) as i64), None)?;
        scanner.project(&[
            "doc_id",
            "location_uri",
            "owner_id",
            "title",
            "author",
            "year",
            "filename",
            "ext",
            "language",
            "chunk_index",
            "indexed_at",
        ])?;
        let batches = scanner
            .try_into_stream()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        let mut positions = Vec::new();
        for batch in &batches {
            let times = batch
                .column_by_name("indexed_at")
                .and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>())
                .ok_or_else(|| anyhow!("Sync snapshot is missing indexed_at"))?;
            let owners = batch
                .column_by_name("owner_id")
                .and_then(|column| column.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| anyhow!("Sync snapshot is missing owner_id"))?;
            let docs = batch
                .column_by_name("doc_id")
                .and_then(|column| column.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| anyhow!("Sync snapshot is missing doc_id"))?;
            for row in 0..batch.num_rows() {
                ensure!(
                    !times.is_null(row)
                        && !owners.is_null(row)
                        && !docs.is_null(row)
                        && !docs.value(row).is_empty(),
                    "Sync snapshot contains an invalid identity"
                );
                positions.push((
                    times.value(row),
                    owners.value(row).to_owned(),
                    docs.value(row).to_owned(),
                ));
            }
        }
        let mut rows = batches_to_results(&batches, None, false)?;
        ensure!(
            rows.len() == positions.len(),
            "Sync projection omitted a stored row"
        );
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        positions.truncate(limit);
        let max_indexed_at = positions.last().map_or(
            cursor.map_or(since_ms, |cursor| cursor.indexed_at),
            |position| position.0,
        );
        let next_cursor = if has_more {
            let (indexed_at, owner_id, doc_id) = positions.last().expect("a nonempty page");
            Some(serde_json::to_string(&SyncCursor {
                version: SYNC_CURSOR_VERSION,
                snapshot_version,
                since_ms,
                indexed_at: *indexed_at,
                owner_id: owner_id.clone(),
                doc_id: doc_id.clone(),
            })?)
        } else {
            None
        };
        Ok(SyncPage {
            rows,
            max_indexed_at,
            has_more,
            next_cursor,
            snapshot_version: Some(snapshot_version),
        })
    }
}
