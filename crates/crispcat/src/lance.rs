//! LanceDB-backed materialization of catalog entries.
//!
//! Phase 4b of PLAN P6 — option C ("hybrid storage"): the .caf file
//! stays the canonical persistent form; this module derives a light
//! search-friendly table from it on demand. Toggling a catalog
//! "active" calls [`materialize`]; toggling it off calls [`drop_catalog`].
//!
//! The table sits next to the existing `documents` table inside the
//! same LanceDB directory (`<data_dir>/lance/`). It's intentionally a
//! separate table — cataloging has no embedding column, no chunk
//! semantics, and a different lifecycle from sorted documents.
//! Cross-linking is left to a future query that joins the two on
//! `entry_path`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures_util::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Table};

use super::index::FileIndex;

const TABLE_NAME: &str = "catalog_entries";

/// Schema for the catalog table:
///
/// * `catalog_path` — absolute path to the source `.caf` file. Used as
///   the partitioning key for `drop_catalog` and as the badge label
///   in search results.
/// * `entry_path`   — full file path inside the cataloged drive.
/// * `filename`     — `entry_path.file_name()`, denormalized for fast
///   name-only search without parsing.
/// * `size`         — bytes (Int64; Arrow's native UInt support is
///   uneven across LanceDB versions, Int64 fits real-world filesizes).
/// * `mtime`        — unix epoch seconds (Int64 for consistency with
///   `size`).
/// * `hash`         — hex digest if computed during scan, else NULL.
fn build_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("catalog_path", DataType::Utf8, false),
        Field::new("entry_path", DataType::Utf8, false),
        Field::new("filename", DataType::Utf8, true),
        Field::new("size", DataType::Int64, false),
        Field::new("mtime", DataType::Int64, false),
        Field::new("hash", DataType::Utf8, true),
    ]))
}

/// Open or create the `catalog_entries` table. Idempotent — calling
/// this for a non-existent table creates an empty one with the schema
/// above.
pub async fn open_or_create(data_dir: &Path) -> Result<Table> {
    let lance_dir = data_dir.join("lance");
    std::fs::create_dir_all(&lance_dir)
        .with_context(|| format!("creating LanceDB dir {}", lance_dir.display()))?;
    let uri = lance_dir.to_string_lossy().into_owned();
    let db = connect(&uri).execute().await.context("connecting to LanceDB")?;

    match db.open_table(TABLE_NAME).execute().await {
        Ok(t) => Ok(t),
        Err(e) if format!("{e}").contains("not found") => db
            .create_empty_table(TABLE_NAME, build_schema())
            .execute()
            .await
            .context("creating catalog_entries table"),
        Err(e) => Err(e.into()),
    }
}

/// Insert every entry from `index` into `catalog_entries`, tagged with
/// `catalog_path`. Replaces any prior rows for the same catalog (calls
/// `drop_catalog` first) so re-materializing after a refresh is a
/// single idempotent op.
pub async fn materialize(
    data_dir: &Path,
    catalog_path: &Path,
    index: &FileIndex,
) -> Result<usize> {
    let table = open_or_create(data_dir).await?;
    drop_catalog_in(&table, catalog_path).await?;

    if index.all_files.is_empty() {
        return Ok(0);
    }

    let cap = index.all_files.len();
    let mut catalog_paths: Vec<&str> = Vec::with_capacity(cap);
    let mut entry_paths: Vec<String> = Vec::with_capacity(cap);
    let mut filenames: Vec<Option<String>> = Vec::with_capacity(cap);
    let mut sizes: Vec<i64> = Vec::with_capacity(cap);
    let mut mtimes: Vec<i64> = Vec::with_capacity(cap);
    let mut hashes: Vec<Option<String>> = Vec::with_capacity(cap);

    let cp_str = catalog_path.to_string_lossy().into_owned();
    for entry in &index.all_files {
        catalog_paths.push(&cp_str);
        entry_paths.push(entry.path.to_string_lossy().into_owned());
        filenames.push(
            entry
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
        );
        // Lance/Arrow `Int64` is the safest portable size column. We
        // accept the (theoretical) loss of bytes > 8 EiB.
        sizes.push(entry.size.min(i64::MAX as u64) as i64);
        mtimes.push(entry.mtime as i64);
        hashes.push(entry.hash.clone());
    }

    let schema = build_schema();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(catalog_paths)),
            Arc::new(StringArray::from(entry_paths)),
            Arc::new(StringArray::from_iter(filenames.iter().map(|s| s.as_deref()))),
            Arc::new(Int64Array::from(sizes)),
            Arc::new(Int64Array::from(mtimes)),
            Arc::new(StringArray::from_iter(hashes.iter().map(|s| s.as_deref()))),
        ],
    )
    .context("building catalog_entries record batch")?;

    let inserted = batch.num_rows();
    let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], schema);
    table
        .add(Box::new(reader))
        .execute()
        .await
        .context("inserting catalog_entries rows")?;
    Ok(inserted)
}

/// Drop every row tagged with this catalog. Handy after the .caf has
/// been refreshed (call `materialize` to re-insert) or when the user
/// toggles the catalog inactive in the UI.
pub async fn drop_catalog(data_dir: &Path, catalog_path: &Path) -> Result<()> {
    let table = open_or_create(data_dir).await?;
    drop_catalog_in(&table, catalog_path).await
}

async fn drop_catalog_in(table: &Table, catalog_path: &Path) -> Result<()> {
    let cp = catalog_path.to_string_lossy();
    // Lance's predicate uses a single-quoted SQL-ish filter. Embedded
    // single quotes get doubled.
    let pred = format!("catalog_path = '{}'", cp.replace('\'', "''"));
    table.delete(&pred).await.context("dropping catalog rows")?;
    Ok(())
}

