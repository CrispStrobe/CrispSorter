//! Exhaustive lexical totals over a consistent pair of local index snapshots.
//!
//! Metadata rows stream in bounded batches; document/facet memberships live in
//! a private temporary SQLite database instead of a corpus-sized in-memory set.
//! The interactive result limit never limits this scan. Catalog filename
//! references and semantic similarity are separate retrieval channels.
use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, atomic::{AtomicBool, Ordering}}};

use anyhow::{bail, ensure, Context, Result};
use arrow_array::{Array, Int32Array, ListArray, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions, Select};
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::index::{fts_index::PreparedFtsSearch, schema::SearchFilters, FtsIndex, LocalIndex};

const BATCH_ROWS: u32 = 512;
pub const FACET_VALUE_LIMIT: usize = 25;

#[derive(Default)]
pub struct ScanCancellation {
    cancelled: AtomicBool,
    changed: tokio::sync::Notify,
}
impl ScanCancellation {
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); self.changed.notify_waiters(); }
    async fn cancelled(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.cancelled.load(Ordering::Acquire) { return; }
            changed.await;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetValue {
    pub value: String,
    pub documents: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetSummary {
    pub values: Vec<FacetValue>,
    pub distinct_values: u64,
    pub omitted_values: u64,
    pub missing_documents: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFacets {
    pub extensions: FacetSummary,
    pub languages: FacetSummary,
    pub years: FacetSummary,
    pub tags: FacetSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSummary {
    pub version: u32,
    pub scope: &'static str,
    pub query: String,
    pub effective_query: String,
    pub generation: String,
    pub matching_documents: u64,
    pub scanned_rows: u64,
    pub facet_value_limit: usize,
    pub facets: SearchFacets,
}

/// The token is opaque and includes the index's identity, not just its row
/// version: switching to another index with the same version invalidates it.
async fn generation_under_guard(local: &LocalIndex, fts: &FtsIndex, volumes: Option<&[String]>) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(local.ingest_journal_path().to_string_lossy().as_bytes());
    hash.update([0]);
    hash.update(local.table_ref().version().await?.to_le_bytes());
    hash.update(fts.index.load_metas()?.opstamp.to_le_bytes());
    let volumes = volumes.map(|values| values.iter().collect::<BTreeSet<_>>());
    hash.update(serde_json::to_vec(&volumes)?);
    Ok(hex::encode(hash.finalize()))
}

pub async fn generation(local: &LocalIndex, fts: &FtsIndex, volumes: Option<&[String]>) -> Result<String> {
    let _guard = local.read_guard(fts).await?;
    generation_under_guard(local, fts, volumes).await
}

/// Uses the same synonym/fuzzy expansion, parsed positional query and Lance
/// metadata predicates as local Full text search. No relevance cutoff applies.
pub async fn summarize(
    local: Arc<LocalIndex>, fts: Arc<FtsIndex>, query: String, filters: SearchFilters,
    cancellation: Option<Arc<ScanCancellation>>,
) -> Result<SearchSummary> {
    ensure!(!query.trim().is_empty() && query.len() <= 8192,
        "Enter a search query of at most 8192 bytes");
    check_cancelled(&cancellation)?;
    let _guard = if let Some(cancellation) = &cancellation {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => bail!("The matching-document scan was cancelled"),
            guard = local.read_guard(&fts) => guard?,
        }
    } else { local.read_guard(&fts).await? };
    check_cancelled(&cancellation)?;
    let generation = generation_under_guard(&local, &fts, filters.mounted_volume_ids.as_deref()).await?;
    let mut effective_query = if filters.synonyms {
        crate::index::synonyms::synonym_expand_query(&query)
    } else { query.clone() };
    if filters.fuzzy { effective_query = crate::index::fts_query::fuzzify_query(&effective_query); }
    let prepare_query = effective_query.clone();
    let prepare_filters = filters.clone();
    let prepare_fts = fts.clone();
    let prepared = Arc::new(tokio::task::spawn_blocking(move ||
        prepare_fts.prepare_search(&prepare_query, &prepare_filters)).await??);
    // Valid legacy documents may start at any stored chunk index. Match every
    // extracted row and deduplicate source identities in temporary storage.
    let predicate = match filters.to_lance_sql() {
        Some(sql) => format!("chunk_index >= 0 AND ({sql})"),
        None => "chunk_index >= 0".into(),
    };
    let mut options = QueryExecutionOptions::default();
    options.max_batch_length = BATCH_ROWS;
    let mut rows = local.table_ref().query().only_if(predicate)
        .select(Select::Columns(["doc_id", "owner_id", "chunk_index", "ext", "language", "year", "tags"]
            .into_iter().map(str::to_owned).collect()))
        .execute_with_options(options).await.context("Scanning matching document metadata")?;
    let scan_cancellation = cancellation.clone();
    let mut accumulator = tokio::task::spawn_blocking(move || Accumulator::new(scan_cancellation)).await??;
    while let Some(batch) = rows.try_next().await? {
        check_cancelled(&cancellation)?;
        let search = prepared.clone();
        let batch_cancellation = cancellation.clone();
        let interrupt = accumulator.database.get_interrupt_handle();
        accumulator = await_sqlite(tokio::task::spawn_blocking(move || {
            accumulator.add_batch(&batch, &search, &batch_cancellation)?;
            Ok::<_, anyhow::Error>(accumulator)
        }), interrupt, &cancellation).await?;
    }
    let interrupt = accumulator.database.get_interrupt_handle();
    let (matching_documents, scanned_rows, facets) = await_sqlite(tokio::task::spawn_blocking(move ||
        accumulator.finish()), interrupt, &cancellation).await?;
    check_cancelled(&cancellation)?;
    // The shared read guard excludes normal writers. This also catches an
    // out-of-process or legacy write that did not participate in that guard.
    ensure!(generation_under_guard(&local, &fts, filters.mounted_volume_ids.as_deref()).await? == generation,
        "The index changed while counting matches. Search again to refresh the summary.");
    Ok(SearchSummary { version: 1, scope: "local_indexed_documents", query, effective_query,
        generation, matching_documents, scanned_rows, facet_value_limit: FACET_VALUE_LIMIT, facets })
}

fn check_cancelled(cancellation: &Option<Arc<ScanCancellation>>) -> Result<()> {
    ensure!(!cancellation.as_ref().is_some_and(|flag| flag.cancelled.load(Ordering::Acquire)),
        "The matching-document scan was cancelled");
    Ok(())
}

async fn await_sqlite<T>(mut work: tokio::task::JoinHandle<Result<T>>, interrupt: rusqlite::InterruptHandle,
    cancellation: &Option<Arc<ScanCancellation>>) -> Result<T> {
    if let Some(cancellation) = cancellation {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                interrupt.interrupt();
                let _ = work.await;
                bail!("The matching-document scan was cancelled");
            },
            value = &mut work => return value?,
        }
    }
    work.await?
}

