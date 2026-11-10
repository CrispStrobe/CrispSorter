//! Exhaustive lexical totals over a consistent pair of local index snapshots.
//!
//! Metadata rows stream in bounded batches; document/facet memberships live in
//! a private temporary SQLite database instead of a corpus-sized in-memory set.
//! The interactive result limit never limits this scan. Catalog filename
//! references and semantic similarity are separate retrieval channels.
use std::{collections::{BTreeMap, BTreeSet}, io::{BufWriter, Write}, path::PathBuf, sync::{Arc, atomic::{AtomicU8, Ordering}}};

use anyhow::{bail, ensure, Context, Result};
use arrow_array::{Array, Int32Array, ListArray, RecordBatch, StringArray, TimestampMillisecondArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions, Select};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::index::{fts_index::PreparedFtsSearch, schema::SearchFilters, FtsIndex, LocalIndex};

const BATCH_ROWS: u32 = 512;
pub const FACET_VALUE_LIMIT: usize = 25;

#[derive(Default)]
pub struct ScanCancellation {
    phase: AtomicU8,
    changed: tokio::sync::Notify,
}
impl ScanCancellation {
    /// Once the atomic publication begins it must report its actual outcome;
    /// callers cannot claim cancellation after the destination was replaced.
    pub fn cancel(&self) -> bool {
        match self.phase.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => { self.changed.notify_waiters(); true },
            Err(1) => true,
            Err(_) => false,
        }
    }
    fn begin_publish(&self) -> Result<()> {
        self.phase.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("The report was cancelled before publication"))?;
        Ok(())
    }
    fn finish_publish(&self) { self.phase.store(3, Ordering::Release); }
    async fn cancelled(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.phase.load(Ordering::Acquire) == 1 { return; }
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

async fn acquire_read_guard(local: &LocalIndex, fts: &FtsIndex, cancellation: &Option<Arc<ScanCancellation>>) -> Result<crate::index::ingest::IndexWriteGuard> {
    check_cancelled(cancellation)?;
    let guard = if let Some(cancellation) = cancellation {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => bail!("The matching-document scan was cancelled"),
            guard = local.read_guard(fts) => guard?,
        }
    } else { local.read_guard(fts).await? };
    check_cancelled(cancellation)?;
    Ok(guard)
}