/// One row in a catalog search result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogHit {
    pub catalog_path: String,
    pub entry_path: String,
    pub filename: Option<String>,
    pub size: i64,
    pub mtime: i64,
    pub hash: Option<String>,
}

/// Search across active catalog entries. The query is matched against
/// the `filename` column via case-insensitive substring (cheap for the
/// initial cut; Phase 4c will swap this for a Tantivy index over the
/// path components).
///
/// `limit` is enforced by LanceDB; pass `None` for "no cap" but expect
/// large returns on broad queries.
pub async fn search(data_dir: &Path, query: &str, limit: Option<usize>) -> Result<Vec<CatalogHit>> {
    let table = open_or_create(data_dir).await?;
    // Arrow's compute kernels accept LIKE; case-insensitive fold via
    // `lower()` on both sides.
    let q = query.to_lowercase().replace('\'', "''");
    let pred = if q.is_empty() {
        // Empty query → return everything (capped).
        None
    } else {
        Some(format!("lower(filename) LIKE '%{q}%'"))
    };

    let mut select = table.query();
    if let Some(p) = &pred {
        select = select.only_if(p);
    }
    if let Some(n) = limit {
        select = select.limit(n);
    }
    let mut stream = select.execute().await.context("querying catalog_entries")?;
    let mut out: Vec<CatalogHit> = Vec::new();
    while let Some(batch) = stream.try_next().await.context("draining stream")? {
        let cp = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("col0 not StringArray")?;
        let ep = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("col1 not StringArray")?;
        let fname = batch
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("col2 not StringArray")?;
        let size = batch
            .column(3)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("col3 not Int64Array")?;
        let mtime = batch
            .column(4)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("col4 not Int64Array")?;
        let hash = batch
            .column(5)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("col5 not StringArray")?;
        for i in 0..batch.num_rows() {
            out.push(CatalogHit {
                catalog_path: cp.value(i).to_string(),
                entry_path: ep.value(i).to_string(),
                filename: if fname.is_null(i) {
                    None
                } else {
                    Some(fname.value(i).to_string())
                },
                size: size.value(i),
                mtime: mtime.value(i),
                hash: if hash.is_null(i) {
                    None
                } else {
                    Some(hash.value(i).to_string())
                },
            });
        }
    }
    Ok(out)
}

/// Distinct catalog paths currently materialized in the table — useful
/// for verifying which catalogs are actually active in the search index
/// without trusting the frontend's settings store.
pub async fn list_active(data_dir: &Path) -> Result<Vec<String>> {
    let table = open_or_create(data_dir).await?;
    let mut stream = table
        .query()
        .select(lancedb::query::Select::columns(&["catalog_path"]))
        .execute()
        .await
        .context("listing catalog_paths")?;
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    while let Some(batch) = stream.try_next().await.context("draining stream")? {
        let cp = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("col0 not StringArray")?;
        for i in 0..batch.num_rows() {
            seen.insert(cp.value(i).to_string());
        }
    }
    Ok(seen.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileEntry;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fake_index(name: &str, files: &[(&str, u64, u32)]) -> FileIndex {
        let mut idx = FileIndex::new(PathBuf::from(format!("/{name}")), false);
        for (n, size, mtime) in files {
            idx.add(FileEntry::new(
                PathBuf::from(format!("/{name}/{n}")),
                *size,
                *mtime,
            ));
        }
        idx
    }

    #[tokio::test]
    async fn materialize_then_search_finds_rows() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();
        let caf_path = PathBuf::from("/tmp/test.caf");
        let idx = fake_index(
            "drive1",
            &[
                ("photos/sunset.jpg", 12345, 1700000000),
                ("photos/beach.jpg", 67890, 1700000001),
                ("docs/notes.md", 100, 1700000002),
            ],
        );
        let n = materialize(data_dir, &caf_path, &idx).await.unwrap();
        assert_eq!(n, 3);

        // Substring-on-filename search.
        let hits = search(data_dir, "sunset", Some(10)).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry_path.ends_with("sunset.jpg"));

        // Empty query returns everything (within the limit).
        let all = search(data_dir, "", Some(100)).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn rematerialize_replaces_prior_rows() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();
        let caf_path = PathBuf::from("/tmp/test.caf");

        let idx1 = fake_index("drive1", &[("a.txt", 100, 1)]);
        materialize(data_dir, &caf_path, &idx1).await.unwrap();

        let idx2 = fake_index("drive1", &[("b.txt", 200, 1), ("c.txt", 300, 1)]);
        materialize(data_dir, &caf_path, &idx2).await.unwrap();

        // a.txt should be gone (overwritten); b + c present.
        let all = search(data_dir, "", Some(100)).await.unwrap();
        let names: std::collections::HashSet<_> =
            all.iter().filter_map(|h| h.filename.clone()).collect();
        assert!(!names.contains("a.txt"));
        assert!(names.contains("b.txt"));
        assert!(names.contains("c.txt"));
    }

    #[tokio::test]
    async fn drop_catalog_removes_only_that_catalog() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();
        let caf_a = PathBuf::from("/tmp/a.caf");
        let caf_b = PathBuf::from("/tmp/b.caf");

        materialize(data_dir, &caf_a, &fake_index("a", &[("file_a.txt", 1, 1)]))
            .await
            .unwrap();
        materialize(data_dir, &caf_b, &fake_index("b", &[("file_b.txt", 2, 2)]))
            .await
            .unwrap();
        assert_eq!(list_active(data_dir).await.unwrap().len(), 2);

        drop_catalog(data_dir, &caf_a).await.unwrap();
        let actives = list_active(data_dir).await.unwrap();
        assert_eq!(actives.len(), 1);
        assert!(actives[0].ends_with("b.caf"));
    }
}