struct MetadataRow {
    doc_id: String,
    owner_id: String,
    chunk_index: i32,
    extension: Option<String>,
    language: Option<String>,
    year: Option<i32>,
    tags: Vec<String>,
}

fn metadata_rows(batch: &RecordBatch) -> Result<Vec<MetadataRow>> {
    let strings = |name: &str| batch.column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .with_context(|| format!("The indexed {name} column is invalid"));
    let ids = strings("doc_id")?;
    let owners = strings("owner_id")?;
    let indices = batch.column_by_name("chunk_index").and_then(|column| column.as_any().downcast_ref::<Int32Array>())
        .context("The indexed chunk column is invalid")?;
    let extensions = strings("ext")?;
    let languages = strings("language")?;
    let years = batch.column_by_name("year").and_then(|column| column.as_any().downcast_ref::<Int32Array>())
        .context("The indexed year column is invalid")?;
    let tag_lists = batch.column_by_name("tags").and_then(|column| column.as_any().downcast_ref::<ListArray>())
        .context("The indexed tags column is invalid")?;
    let value = |column: &StringArray, row| (!column.is_null(row) && !column.value(row).is_empty())
        .then(|| column.value(row).to_owned());
    let mut rows = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        ensure!(!ids.is_null(row) && !ids.value(row).is_empty() && !owners.is_null(row),
            "An indexed document has no usable source identity");
        let mut tags = BTreeSet::new();
        if !tag_lists.is_null(row) {
            let values = tag_lists.value(row);
            let values = values.as_any().downcast_ref::<StringArray>().context("The indexed tag values are invalid")?;
            for tag in values.iter().flatten().filter(|value| !value.is_empty()) { tags.insert(tag.to_owned()); }
        }
        rows.push(MetadataRow { doc_id: ids.value(row).to_owned(), owner_id: owners.value(row).to_owned(), chunk_index: indices.value(row),
            extension: value(extensions, row), language: value(languages, row),
            year: (!years.is_null(row)).then(|| years.value(row)), tags: tags.into_iter().collect() });
    }
    Ok(rows)
}

struct Accumulator {
    database: Connection,
    // Keep the private file alive until the SQLite connection is closed.
    _file: tempfile::NamedTempFile,
    scanned_rows: u64,
}

impl Accumulator {
    fn new(cancellation: Option<Arc<ScanCancellation>>) -> Result<Self> {
        let file = tempfile::NamedTempFile::new().context("Creating temporary search summary storage")?;
        let database = Connection::open(file.path())?;
        if let Some(cancellation) = cancellation {
            database.progress_handler(1000, Some(move || cancellation.cancelled.load(Ordering::Acquire)))?;
        }
        database.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE;
            PRAGMA cache_size=-2048; PRAGMA mmap_size=0;
            CREATE TABLE documents (owner TEXT NOT NULL, doc_id TEXT NOT NULL, representative_key TEXT NOT NULL,
                PRIMARY KEY(owner,doc_id)) WITHOUT ROWID;
            CREATE TABLE facets (kind TEXT NOT NULL, value TEXT NOT NULL, owner TEXT NOT NULL, doc_id TEXT NOT NULL,
                PRIMARY KEY(kind,value,owner,doc_id)) WITHOUT ROWID;
            CREATE INDEX facet_documents ON facets(kind,owner,doc_id);")?;
        Ok(Self { database, _file: file, scanned_rows: 0 })
    }