/// Uses the same synonym/fuzzy expansion, parsed positional query and Lance
/// metadata predicates as local Full text search. No relevance cutoff applies.
async fn scan_documents(
    local: Arc<LocalIndex>, fts: Arc<FtsIndex>, query: String, filters: SearchFilters,
    cancellation: Option<Arc<ScanCancellation>>,
) -> Result<(Accumulator, String)> {
    ensure!(!query.trim().is_empty() && query.len() <= 8192,
        "Enter a search query of at most 8192 bytes");
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
        .select(Select::Columns(["doc_id", "owner_id", "chunk_index", "ext", "language", "year", "tags",
            "title", "filename", "author", "location_uri", "url", "source_hash", "indexed_at"]
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
    Ok((accumulator, effective_query))
}

pub async fn summarize(
    local: Arc<LocalIndex>, fts: Arc<FtsIndex>, query: String, filters: SearchFilters,
    cancellation: Option<Arc<ScanCancellation>>,
) -> Result<SearchSummary> {
    let _guard = acquire_read_guard(&local, &fts, &cancellation).await?;
    let generation = generation_under_guard(&local, &fts, filters.mounted_volume_ids.as_deref()).await?;
    let (accumulator, effective_query) = scan_documents(local.clone(), fts.clone(), query.clone(), filters.clone(), cancellation.clone()).await?;
    let interrupt = accumulator.database.get_interrupt_handle();
    let (matching_documents, scanned_rows, facets) = await_sqlite(tokio::task::spawn_blocking(move ||
        accumulator.finish()), interrupt, &cancellation).await?;
    check_cancelled(&cancellation)?;
    ensure!(generation_under_guard(&local, &fts, filters.mounted_volume_ids.as_deref()).await? == generation,
        "The index changed while counting matches. Search again to refresh the summary.");
    Ok(SearchSummary { version: 1, scope: "local_indexed_documents", query, effective_query,
        generation, matching_documents, scanned_rows, facet_value_limit: FACET_VALUE_LIMIT, facets })
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportFormat { Csv, Json }

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportSort { Filename, Title, YearAsc, YearDesc, Location }
impl ReportSort {
    fn sql(self) -> &'static str {
        match self {
            Self::Filename => "sort_filename COLLATE NOCASE ASC, owner ASC, doc_id ASC",
            Self::Title => "sort_title COLLATE NOCASE ASC, owner ASC, doc_id ASC",
            Self::YearAsc => "sort_year IS NULL ASC, sort_year ASC, owner ASC, doc_id ASC",
            Self::YearDesc => "sort_year IS NULL ASC, sort_year DESC, owner ASC, doc_id ASC",
            Self::Location => "sort_location COLLATE NOCASE ASC, owner ASC, doc_id ASC",
        }
    }
}

pub struct ExportOptions {
    pub path: PathBuf,
    pub format: ReportFormat,
    pub sort: ReportSort,
    pub expected_generation: Option<String>,
    pub protected_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportReceipt {
    pub path: PathBuf,
    pub documents: u64,
    pub scope: &'static str,
    pub generation: String,
    pub format: ReportFormat,
    pub sort: ReportSort,
}

/// Retains the real read lease through final generation validation and atomic
/// publication. The adapter must also re-check mounted sources and active index.
pub struct PreparedLexicalReport {
    staging: crate::index::report_output::StagedReport,
    receipt: ReportReceipt,
    local: Arc<LocalIndex>,
    fts: Arc<FtsIndex>,
    volumes: Option<Vec<String>>,
    cancellation: Option<Arc<ScanCancellation>>,
    _guard: crate::index::ingest::IndexWriteGuard,
}
impl PreparedLexicalReport {
    pub async fn publish(self) -> Result<ReportReceipt> {
        self.publish_with_validation(|| Ok(())).await
    }

    pub async fn publish_with_validation(self, validate_scope: impl FnOnce() -> Result<()> + Send + 'static) -> Result<ReportReceipt> {
        check_cancelled(&self.cancellation)?;
        ensure!(generation_under_guard(&self.local, &self.fts, self.volumes.as_deref()).await? == self.receipt.generation,
            "The index changed during export. The previous report was preserved; search again.");
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _guard = self._guard;
            let mut receipt = self.receipt;
            let published = self.staging.publish(|| {
                check_cancelled(&self.cancellation)?;
                ensure!(runtime.block_on(generation_under_guard(&self.local, &self.fts, self.volumes.as_deref()))? == receipt.generation,
                    "The index changed before publication. The previous report was preserved; search again.");
                validate_scope()?;
                if let Some(cancellation) = &self.cancellation { cancellation.begin_publish()?; }
                Ok(())
            });
            if let Some(cancellation) = &self.cancellation { cancellation.finish_publish(); }
            receipt.path = published?;
            Ok(receipt)
        }).await?
    }
}

pub async fn prepare_export(
    local: Arc<LocalIndex>, fts: Arc<FtsIndex>, query: String, filters: SearchFilters,
    mut options: ExportOptions, cancellation: Option<Arc<ScanCancellation>>,
) -> Result<PreparedLexicalReport> {
    let guard = acquire_read_guard(&local, &fts, &cancellation).await?;
    let generation = generation_under_guard(&local, &fts, filters.mounted_volume_ids.as_deref()).await?;
    ensure!(options.expected_generation.as_ref().map_or(true, |expected| expected == &generation),
        "The index or source scope changed after the match count. Count again before exporting that snapshot.");
    options.protected_roots.push(local.ingest_journal_path().parent().context("The index has no storage directory")?.to_owned());
    let (path, protected_roots) = (options.path.clone(), options.protected_roots.clone());
    let staging = tokio::task::spawn_blocking(move ||
        crate::index::report_output::StagedReport::new(&path, &protected_roots, &[])).await??;
    protect_index_sources(&local, staging.destination().to_owned(), &cancellation).await?;
    let (accumulator, effective_query) = scan_documents(local.clone(), fts.clone(), query.clone(), filters.clone(), cancellation.clone()).await?;
    let interrupt = accumulator.database.get_interrupt_handle();
    let export_filters = filters.clone(); let export_generation = generation.clone(); let export_cancellation = cancellation.clone();
    let (staging, documents) = await_sqlite(tokio::task::spawn_blocking(move || {
        accumulator.write_report(staging, &query, &effective_query, &export_filters, &export_generation,
            options.format, options.sort, &export_cancellation)
    }), interrupt, &cancellation).await?;
    check_cancelled(&cancellation)?;
    let receipt = ReportReceipt { path: staging.destination().to_owned(), documents,
        scope: "local_indexed_documents", generation, format: options.format, sort: options.sort };
    Ok(PreparedLexicalReport { staging, receipt, local, fts, volumes: filters.mounted_volume_ids,
        cancellation, _guard: guard })
}

/// Includes sources outside the query, so a report cannot replace an indexed
/// document just because that source did not match the exported query.
pub async fn protect_index_sources(local: &LocalIndex, destination: PathBuf,
    cancellation: &Option<Arc<ScanCancellation>>) -> Result<()> {
    let mut options = QueryExecutionOptions::default(); options.max_batch_length = BATCH_ROWS;
    let mut rows = local.table_ref().query().select(Select::Columns(vec!["location_uri".into()]))
        .execute_with_options(options).await?;
    while let Some(batch) = rows.try_next().await? {
        check_cancelled(cancellation)?;
        let path = destination.clone(); let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            let values = batch.column_by_name("location_uri").and_then(|column| column.as_any().downcast_ref::<StringArray>())
                .context("The indexed source-location column is invalid")?;
            let mut seen = BTreeSet::new();
            for value in values.iter().flatten() {
                check_cancelled(&cancellation)?;
                if seen.insert(value) { crate::index::report_output::reject_source_destination(&path, value)?; }
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
    }
    Ok(())
}

fn check_cancelled(cancellation: &Option<Arc<ScanCancellation>>) -> Result<()> {
    ensure!(!cancellation.as_ref().is_some_and(|flag| flag.phase.load(Ordering::Acquire) == 1),
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

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataRow {
    doc_id: String,
    owner_id: String,
    #[serde(rename = "representativeChunkIndex")]
    chunk_index: i32,
    title: Option<String>,
    filename: Option<String>,
    author: Option<String>,
    location_uri: String,
    url: Option<String>,
    source_hash: Option<String>,
    indexed_at: Option<i64>,
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
    let titles = strings("title")?; let filenames = strings("filename")?; let authors = strings("author")?;
    let locations = strings("location_uri")?; let urls = strings("url")?; let hashes = strings("source_hash")?;
    let indexed_at = batch.column_by_name("indexed_at").and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>())
        .context("The indexed timestamp column is invalid")?;
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
            title: value(titles, row), filename: value(filenames, row), author: value(authors, row),
            location_uri: value(locations, row).unwrap_or_default(), url: value(urls, row), source_hash: value(hashes, row),
            indexed_at: (!indexed_at.is_null(row)).then(|| indexed_at.value(row)),
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
            database.progress_handler(1000, Some(move || cancellation.phase.load(Ordering::Acquire) == 1))?;
        }
        database.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE;
            PRAGMA cache_size=-2048; PRAGMA mmap_size=0;
            CREATE TABLE documents (owner TEXT NOT NULL, doc_id TEXT NOT NULL, representative_key TEXT NOT NULL,
                metadata TEXT NOT NULL, sort_title TEXT NOT NULL, sort_filename TEXT NOT NULL,
                sort_year INTEGER, sort_location TEXT NOT NULL,
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
            let mut document = transaction.prepare_cached("INSERT INTO documents(owner,doc_id,representative_key,metadata,sort_title,sort_filename,sort_year,sort_location)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                ON CONFLICT(owner,doc_id) DO UPDATE SET representative_key=excluded.representative_key,metadata=excluded.metadata,
                    sort_title=excluded.sort_title,sort_filename=excluded.sort_filename,sort_year=excluded.sort_year,sort_location=excluded.sort_location
                WHERE excluded.representative_key < documents.representative_key")?;
            let mut facet = transaction.prepare_cached("INSERT OR IGNORE INTO facets(kind,value,owner,doc_id) VALUES (?1,?2,?3,?4)")?;
            let mut replace = transaction.prepare_cached("DELETE FROM facets WHERE owner=?1 AND doc_id=?2 AND kind!='tag'")?;
            for row in &rows {
                check_cancelled(cancellation)?;
                if !allowed.contains(&(row.owner_id.as_str(), row.doc_id.clone())) { continue; }
                // Scalars use the lowest matching stored chunk, with a stable
                // tie-break for inconsistent duplicate legacy rows. Tags are a
                // per-document union, so repetitions never increase a count.
                let metadata = serde_json::to_string(row)?;
                let representative = format!("{:010}:{metadata}", row.chunk_index);
                if document.execute(params![row.owner_id, row.doc_id, representative, metadata,
                    row.title.as_deref().or(row.filename.as_deref()).unwrap_or_default(), row.filename.as_deref().unwrap_or_default(),
                    row.year, row.location_uri])? > 0 {
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

    fn write_report(self, mut staging: crate::index::report_output::StagedReport,
        query: &str, effective_query: &str, filters: &SearchFilters, generation: &str,
        format: ReportFormat, sort: ReportSort, cancellation: &Option<Arc<ScanCancellation>>,
    ) -> Result<(crate::index::report_output::StagedReport, u64)> {
        let total = self.database.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get::<_, i64>(0))? as u64;
        let header = serde_json::json!({ "schema": "CrispSorter.lexical-report", "version": 1,
            "scope": "local_indexed_documents", "complete": true, "matchingDocuments": total,
            "query": query, "effectiveQuery": effective_query, "filters": filters, "generation": generation,
            "sort": sort, "ordering": "global metadata order; owner and document ID break ties; unknown years follow known years",
            "representative": "lowest matching stored chunk; tags are the per-document union of matching stored rows",
            "excludedChannels": ["catalog_filename_references", "semantic_similarity"],
            "exportedAtUnixMs": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis(),
        });
        {
            let mut output = BufWriter::new(staging.file_mut());
            if matches!(format, ReportFormat::Json) {
                let mut prefix = serde_json::to_vec(&header)?; prefix.pop();
                output.write_all(&prefix)?; output.write_all(b",\"documents\":[")?;
            } else {
                output.write_all(b"record_type,owner_id,doc_id,title,filename,author,year,extension,language,location_uri,source_url,source_hash,indexed_at,representative_chunk_index,tags_json,query,effective_query,scope,generation,total_documents,sort,filters_json\r\n")?;
                let mut metadata = vec!["report".to_owned()]; metadata.extend(vec![String::new(); 14]);
                metadata.extend([query.to_owned(), effective_query.to_owned(), "local_indexed_documents".into(), generation.to_owned(),
                    total.to_string(), serde_json::to_string(&sort)?.trim_matches('"').to_owned(), serde_json::to_string(filters)?]);
                output.write_all(metadata.iter().map(|value| csv_cell(value)).collect::<Vec<_>>().join(",").as_bytes())?;
                output.write_all(b"\r\n")?;
            }
            let mut rows = self.database.prepare(&format!("SELECT metadata FROM documents ORDER BY {}", sort.sql()))?;
            let mut tags = self.database.prepare("SELECT value FROM facets WHERE kind='tag' AND owner=?1 AND doc_id=?2 ORDER BY value")?;
            let mut rows = rows.query([])?;
            let mut written = 0u64;
            let filters_json = serde_json::to_string(filters)?;
            let sort_json = serde_json::to_string(&sort)?;
            while let Some(row) = rows.next()? {
                check_cancelled(cancellation)?;
                let mut document: MetadataRow = serde_json::from_str(&row.get::<_, String>(0)?)?;
                document.tags = tags.query_map(params![document.owner_id, document.doc_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if matches!(format, ReportFormat::Json) {
                    if written > 0 { output.write_all(b",")?; }
                    serde_json::to_writer(&mut output, &document)?;
                } else {
                    let values = vec!["document".into(), document.owner_id, document.doc_id, document.title.unwrap_or_default(),
                        document.filename.unwrap_or_default(), document.author.unwrap_or_default(),
                        document.year.map(|value| value.to_string()).unwrap_or_default(), document.extension.unwrap_or_default(),
                        document.language.unwrap_or_default(), document.location_uri, document.url.unwrap_or_default(),
                        document.source_hash.unwrap_or_default(), document.indexed_at.map(|value| value.to_string()).unwrap_or_default(),
                        document.chunk_index.to_string(), serde_json::to_string(&document.tags)?, query.to_owned(), effective_query.to_owned(),
                        "local_indexed_documents".into(), generation.to_owned(), total.to_string(), sort_json.trim_matches('"').to_owned(), filters_json.clone()];
                    let line = values.iter().map(|value| csv_cell(value)).collect::<Vec<_>>().join(",");
                    output.write_all(line.as_bytes())?; output.write_all(b"\r\n")?;
                }
                written += 1;
            }
            ensure!(written == total, "The complete report row count changed while exporting");
            if matches!(format, ReportFormat::Json) { output.write_all(b"]}\n")?; }
            output.flush().context("Flushing the complete matching-document report")?;
        }
        check_cancelled(cancellation)?;
        Ok((staging, total))
    }
}

fn csv_cell(value: &str) -> String {
    // Keep file names and metadata from being evaluated as spreadsheet formulas.
    let prefix = if value.trim_start().starts_with(['=', '+', '-', '@']) || value.starts_with(['\t', '\r', '\n']) { "'" } else { "" };
    format!("\"{prefix}{}\"", value.replace('"', "\"\""))
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

    #[test]
    fn spreadsheet_formula_guards_include_ascii_and_unicode_leading_whitespace() {
        for value in ["=SUM(1,2)", "  =SUM(1,2)", "\u{2003}+unsafe", "\u{a0}@unsafe", " -unsafe", "\tplain", "\rplain", "\nplain"] {
            assert!(csv_cell(value).starts_with("\"'"), "{value:?}");
        }
        assert_eq!(csv_cell("normal, \"quoted\""), "\"normal, \"\"quoted\"\"\"");
    }

    #[test]
    fn cancellation_is_rejected_once_atomic_publication_has_started() {
        let directory = tempfile::tempdir().unwrap(); let path = directory.path().join("report.json");
        std::fs::write(&path, b"previous").unwrap();
        let token = ScanCancellation::default();
        let mut staged = crate::index::report_output::StagedReport::new(&path, &[], &[]).unwrap();
        staged.file_mut().write_all(b"complete new report").unwrap();
        staged.publish(|| {
            token.begin_publish()?;
            assert!(!token.cancel(), "Cancellation cannot win after publication begins");
            assert_eq!(std::fs::read(&path).unwrap(), b"previous");
            Ok(())
        }).unwrap();
        token.finish_publish();
        assert!(!token.cancel(), "A completed report cannot be reported as cancelled");
        assert_eq!(std::fs::read(&path).unwrap(), b"complete new report");
        let cancelled = ScanCancellation::default(); assert!(cancelled.cancel()); assert!(cancelled.begin_publish().is_err());
    }
}