    fn add_batch(&mut self, batch: &RecordBatch, search: &PreparedFtsSearch,
        cancellation: &Option<Arc<ScanCancellation>>) -> Result<()> {
        ensure!(batch.num_rows() <= BATCH_ROWS as usize, "The metadata scan exceeded its batch bound");
        self.scanned_rows = self.scanned_rows.checked_add(batch.num_rows() as u64).context("Search row count overflow")?;
        let rows = metadata_rows(batch)?;
        let mut owners = BTreeMap::<&str, Vec<String>>::new();
        for row in &rows { owners.entry(&row.owner_id).or_default().push(row.doc_id.clone()); }
        let mut allowed = BTreeSet::new();
        for (owner, ids) in owners {
            check_cancelled(cancellation)?;
            for id in search.matching_doc_ids_for_owner(&ids, owner)? {
                allowed.insert((owner, id));
            }
        }
        let transaction = self.database.transaction()?;
        {
            let mut document = transaction.prepare_cached("INSERT INTO documents(owner,doc_id,representative_key) VALUES (?1,?2,?3)
                ON CONFLICT(owner,doc_id) DO UPDATE SET representative_key=excluded.representative_key
                WHERE excluded.representative_key < documents.representative_key")?;
            let mut facet = transaction.prepare_cached("INSERT OR IGNORE INTO facets(kind,value,owner,doc_id) VALUES (?1,?2,?3,?4)")?;
            let mut replace = transaction.prepare_cached("DELETE FROM facets WHERE owner=?1 AND doc_id=?2 AND kind!='tag'")?;
            for row in &rows {
                check_cancelled(cancellation)?;
                if !allowed.contains(&(row.owner_id.as_str(), row.doc_id.clone())) { continue; }
                // Scalars use the lowest matching stored chunk, with a stable
                // tie-break for inconsistent duplicate legacy rows. Tags are a
                // per-document union, so repetitions never increase a count.
                let representative = format!("{:010}:{}", row.chunk_index,
                    serde_json::to_string(&(&row.extension, &row.language, row.year))?);
                if document.execute(params![row.owner_id, row.doc_id, representative])? > 0 {
                    replace.execute(params![row.owner_id, row.doc_id])?;
                    for (kind, value) in [("extension", row.extension.clone()), ("language", row.language.clone()),
                        ("year", row.year.map(|year| year.to_string()))] {
                        if let Some(value) = value { facet.execute(params![kind, value, row.owner_id, row.doc_id])?; }
                    }
                }
                for tag in &row.tags { facet.execute(params!["tag", tag, row.owner_id, row.doc_id])?; }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn facet(&self, kind: &str) -> Result<FacetSummary> {
        let distinct_values = self.database.query_row(
            "SELECT COUNT(*) FROM (SELECT value FROM facets WHERE kind=?1 GROUP BY value)", [kind], |row| row.get::<_, i64>(0))? as u64;
        let missing_documents = self.database.query_row(
            "SELECT COUNT(*) FROM documents d WHERE NOT EXISTS (SELECT 1 FROM facets f
                WHERE f.kind=?1 AND f.owner=d.owner AND f.doc_id=d.doc_id)", [kind], |row| row.get::<_, i64>(0))? as u64;
        let mut statement = self.database.prepare(
            "SELECT value, COUNT(*) AS documents FROM facets WHERE kind=?1 GROUP BY value
                ORDER BY documents DESC, value ASC LIMIT ?2")?;
        let values = statement.query_map(params![kind, FACET_VALUE_LIMIT as i64], |row| Ok(FacetValue {
            value: row.get(0)?, documents: row.get::<_, i64>(1)? as u64,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(FacetSummary { omitted_values: distinct_values.saturating_sub(values.len() as u64),
            values, distinct_values, missing_documents })
    }

    fn finish(self) -> Result<(u64, u64, SearchFacets)> {
        let matching_documents = self.database.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get::<_, i64>(0))? as u64;
        Ok((matching_documents, self.scanned_rows, SearchFacets {
            extensions: self.facet("extension")?, languages: self.facet("language")?,
            years: self.facet("year")?, tags: self.facet("tag")?,
        }))
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_aggregation_is_cooperatively_interruptible() {
        let cancellation = Arc::new(ScanCancellation::default());
        let accumulator = Accumulator::new(Some(cancellation.clone())).unwrap();
        let worker = tokio::task::spawn_blocking(move || accumulator.database.query_row(
            "WITH RECURSIVE values_to_count(value) AS (VALUES(0) UNION ALL
                SELECT value+1 FROM values_to_count WHERE value<1000000000)
                SELECT COUNT(*) FROM values_to_count", [], |row| row.get::<_, i64>(0)));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), worker).await.unwrap().unwrap();
        assert!(matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::OperationInterrupted));
    }
}
