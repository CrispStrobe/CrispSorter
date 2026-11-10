/// Ingest pipeline: text extraction output → chunks → embeddings → indexes.
///
/// `IngestPipeline` owns a shared `FtsIndex`, `LocalIndex`, and `Embedder`.
/// Embedding (CPU/GPU-bound) runs on the caller's task; the resulting chunks
/// are serialised through a single background writer task that owns all
/// LanceDB + Tantivy mutations.  This gives three properties:
///
///   1. No concurrent writes to LanceDB / Tantivy — one writer at a time,
///      regardless of how many concurrent callers embed in parallel.
///   2. Queue depth (`pending`) is measurable — exposed via `queue_depth()`
///      and the `index_queue_depth` Tauri command (PLAN P11 step 3).
///   3. Callers still `await` the result via a oneshot channel, so no
///      breaking changes to existing Tauri command signatures.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use super::embedder::Embedder;
use super::fts_index::{FtsIndex, TantivyInput};
use super::local_index::LocalIndex;
use super::ner::NerHandle;
use super::schema::DocumentChunk;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Approximate word count per chunk. Use 1500 for bge-m3, 350 for 512-token models.
    pub chunk_max_words: usize,
    /// Word overlap between consecutive chunks. Use 200 for bge-m3, 50 for others.
    pub chunk_stride: usize,
    /// How many chunks to embed + write per LanceDB batch.
    pub batch_size: usize,
    /// Explicitly store searchable text without embeddings.
    pub text_only: bool,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            chunk_max_words: 1500,
            chunk_stride: 200,
            batch_size: 32,
            text_only: false,
        }
    }
}

// ── RawDocument ─────────────────────────────────────────────────────────────

/// All information about a document as it arrives from CrispSorter's extractors.
///
/// Both `full_text` (plain) and `full_text_md` (Markdown) are optional:
/// `full_text` is used for chunking and embedding; `full_text_md` is stored
/// verbatim for display.
#[derive(Debug, Clone)]
pub struct RawDocument {
    // Text content
    pub full_text: String,
    pub full_text_md: String,
    pub headings: Vec<String>,

    // Document metadata
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub filename: String,
    pub ext: String,
    pub language: String,

    // Provenance
    pub source_hash: String,
    pub location_uri: String,
    pub owner_id: String,
    pub tags: Vec<String>,

    /// Unix epoch seconds of the source file's last-modified time.
    /// `None` for non-file ingests (pasted text, web URLs once we add
    /// them, etc.). Stored in `metadata_json` as `{"mtime_unix": v}`
    /// so we can mtime-skip on re-ingest without a schema migration
    /// (PLAN P7.4.3). Default `None` keeps the existing
    /// `index_ingest_document` callers compiling — skip-check just
    /// returns "no record" for those.
    pub mtime_unix: Option<i64>,

    /// Byte size of the source file (PLAN P9 open UX follow-up).
    /// Written into `metadata_json` as `fs_size` so the Übersicht
    /// size column renders for L3 rows (L1 already writes it via
    /// `L1FileEntry.size`). `None` for non-file ingests.
    pub file_size: Option<i64>,

    /// Stable id of the volume the source file lives on (PLAN P7.6).
    /// Populated by the `volume::volume_id_for_path` helper at ingest
    /// time. Stored alongside `mtime_unix` in `metadata_json` so a
    /// future search-time filter can hide rows from currently-unmounted
    /// volumes without a schema migration. `None` for non-file ingests
    /// or when the platform helper fails (best-effort enrichment).
    pub volume_id: Option<String>,

    /// Whether this row was written while image indexing was enabled.
    ///
    /// Stored in `metadata_json` as `"images_indexed":true` (absent when
    /// false, mirroring `mtime_unix`'s convention) so the re-ingest guard can
    /// tell "no image vectors because there are none to have" from "no image
    /// vectors because the feature was off at the time". Turning image
    /// indexing on therefore backfills existing documents exactly once
    /// instead of leaving them permanently without vectors.
    pub images_indexed: bool,

    /// Parent directory of the source file (PLAN P9 step 3).
    /// Written directly to the `parent_dir` column so folder-prefix
    /// filters in `query_documents` use a scalar index instead of a
    /// JSON LIKE scan.  `None` for non-file ingests (pasted text, etc.).
    pub parent_dir: Option<String>,

    /// P13.5 Phase 8 batch — translation of `full_text` produced by
    /// the extractor's post-dispatch MT pass (when
    /// `ExtractOptions::translate_to` was supplied).  Written into
    /// the `text_translated` LanceDB column (added by the
    /// `AddTextTranslatedColumns` migration).  Replicated across
    /// every chunk row, matching the existing `full_text_md`
    /// convention — slightly wasteful for big translations but keeps
    /// downstream queries from needing a JOIN.  `None` when no
    /// translation was attempted, the source language was unknown,
    /// or MT failed.
    pub translated_text: Option<String>,
    /// ISO 639-1 target language of [`Self::translated_text`].
    /// Written into the `text_translated_lang` column.  Lets a
    /// future search-side filter say "give me docs where the
    /// translated column is English" without parsing every row.
    pub translated_to_lang: Option<String>,

    /// P13.6 Step 7 — audio L2 metadata.  Populated by bg_ingest
    /// from `ExtractedDocument.audio` (symphonia probe, no decode
    /// pass).  Replicated across every chunk row of the same doc;
    /// `None` for non-audio extractors.  Lands in the `audio_*`
    /// LanceDB columns added by migration v101.
    pub audio_duration_seconds: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate_hz: Option<i32>,
    pub audio_channels: Option<i32>,
    pub audio_bitrate_kbps: Option<i32>,

    /// P13.6 Step 9 — image L2 (EXIF).  Populated by the OCR
    /// extractor for images.  None for non-image rows.  Lands in
    /// the `image_*` LanceDB columns added by migration v102.
    pub image_camera_make: Option<String>,
    pub image_camera_model: Option<String>,
    pub image_lens_model: Option<String>,
    pub image_taken_at_unix: Option<i64>,
    pub image_iso: Option<i32>,

    /// Stage Z — pre-packed ColBERT multivec (raw little-endian f32 bytes,
    /// `n_tokens × dim × 4` bytes total).  `None` unless the embedder
    /// ran the ColBERT model.  Forwarded into `DocumentChunk.multivec_packed`
    /// so the LanceDB writer can persist it without re-packing.
    pub multivec_packed: Option<Vec<u8>>,
    /// Stage Z — number of token vectors in `multivec_packed`.
    pub multivec_n_tokens: Option<i16>,

    /// v106 — Original source URL the document came from.  Populated
    /// by the markdown extractor from YAML frontmatter (`url:`), by
    /// the PDF extractor from XMP `/URL`, by the EPUB extractor from
    /// `<dc:source>`.  Forwarded into `DocumentChunk.url` so the
    /// LanceDB writer persists it as a first-class column.  `None`
    /// for files with no provenance URL.
    pub url: Option<String>,

    /// P17.5 — 2048-D cross-modal embedding from BidirLM-Omni.
    /// Populated by bg_ingest for image/audio files when the
    /// `crispembed` feature is compiled in.  `None` otherwise.
    pub embedding_omni: Option<Vec<f32>>,

    /// P17.7 — 768-D ViT image embedding (SigLIP/CLIP).
    /// Populated by bg_ingest for image files when the
    /// `crispembed` feature is compiled in.  `None` otherwise.
    pub embedding_vit: Option<Vec<f32>>,
}

// ── IngestStats ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestStats {
    pub chunk_count: usize,
    pub embed_time_ms: u64,
    pub write_time_ms: u64,
    /// Actual committed occurrence identities, in the caller's input order.
    /// Repeated source inputs resolve to the same final committed identity.
    #[serde(default)]
    pub doc_ids: Vec<String>,
}

// ── Writer task types ────────────────────────────────────────────────────────

/// Owned Tantivy inputs that can be sent across the channel boundary.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TantivyInputOwned {
    pub(crate) doc_id: String,
    pub(crate) owner_id: String,
    pub(crate) language: String,
    pub(crate) title: String,
    pub(crate) headings: String,
    pub(crate) body: String,
    /// MT-pass output replicated for the FTS write — `None` when the
    /// extractor didn't translate (no `--translate-to`, source lang
    /// unknown, or MT failed).  Wired into the `body_translated`
    /// Tantivy field when the on-disk schema has it.
    pub(crate) body_translated: Option<String>,
}

/// The intent is durable before Lance commits. A generation marker on the
/// committed rows distinguishes an applied transaction from an interrupted
/// preparation, without guessing from timestamps or file contents.
#[derive(serde::Serialize, serde::Deserialize)]
struct PendingFtsWrite {
    generation: String,
    doc_ids: Vec<String>,
    replace_ids: Vec<String>,
    inputs: Vec<TantivyInputOwned>,
    #[serde(default)]
    receipt_rows: Vec<String>,
    #[serde(default)]
    delete_only: bool,
    #[serde(default)]
    absent_occurrences: Vec<super::local_index::SourceOccurrence>,
}

fn open_write_journal(path: &std::path::Path) -> Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(10))?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS pending_fts_writes (
            generation TEXT PRIMARY KEY NOT NULL,
            payload TEXT NOT NULL
        );")?;
    Ok(connection)
}

fn record_fts_intent(path: &std::path::Path, pending: &PendingFtsWrite) -> Result<()> {
    open_write_journal(path)?.execute(
        "INSERT INTO pending_fts_writes (generation, payload) VALUES (?1, ?2)",
        rusqlite::params![pending.generation, serde_json::to_string(pending)?],
    )?;
    Ok(())
}

fn pending_fts_writes(path: &std::path::Path) -> Result<Vec<PendingFtsWrite>> {
    let connection = open_write_journal(path)?;
    let mut statement = connection.prepare("SELECT payload FROM pending_fts_writes ORDER BY rowid")?;
    let payloads = statement.query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    payloads.into_iter().map(|s| serde_json::from_str(&s).map_err(Into::into)).collect()
}

fn finish_fts_intent(path: &std::path::Path, generation: &str) -> Result<()> {
    open_write_journal(path)?.execute("DELETE FROM pending_fts_writes WHERE generation = ?1", [generation])?;
    Ok(())
}

/// Re-initialising settings may briefly leave two pipeline tasks alive. Share
/// their write lock by journal path so a new writer cannot discard an intent
/// while the previous writer is still preparing its vector transaction.
fn write_lock_for(path: &std::path::Path) -> Arc<Mutex<()>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<
        std::path::PathBuf, std::sync::Weak<Mutex<()>>>>> = std::sync::OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let key = path.parent().and_then(|p| p.canonicalize().ok())
        .map(|p| p.join("index-write-journal.sqlite")).unwrap_or_else(|| path.to_owned());
    if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) { return lock; }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// SQLite's process lock is released by the OS after a crash. A separate
/// connection keeps the lease across both stores without blocking journal
/// transactions, including when the CLI and desktop run at the same time.
async fn acquire_process_write_guard(journal_path: &std::path::Path) -> Result<rusqlite::Connection> {
    let path = journal_path.with_file_name("index-write-lock.sqlite");
    tokio::task::spawn_blocking(move || -> Result<rusqlite::Connection> {
        let connection = rusqlite::Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        Ok(connection)
    }).await.context("opening index writer lease")?
}

/// Hold across a complete index read or mutation. Dropping releases both the
/// in-process lock and cross-process lease, including on cancellation.
pub struct IndexWriteGuard {
    _local: Option<tokio::sync::OwnedMutexGuard<()>>,
    _lease: Option<rusqlite::Connection>,
}

impl IndexWriteGuard {
    pub(crate) fn immutable_snapshot() -> Self { Self { _local: None, _lease: None } }
}

pub(crate) async fn acquire_index_guard(path: &std::path::Path) -> Result<IndexWriteGuard> {
    let local = write_lock_for(path).lock_owned().await;
    let lease = acquire_process_write_guard(path).await?;
    Ok(IndexWriteGuard { _local: Some(local), _lease: Some(lease) })
}

fn apply_fts_write(fts: &FtsIndex, pending: &PendingFtsWrite) -> Result<()> {
    let mut writer = fts.writer().context("opening full-text writer for repair")?;
    for doc_id in &pending.replace_ids {
        fts.delete_document(&mut writer, doc_id)?;
    }
    for input in &pending.inputs {
        fts.add_document(&mut writer, TantivyInput {
            doc_id: &input.doc_id, owner_id: &input.owner_id,
            language: &input.language, title: &input.title,
            headings: &input.headings, body: &input.body,
            body_translated: input.body_translated.as_deref(),
        })?;
    }
    writer.commit().context("committing full-text repair")?;
    super::result_cache::invalidate();
    Ok(())
}

pub(crate) async fn repair_fts_writes(fts: &FtsIndex, vector: &LocalIndex) -> Result<()> {
    for pending in pending_fts_writes(vector.ingest_journal_path())? {
        let committed = if pending.delete_only && !pending.absent_occurrences.is_empty() {
            vector.occurrences_absent(&pending.absent_occurrences).await?
        } else if pending.delete_only {
            vector.documents_absent(&pending.replace_ids).await?
        } else if !pending.receipt_rows.is_empty() {
            vector.row_generation_committed(&pending.receipt_rows, &pending.generation).await?
        } else {
            vector.generation_committed(&pending.doc_ids, &pending.generation).await?
        };
        if committed {
            anyhow::ensure!(vector.occurrences_absent(&pending.absent_occurrences).await?,
                "Incomplete source occurrence replacement; full-text repair retained");
            apply_fts_write(fts, &pending)?;
        }
        // A noncommitted intent never changes the last good full-text entry.
        finish_fts_intent(vector.ingest_journal_path(), &pending.generation)?;
    }
    Ok(())
}

pub(crate) async fn apply_imported_chunks(
    local: &LocalIndex, fts: &FtsIndex, chunks: &[DocumentChunk], replace_doc_ids: &[String],
) -> Result<()> {
    apply_imported_chunks_if_version(local, fts, chunks, replace_doc_ids, None).await.map(|_| ())
}

pub(crate) async fn apply_imported_chunks_if_version(
    local: &LocalIndex, fts: &FtsIndex, chunks: &[DocumentChunk], replace_doc_ids: &[String],
    expected_version: Option<u64>,
) -> Result<bool> {
    anyhow::ensure!(local.dims > 0, "An index snapshot is read-only");
    let _guard = local.read_guard(fts).await?;
    if let Some(expected) = expected_version {
        if local.table_ref().version().await? != expected { return Ok(false); }
    }
    let affected_ids = replace_doc_ids.iter().map(String::as_str)
        .chain(chunks.iter().map(|chunk| chunk.doc_id.as_str())).collect::<std::collections::BTreeSet<_>>();
    for doc_id in affected_ids {
        let occurrences = local.document_occurrences(doc_id).await?;
        anyhow::ensure!(occurrences.len() <= 1,
            "This legacy document ID refers to multiple owners or locations; review the selected occurrence before replacement");
        let incoming_scopes = chunks.iter().filter(|chunk| chunk.doc_id == doc_id)
            .map(|chunk| (&chunk.owner_id, &chunk.location_uri)).collect::<std::collections::BTreeSet<_>>();
        anyhow::ensure!(incoming_scopes.len() <= 1, "Imported document identity spans different owners or locations");
        if let (Some((stored_owner, _)), Some((incoming_owner, _))) = (occurrences.first(), incoming_scopes.first()) {
            anyhow::ensure!(stored_owner == *incoming_owner, "Imported document identity belongs to another owner");
        }
    }
    let mut prepared = local.prepare_imported_chunks(chunks, replace_doc_ids).await?;
    let mut replace_ids: std::collections::BTreeSet<String> = replace_doc_ids.iter().cloned().collect();
    replace_ids.extend(prepared.iter().map(|c| c.doc_id.clone()));
    if replace_ids.is_empty() { return Ok(true); }
    let inputs = local.fts_inputs_after_import(&prepared, replace_doc_ids).await?;
    let pending = PendingFtsWrite {
        generation: uuid::Uuid::new_v4().to_string(),
        doc_ids: prepared.iter().map(|c| c.doc_id.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect(),
        replace_ids: replace_ids.into_iter().collect(),
        receipt_rows: prepared.iter().map(|c| c.id.clone()).collect(),
        delete_only: prepared.is_empty(), inputs, absent_occurrences: Vec::new(),
    };
    record_fts_intent(local.ingest_journal_path(), &pending)?;
    local.replace_imported_rows(&mut prepared, replace_doc_ids, &pending.generation).await?;
    apply_fts_write(fts, &pending).context("Vector rows saved; full-text repair is pending")?;
    finish_fts_intent(local.ingest_journal_path(), &pending.generation)?;
    Ok(true)
}

/// Metadata fields and their lexical representation advance together. The
/// receipt identifies every affected row, including metadata-only sources,
/// so a failed full-text commit can be replayed after reopening the index.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_metadata_update(
    local: &LocalIndex, fts: &FtsIndex, doc_id: &str, title: Option<&str>,
    author: Option<&str>, year: Option<i32>, language: Option<&str>,
    page_count: Option<i32>, metadata_json: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(local.dims > 0, "An index snapshot is read-only");
    let _guard = local.read_guard(fts).await?;
    let generation = uuid::Uuid::new_v4().to_string();
    let (receipt_rows, prepared_rows) = local.prepare_metadata_update(
        doc_id, title, author, year, language, page_count, metadata_json, &generation).await?;
    if receipt_rows.is_empty() { return Ok(()); }
    let mut inputs = local.fts_inputs_for_docs(&[doc_id.to_owned()]).await?;
    for input in &mut inputs {
        if let Some(title) = title { input.title = title.to_owned(); }
        if let Some(language) = language { input.language = language.to_owned(); }
    }
    let pending = PendingFtsWrite {
        generation, doc_ids: vec![doc_id.to_owned()], replace_ids: vec![doc_id.to_owned()],
        inputs, receipt_rows, delete_only: false, absent_occurrences: Vec::new(),
    };
    record_fts_intent(local.ingest_journal_path(), &pending)?;
    local.apply_metadata_columns(doc_id, prepared_rows).await?;
    apply_fts_write(fts, &pending).context("Metadata saved; full-text repair is pending")?;
    finish_fts_intent(local.ingest_journal_path(), &pending.generation)?;
    Ok(())
}

/// Remote document fields may advance without replacing reviewed body rows.
pub(crate) struct RemoteMetadata<'a> {
    pub doc_id: &'a str,
    pub source_hash: &'a str,
    pub owner_id: &'a str,
    pub location_uri: &'a str,
    pub title: Option<&'a str>,
    pub author: Option<&'a str>,
    pub year: Option<i32>,
    pub language: Option<&'a str>,
    pub indexed_at: i64,
    pub origin: &'a str,
}

pub(crate) async fn apply_remote_metadata_if_version(
    local: &LocalIndex, fts: &FtsIndex, update: RemoteMetadata<'_>, expected_version: u64,
) -> Result<bool> {
    anyhow::ensure!(local.dims > 0, "An index snapshot is read-only");
    let _guard = local.read_guard(fts).await?;
    if local.table_ref().version().await? != expected_version { return Ok(false); }
    let Some(current) = local.fetch_document_summary(update.doc_id).await? else { return Ok(false); };
    anyhow::ensure!(current.owner_id == update.owner_id, "Remote metadata belongs to a different owner");
    anyhow::ensure!(local.document_occurrences(update.doc_id).await?.len() <= 1,
        "This legacy document ID has multiple locations; select the occurrence before updating metadata");
    anyhow::ensure!(update.indexed_at >= 0, "Remote metadata timestamp is invalid");
    anyhow::ensure!(current.chunk_index < 0 || current.source_hash == update.source_hash,
        "The remote document content changed; refresh its source before applying metadata to reviewed text");
    let generation = uuid::Uuid::new_v4().to_string();
    let patch = serde_json::json!({"sync_remote_url":update.origin.trim_end_matches('/'),
        "sync_remote_indexed_at":update.indexed_at,"sync_base_generation":generation}).to_string();
    let (receipt_rows, prepared) = local.prepare_metadata_update(update.doc_id,
        update.title, update.author, update.year, update.language, None, Some(&patch), &generation).await?;
    let mut rows = Vec::with_capacity(prepared.len());
    for batch in prepared {
        let schema = batch.schema();
        let mut columns = batch.columns().to_vec();
        if !update.location_uri.is_empty() {
            columns[schema.index_of("location_uri")?] = Arc::new(arrow_array::StringArray::from(vec![update.location_uri; batch.num_rows()]));
        }
        columns[schema.index_of("indexed_at")?] = Arc::new(arrow_array::TimestampMillisecondArray::from(vec![update.indexed_at; batch.num_rows()]));
        if current.chunk_index < 0 && !update.source_hash.is_empty() {
            columns[schema.index_of("source_hash")?] = Arc::new(arrow_array::StringArray::from(vec![update.source_hash; batch.num_rows()]));
        }
        rows.push(arrow_array::RecordBatch::try_new(schema, columns)?);
    }
    let mut inputs = local.fts_inputs_for_docs(&[update.doc_id.to_owned()]).await?;
    for input in &mut inputs {
        if let Some(title) = update.title { input.title = title.to_owned(); }
        if let Some(language) = update.language { input.language = language.to_owned(); }
    }
    let pending = PendingFtsWrite { generation, doc_ids: vec![update.doc_id.to_owned()],
        replace_ids: vec![update.doc_id.to_owned()], inputs, receipt_rows, delete_only: false, absent_occurrences: Vec::new() };
    record_fts_intent(local.ingest_journal_path(), &pending)?;
    local.apply_metadata_columns(update.doc_id, rows).await?;
    apply_fts_write(fts, &pending).context("Remote metadata saved; full-text repair is pending")?;
    finish_fts_intent(local.ingest_journal_path(), &pending.generation)?;
    Ok(true)
}

pub(crate) async fn apply_occurrence_deletion(
    local: &LocalIndex, fts: &FtsIndex, occurrence: super::local_index::SourceOccurrence,
) -> Result<()> {
    anyhow::ensure!(local.dims > 0, "An index snapshot is read-only");
    let _guard = local.read_guard(fts).await?;
    if local.occurrences_absent(std::slice::from_ref(&occurrence)).await? { return Ok(()); }
    let generation = uuid::Uuid::new_v4().to_string();
    let plan = local.prepare_occurrence_delete(occurrence, &generation).await?;
    let pending = PendingFtsWrite {
        generation, doc_ids: plan.preserved_inputs.iter().map(|i| i.doc_id.clone()).collect(),
        replace_ids: plan.replace_ids.clone(), inputs: plan.preserved_inputs.clone(),
        receipt_rows: plan.receipt_rows.clone(), delete_only: plan.receipt_rows.is_empty(),
        absent_occurrences: plan.absent_occurrences.clone(),
    };
    record_fts_intent(local.ingest_journal_path(), &pending)?;
    local.commit_occurrence_plan(&[], plan).await?;
    apply_fts_write(fts, &pending).context("Source removed; full-text repair is pending")?;
    finish_fts_intent(local.ingest_journal_path(), &pending.generation)?;
    Ok(())
}

/// One unit of work for the background writer task.
struct WriterJob {
    all_chunks: Vec<DocumentChunk>,
    /// Empty for L1 writes (no full-text in Tantivy for metadata-only rows).
    tantivy_inputs: Vec<TantivyInputOwned>,
    total_chunk_count: usize,
    embed_time_ms: u64,
    verified_sources: Vec<SourceReceipt>,
    expected_occurrences: Vec<(String, String)>,
    reply: tokio::sync::oneshot::Sender<Result<IngestStats>>,
}

#[derive(Clone)]
struct SourceReceipt {
    path: std::path::PathBuf,
    expected_sha256: String,
}

pub(crate) fn hash_local_source(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut source = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 { break; }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

async fn verify_source_receipts(receipts: Vec<SourceReceipt>) -> Result<()> {
    if receipts.is_empty() { return Ok(()); }
    tokio::task::spawn_blocking(move || -> Result<()> {
        for receipt in receipts {
            let actual = hash_local_source(&receipt.path)
                .with_context(|| format!("Verifying source before commit: {}", receipt.path.display()))?;
            anyhow::ensure!(actual == receipt.expected_sha256,
                "Source changed before index commit: {}. The previous index is retained; retry the file.", receipt.path.display());
        }
        Ok(())
    }).await.context("Source verification task failed")?
}

// ── Pipeline ─────────────────────────────────────────────────────────────────

pub struct IngestPipeline {
    pub fts: Arc<FtsIndex>,
    pub vector: Arc<LocalIndex>,
    /// `None` when the index was init'd without vector capabilities
    /// (`use_vector = false`) or when only L1 / L2 ingest was requested.
    /// L3 ingest checks this and errors clearly if not present.
    pub embedder: Option<Arc<Mutex<Embedder>>>,
    pub config: IngestConfig,
    /// P19 — optional GLiNER NER handle.  When set (via [`Self::with_ner`]),
    /// `ingest_documents_batch` runs NER once per document on its `full_text`
    /// and merges the resulting `"<label>:<text>"` entity tags into the
    /// document's `tags` before rows are built.  `None` = NER disabled
    /// (ingest behaviour unchanged).
    ner: Option<NerHandle>,
    /// Channel to the single background writer task.
    writer_tx: tokio::sync::mpsc::Sender<WriterJob>,
    /// Jobs submitted to the writer but not yet completed (queued + in-flight).
    /// Surfaced via `queue_depth()` → `index_queue_depth` Tauri command.
    pending: Arc<AtomicUsize>,
}

impl IngestPipeline {
    pub fn new(
        fts: Arc<FtsIndex>,
        vector: Arc<LocalIndex>,
        embedder: Option<Arc<Mutex<Embedder>>>,
        config: IngestConfig,
    ) -> Self {
        let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<WriterJob>(256);
        let pending = Arc::new(AtomicUsize::new(0));

        // Clone Arcs for the writer task. The task owns these for its
        // lifetime; the pipeline fields hold separate Arcs for query paths.
        let fts_w = fts.clone();
        let vector_w = vector.clone();
        let pending_w = pending.clone();
        let write_lock = write_lock_for(vector.ingest_journal_path());
        tokio::spawn(async move {
            // Recover a crash between the vector and full-text commits before
            // accepting another generation. Failure keeps the durable intent;
            // the next submitted job retries rather than reporting success.
            {
                let _guard = write_lock.lock().await;
                let recovery = async {
                    let _lease = acquire_process_write_guard(vector_w.ingest_journal_path()).await?;
                    repair_fts_writes(&fts_w, &vector_w).await
                }.await;
                if let Err(error) = recovery {
                    eprintln!("[index] pending full-text repair: {error:#}");
                }
            }
            while let Some(mut job) = writer_rx.recv().await {
                let result: Result<IngestStats> = (async {
                    let _guard = write_lock.lock().await;
                    let _lease = acquire_process_write_guard(vector_w.ingest_journal_path()).await?;
                    let write_start = Instant::now();
                    repair_fts_writes(&fts_w, &vector_w).await
                        .context("repairing a previous full-text write")?;
                    // A slow embedding job can outlive a newer extraction of
                    // the same source. Verify after taking the writer lease,
                    // before creating an intent or replacing any good rows.
                    verify_source_receipts(job.verified_sources).await?;
                    let mut doc_ids = Vec::new();
                    if job.tantivy_inputs.is_empty() {
                        vector_w.upsert_metadata_rows(&job.all_chunks).await?;
                    } else {
                        let generation = uuid::Uuid::new_v4().to_string();
                        let provisional_ids = job.tantivy_inputs.iter().map(|input| input.doc_id.clone()).collect::<Vec<_>>();
                        let plan = vector_w.prepare_occurrence_write(&mut job.all_chunks, &mut job.tantivy_inputs, &generation).await?;
                        for (provisional, expected) in &job.expected_occurrences {
                            let resolved = provisional_ids.iter().position(|id| id == provisional).and_then(|index| plan.doc_ids.get(index));
                            anyhow::ensure!(resolved == Some(expected),
                                "The selected source identity changed while extraction was running; reload the source before promoting it");
                        }
                        doc_ids = plan.doc_ids.clone();
                        job.tantivy_inputs.extend(plan.preserved_inputs.clone());
                        let pending = PendingFtsWrite {
                            generation,
                            doc_ids: job.tantivy_inputs.iter().map(|i| i.doc_id.clone()).collect(),
                            replace_ids: plan.replace_ids.clone(),
                            inputs: job.tantivy_inputs,
                            receipt_rows: plan.receipt_rows.clone(), delete_only: false,
                            absent_occurrences: plan.absent_occurrences.clone(),
                        };
                        record_fts_intent(vector_w.ingest_journal_path(), &pending)
                            .context("persisting index write intent")?;
                        vector_w.commit_occurrence_plan(&job.all_chunks, plan).await?;
                        // Failure from this point leaves a durable repair. A
                        // retry or reopening the pipeline completes the exact
                        // full-text update; completed vector rows are not lost.
                        apply_fts_write(&fts_w, &pending)
                            .context("Vector rows saved; full-text repair is pending")?;
                        finish_fts_intent(vector_w.ingest_journal_path(), &pending.generation)?;
                    }

                    let write_time_ms = write_start.elapsed().as_millis() as u64;
                    Ok(IngestStats {
                        chunk_count: job.total_chunk_count,
                        embed_time_ms: job.embed_time_ms,
                        write_time_ms,
                        doc_ids,
                    })
                })
                .await;

                // Decrement AFTER the write completes (or errors) so
                // `pending` counts both queued and in-flight jobs.
                pending_w.fetch_sub(1, Ordering::Relaxed);
                let _ = job.reply.send(result);
            }
        });

        IngestPipeline {
            fts,
            vector,
            embedder,
            config,
            ner: None,
            writer_tx,
            pending,
        }
    }

    /// Attach a GLiNER NER handle (P19). Builder so the existing
    /// `IngestPipeline::new` call sites stay unchanged; only the paths that
    /// opt into NER call this. `None` is a no-op.
    pub fn with_ner(mut self, ner: Option<NerHandle>) -> Self {
        self.ner = ner;
        self
    }

    /// Number of write jobs currently queued or in flight.
    /// Zero means the writer task is idle.
    pub fn queue_depth(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// Await recovery before publishing the index as ready. Queries use the
    /// same read guard, so a later failed full-text commit is surfaced too.
    pub async fn ensure_ready(&self) -> Result<()> {
        let _guard = self.vector.read_guard(&self.fts).await?;
        Ok(())
    }

    /// Enqueue a write job and block until the writer task completes it.
    async fn submit_and_await(
        &self,
        all_chunks: Vec<DocumentChunk>,
        tantivy_inputs: Vec<TantivyInputOwned>,
        total_chunk_count: usize,
        embed_time_ms: u64,
        verified_sources: Vec<SourceReceipt>,
        expected_occurrences: Vec<(String, String)>,
    ) -> Result<IngestStats> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        // Reserving can be cancelled while the queue is full. Count the job
        // only after capacity exists, then enqueue without another await.
        let permit = self.writer_tx.reserve().await.map_err(|_| anyhow::anyhow!(
            "Writer task has stopped — index may need re-init"))?;
        self.pending.fetch_add(1, Ordering::Relaxed);
        permit.send(WriterJob { all_chunks, tantivy_inputs, total_chunk_count,
            embed_time_ms, verified_sources, expected_occurrences, reply: reply_tx });
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Writer task dropped reply channel"))?
    }

    /// Full ingest pipeline for one document.
    ///
    /// Embeds the document inline, then submits the resulting chunks to the
    /// background writer task and awaits completion.
    pub async fn ingest_document(&self, raw: RawDocument) -> Result<IngestStats> {
        self.ingest_documents_batch(vec![raw]).await
    }

    /// File extraction opt-in: `source_hash` must be the SHA-256 of the source
    /// bytes. Remote/import identities never trigger a local filesystem read.
    pub async fn ingest_verified_document(&self, raw: RawDocument, source_path: std::path::PathBuf) -> Result<IngestStats> {
        let receipt = SourceReceipt { path: source_path, expected_sha256: raw.source_hash.clone() };
        self.ingest_documents_with_receipts(vec![raw], vec![receipt]).await
    }

    /// A selected-source promotion must still resolve to that exact stored
    /// occurrence when it reaches the writer, after all extraction work.
    pub async fn ingest_verified_document_for_occurrence(&self, raw: RawDocument, source_path: std::path::PathBuf, expected_doc_id: String) -> Result<IngestStats> {
        let receipt = SourceReceipt { path: source_path, expected_sha256: raw.source_hash.clone() };
        let expected = (doc_id_for(&raw), expected_doc_id);
        self.ingest_documents_with_context(vec![raw], vec![receipt], vec![expected]).await
    }

    /// Explicit file-byte receipts for callers whose document identities are
    /// derived from extracted text. A receipt is never inferred from a URI.
    pub async fn ingest_documents_batch_verified(&self, raws: Vec<RawDocument>, sources: Vec<(std::path::PathBuf, String)>) -> Result<IngestStats> {
        let receipts = sources.into_iter().map(|(path, expected_sha256)| SourceReceipt { path, expected_sha256 }).collect();
        self.ingest_documents_with_receipts(raws, receipts).await
    }

    /// Bulk ingest of N documents with coalesced LanceDB writes + one Tantivy commit.
    ///
    /// Embedding runs inline on the caller's task (GPU/CPU-bound).
    /// The resulting chunks are submitted to the background writer task, which
    /// serialises all LanceDB + Tantivy mutations so concurrent callers never
    /// race on the indexes. Callers await the result via a oneshot channel.
    pub async fn ingest_documents_batch(&self, raws: Vec<RawDocument>) -> Result<IngestStats> {
        self.ingest_documents_with_receipts(raws, Vec::new()).await
    }

    async fn ingest_documents_with_receipts(&self, raws: Vec<RawDocument>, verified_sources: Vec<SourceReceipt>) -> Result<IngestStats> {
        self.ingest_documents_with_context(raws, verified_sources, Vec::new()).await
    }

    async fn ingest_documents_with_context(&self, mut raws: Vec<RawDocument>, verified_sources: Vec<SourceReceipt>, expected_occurrences: Vec<(String, String)>) -> Result<IngestStats> {
        if raws.is_empty() {
            return Ok(IngestStats { chunk_count: 0, embed_time_ms: 0, write_time_ms: 0, doc_ids: Vec::new() });
        }
        anyhow::ensure!(self.config.batch_size > 0, "Embedding batch size must be greater than zero");
        let input_order = raws.iter().map(doc_id_for).collect::<Vec<_>>();
        let mut latest = std::collections::HashMap::new();
        for (index, id) in input_order.iter().enumerate() { latest.insert(id.clone(), index); }
        raws = raws.into_iter().enumerate().filter_map(|(index, raw)|
            (latest.get(&doc_id_for(&raw)) == Some(&index)).then_some(raw)).collect();
        let unique_order = raws.iter().map(doc_id_for).collect::<Vec<_>>();
        for raw in &raws {
            anyhow::ensure!(!raw.source_hash.trim().is_empty(), "Document identity is required");
            anyhow::ensure!(!raw.full_text.trim().is_empty()
                || raw.embedding_vit.is_some() || raw.embedding_omni.is_some(),
                "No searchable text was extracted from {}; use metadata indexing or retry extraction with OCR", raw.filename);
        }
        let active_embedder = if self.config.text_only { None } else { self.embedder.as_ref() };

        // ── P19 NER phase ───────────────────────────────────────────────
        // Run GLiNER once per document on the (truncated) full_text and merge
        // the resulting entity tags into raw.tags BEFORE chunk rows are built,
        // so every chunk of a doc carries the same entity tags (chunk_index
        // convention).  No-op when NER is disabled or the feature is off.
        let mut field_evidence = std::collections::HashMap::new();
        if let Some(ner) = &self.ner {
            for raw in raws.iter_mut() {
                let mut extracted = ner.extract_fields(&raw.full_text).await;
                if ner.entity_limit() > 0 { extracted.truncate(ner.entity_limit()); }
                let entities = extracted.iter().map(|(label, value, score)| super::ner::ScoredEntity {
                    label: label.clone(), text: value.clone(), score: *score,
                }).collect();
                let entity_tags = super::ner::build_entity_tags(entities, 0.0, ner.entity_limit());
                merge_tags(&mut raw.tags, entity_tags);
                let doc_id = doc_id_for(raw);
                // Stored identity and current reviews are resolved under the
                // writer gate after embedding, including moved legacy rows.
                let fields = super::fields::from_extraction(&doc_id, &raw.full_text, ner.model().display_name(), extracted);
                field_evidence.insert(doc_id, fields);
            }
        }

        // P25.5 — Barcode/QR detection in extracted text.
        for raw in raws.iter_mut() {
            let barcode_tags = super::barcode::detect_barcode_tags(&raw.full_text, 10);
            if !barcode_tags.is_empty() {
                merge_tags(&mut raw.tags, barcode_tags);
            }
        }

        if active_embedder.is_none() && !self.config.text_only
            && raws.iter().any(|r| !r.full_text.trim().is_empty()) {
            anyhow::bail!("Embedding is enabled but the model is not loaded; initialise it or select text-only indexing");
        }

        let mut all_chunks: Vec<DocumentChunk> = Vec::new();
        let mut tantivy_inputs: Vec<TantivyInputOwned> = Vec::with_capacity(raws.len());
        let mut total_chunk_count: usize = 0;

        // ── Embedding phase ─────────────────────────────────────────────
        let embed_start = Instant::now();
        for raw in &raws {
            // Text-only indexes retain the complete body as one representative
            // row and never load or call an embedding model.
            let has_text = !raw.full_text.trim().is_empty();
            let chunks = if has_text && active_embedder.is_some() {
                let embedder = active_embedder.expect("checked above");
                embedder.lock().await.chunk_for_embedding(&raw.full_text)?
            } else {
                vec![super::embedder::TextChunk {
                    text: raw.full_text.clone(), start_char: 0,
                    end_char: raw.full_text.len(), chunk_index: 0,
                }]
            };
            anyhow::ensure!(!chunks.is_empty(), "No searchable chunks were produced for {}", raw.filename);
            let chunk_total = chunks.len() as i32;
            total_chunk_count += chunks.len();

            if let Some(embedder) = active_embedder.filter(|_| has_text) {
            for batch in chunks.chunks(self.config.batch_size) {
                let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
                let (dense, sparse, multivecs, model_id) = {
                    use super::embedder::EmbedRole;
                    let mut emb = embedder.lock().await;
                    // Clone texts only when ColBERT is active; otherwise
                    // pass texts directly to embed_full and skip the copy.
                    let has_colbert = emb.has_colbert();
                    let (embed_texts, colbert_texts) = if has_colbert {
                        (texts.clone(), Some(texts))
                    } else {
                        (texts, None)
                    };
                    let (dense, sparse) = emb.embed_full(embed_texts, EmbedRole::Passage)?;
                    let multivecs = if let Some(ct) = colbert_texts {
                        emb.embed_multivec(ct)?
                    } else {
                        vec![vec![]; batch.len()]
                    };
                    let mid = format!("{:?}", emb.model());
                    (dense, sparse, multivecs, mid)
                };
                anyhow::ensure!(dense.vectors.len() == batch.len(),
                    "Embedding model returned {} vectors for {} passages", dense.vectors.len(), batch.len());
                anyhow::ensure!(dense.vectors.iter().all(|v| v.len() == self.vector.dims && v.iter().all(|n| n.is_finite())),
                    "Embedding model returned invalid dimensions or non-finite values");
                for (i, text_chunk) in batch.iter().enumerate() {
                    let embedding = dense.vectors.get(i).cloned();
                    let sparse_json = sparse
                        .get(i)
                        .and_then(|sv| sv.as_ref().map(|s| s.to_json().to_string()));
                    let multivec = multivecs.get(i).cloned().filter(|v| !v.is_empty());
                    all_chunks.push(build_doc_chunk(
                        text_chunk,
                        raw,
                        chunk_total,
                        embedding,
                        sparse_json,
                        model_id.clone(),
                        multivec,
                    ));
                }
            }

            } else {
                for chunk in &chunks {
                    all_chunks.push(build_doc_chunk(chunk, raw, chunk_total, None, None, String::new(), None));
                }
            }

            tantivy_inputs.push(TantivyInputOwned {
                doc_id: doc_id_for(raw),
                owner_id: raw.owner_id.clone(),
                language: raw.language.clone(),
                title: raw.title.clone().unwrap_or_default(),
                headings: raw.headings.join(" "),
                body: raw.full_text.clone(),
                body_translated: raw.translated_text.clone(),
            });
        }
        let embed_time_ms = if active_embedder.is_some() { embed_start.elapsed().as_millis() as u64 } else { 0 };

        for chunk in &mut all_chunks {
            if let Some(fields) = field_evidence.get(&chunk.doc_id) {
                super::fields::attach(&mut chunk.metadata_json, fields);
                merge_tags(&mut chunk.tags, fields.iter().filter(|f| f.review_status != "rejected").map(super::fields::tag).collect());
            }
        }
        // ── Write phase: submit to background writer task ───────────────
        let mut result = self.submit_and_await(all_chunks, tantivy_inputs, total_chunk_count, embed_time_ms, verified_sources, expected_occurrences).await?;
        let resolved: std::collections::HashMap<_, _> = unique_order.into_iter().zip(result.doc_ids).collect();
        result.doc_ids = input_order.into_iter().map(|id| resolved.get(&id).cloned().context("Missing committed source identity")).collect::<Result<_>>()?;
        Ok(result)
    }

    /// Prepare a complete replacement before changing either index. Ordinary
    /// ingest is idempotent too; this entry point preserves its public API.
    pub async fn reingest_document(&self, raw: RawDocument) -> Result<IngestStats> {
        self.ingest_document(raw).await
    }

    /// Level-1 ingest: write a single metadata-only row for each input file.
    ///
    /// No text extraction, no embedding. Goes through the background writer
    /// task (same as L3) so all LanceDB mutations are serialised.
    pub async fn ingest_l1(&self, files: &[L1FileEntry]) -> Result<IngestStats> {
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let chunks: Vec<DocumentChunk> = files
            .iter()
            .map(|f| {
                let meta = serde_json::json!({
                    "level":      1,
                    "fs_size":    f.size,
                    "fs_mtime":   f.mtime_ms,
                    "fs_ctime":   f.ctime_ms,
                    "parent_dir": f.parent_dir,
                });
                let doc_id = f.doc_id.clone();
                DocumentChunk {
                    id: chunk_row_id(&doc_id, -1),
                    doc_id,
                    location_uri: f.location_uri.clone(),
                    owner_id: f.owner_id.clone(),
                    filename: Some(f.filename.clone()),
                    title: None,
                    author: None,
                    year: None,
                    ext: Some(f.ext.clone()),
                    language: None,
                    page_count: None,
                    headings_text: None,
                    full_text: None,
                    full_text_md: None,
                    embedding: None,
                    embedding_sparse: None,
                    embedding_model: None,
                    chunk_index: -1,
                    chunk_total: 0,
                    chunk_start_char: None,
                    chunk_end_char: None,
                    indexed_at: now_ms,
                    source_hash: f.source_hash.clone(),
                    tags: vec![],
                    metadata_json: Some(meta.to_string()),
                    parent_dir: Some(f.parent_dir.clone()),
                    volume_id: f.volume_id.clone(),
                    // L1 catalog rows aren't extracted text; no
                    // translation possible.
                    text_translated: None,
                    text_translated_lang: None,
                    // L1 manifest-only writes don't have audio L2
                    // metadata — the symphonia probe runs during
                    // L3 extraction.  Promotes via Step 8 will
                    // patch these fields when transcribing.
                    audio_duration_seconds: None,
                    audio_codec: None,
                    audio_sample_rate_hz: None,
                    audio_channels: None,
                    audio_bitrate_kbps: None,
                    image_camera_make: None,
                    image_camera_model: None,
                    image_lens_model: None,
                    image_taken_at_unix: None,
                    image_iso: None,
                    multivec_packed: None,
                    multivec_n_tokens: None,
                    url: None,
                    embedding_omni: None,
                    embedding_vit: None,
                    summary: None,
                    doc_status: None,
                }
            })
            .collect();

        let total = chunks.len();
        // L1 has no Tantivy entries (no full-text body yet).
        self.submit_and_await(chunks, vec![], total, 0, Vec::new(), Vec::new()).await
    }

    /// Level-2 fallback: write one metadata-only row for a file whose L3
    /// extraction failed or was skipped. Carries the L2 title/author/year
    /// (from `l2_metadata::read`) plus an `extraction_failure` blob so
    /// Übersicht can show the right badge and subsequent runs can skip/retry.
    ///
    /// Like `ingest_l1`, this goes through the background writer task —
    /// no embedding is produced.
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_l2_row(
        &self,
        doc_id: String,
        location_uri: String,
        owner_id: String,
        filename: String,
        ext: String,
        source_hash: String,
        // Filesystem metadata
        mtime_unix: Option<i64>,
        file_size: Option<i64>,
        parent_dir: Option<String>,
        volume_id: Option<String>,
        // L2 metadata (best-effort; None when not available)
        title: Option<String>,
        author: Option<String>,
        year: Option<i32>,
        language: Option<String>,
        page_count: Option<i32>,
        // Why L3 failed — stored so Übersicht can render the badge.
        failure_reason: &super::task_failure::TaskFailureReason,
        failure_msg: &str,
    ) -> Result<IngestStats> {
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut meta = serde_json::json!({
            "level": 2,
            "extraction_failure": {
                "reason": failure_reason.as_tag(),
                // Byte-sliced, so the 512 cap has to land on a character
                // boundary or this panics — and the message routinely carries
                // a file path, which is exactly where non-ASCII shows up.
                // Recording *why* extraction failed must not itself crash the
                // ingest that was already handling a failure.
                "msg": truncate_on_char_boundary(failure_msg, 512),
                "at": now_ms
            }
        });
        if let Some(m) = mtime_unix {
            meta["mtime_unix"] = serde_json::Value::from(m);
            meta["fs_mtime"] = serde_json::Value::from(m.saturating_mul(1000));
        }
        if let Some(s) = file_size {
            meta["fs_size"] = serde_json::Value::from(s);
        }
        if let Some(ref pd) = parent_dir {
            meta["parent_dir"] = serde_json::Value::from(pd.as_str());
        }
        if let Some(ref v) = volume_id {
            meta["volume_id"] = serde_json::Value::from(v.as_str());
        }
        if let Some(pc) = page_count {
            meta["page_count"] = serde_json::Value::from(pc);
        }

        let chunk = DocumentChunk {
            id: chunk_row_id(&doc_id, -1),
            doc_id,
            location_uri,
            owner_id,
            filename: Some(filename),
            title,
            author,
            year,
            ext: Some(ext),
            language,
            page_count,
            headings_text: None,
            full_text: None,
            full_text_md: None,
            embedding: None,
            embedding_sparse: None,
            embedding_model: None,
            chunk_index: -1,
            chunk_total: 0,
            chunk_start_char: None,
            chunk_end_char: None,
            indexed_at: now_ms,
            source_hash,
            tags: vec![],
            metadata_json: Some(meta.to_string()),
            parent_dir,
            volume_id,
            // L2 metadata-only rows (failed extraction) have no text
            // to translate.
            text_translated: None,
            text_translated_lang: None,
            // No symphonia probe runs on L2-fallback paths — Step 8
            // promote can patch these fields if the user re-runs
            // extraction via the "Transcribe" search-result action.
            audio_duration_seconds: None,
            audio_codec: None,
            audio_sample_rate_hz: None,
            audio_channels: None,
            audio_bitrate_kbps: None,
            image_camera_make: None,
            image_camera_model: None,
            image_lens_model: None,
            image_taken_at_unix: None,
            image_iso: None,
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
            embedding_omni: None,
            embedding_vit: None,
            summary: None,
            doc_status: None,
        };

        self.submit_and_await(vec![chunk], vec![], 1, 0, Vec::new(), Vec::new()).await
    }
}

/// One file's filesystem-only metadata for the L1 ingest path.
///
/// Frontend sends camelCase (Tauri convention) so the deserialisation
/// needs the rename: `docId` -> `doc_id`, `mtimeMs` -> `mtime_ms`, etc.
/// `volume_id` is optional (`#[serde(default)]`) so existing frontends that
/// don't send it yet don't break — they get `None` and the row is simply not
/// filtered by volume availability.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct L1FileEntry {
    pub doc_id: String,
    pub source_hash: String,
    pub location_uri: String,
    pub owner_id: String,
    pub filename: String,
    pub ext: String,
    pub parent_dir: String,
    pub size: i64,
    pub mtime_ms: i64,
    pub ctime_ms: i64,
    /// Stable volume identifier (diskutil UUID on macOS, blkid UUID on
    /// Linux, hex VolumeSerialNumber on Windows). `None` when the source
    /// drive was not mounted at ingest time (e.g., offline .caf import)
    /// or when the caller did not supply one.
    #[serde(default)]
    pub volume_id: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Derive the preferred identity of an owner and source occurrence. Existing
/// stored identities can be reused by the writer; see `IngestStats::doc_ids`.
pub fn doc_id_for(raw: &RawDocument) -> String {
    super::source_identity::occurrence_id(&raw.owner_id, &raw.location_uri, &raw.source_hash)
}

/// P19 — merge NER entity tags into a document's existing tags, deduping
/// case-insensitively while preserving order (existing tags first, then new
/// entity tags in score order). Keeps the first-seen casing.
/// Truncate `s` to at most `max_bytes`, never splitting a character.
///
/// `&s[..n]` panics when `n` lands inside a multi-byte character, which is
/// routine for any text that is not pure ASCII. Every cap-a-string-at-N site
/// needs this; doing it by hand is how a German umlaut took down an ingest.
pub(crate) fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

pub(crate) fn merge_tags(tags: &mut Vec<String>, new_tags: Vec<String>) {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = tags.iter().map(|t| t.to_lowercase()).collect();
    for t in new_tags {
        if seen.insert(t.to_lowercase()) {
            tags.push(t);
        }
    }
}

/// Build the unique row `id` = hash(doc_id + ":" + chunk_index).
/// Avoids accidental collisions when the same document is re-chunked.
pub fn chunk_row_id(doc_id: &str, chunk_index: i32) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    doc_id.hash(&mut h);
    chunk_index.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Pack `Vec<Vec<f32>>` (ColBERT token vecs) into little-endian f32 bytes.
/// Returns `(bytes, n_tokens)` or `None` when the input is empty.
pub(crate) fn pack_multivec(vecs: Vec<Vec<f32>>) -> Option<(Vec<u8>, i16)> {
    if vecs.is_empty() { return None; }
    let n = vecs.len();
    let dim = vecs[0].len();
    if dim == 0 { return None; }
    let mut bytes = Vec::with_capacity(n * dim * 4);
    for vec in &vecs {
        for &f in vec {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    Some((bytes, n as i16))
}

/// Convert a `TextChunk` + `RawDocument` metadata + embedding into a `DocumentChunk`.
fn build_doc_chunk(
    tc: &super::embedder::TextChunk,
    raw: &RawDocument,
    chunk_total: i32,
    embedding: Option<Vec<f32>>,
    sparse_json: Option<String>,
    model_id: String,
    multivec: Option<Vec<Vec<f32>>>,
) -> DocumentChunk {
    let (multivec_packed, multivec_n_tokens) = multivec
        .and_then(pack_multivec)
        .map(|(b, n)| (Some(b), Some(n)))
        .unwrap_or((None, None));
    let doc_id = doc_id_for(raw);
    let id = chunk_row_id(&doc_id, tc.chunk_index);

    DocumentChunk {
        id,
        doc_id,
        location_uri: raw.location_uri.clone(),
        owner_id: raw.owner_id.clone(),
        filename: Some(raw.filename.clone()),
        title: raw.title.clone(),
        author: raw.author.clone(),
        year: raw.year,
        ext: Some(raw.ext.clone()),
        language: Some(raw.language.clone()),
        page_count: None,
        headings_text: if raw.headings.is_empty() {
            None
        } else {
            Some(raw.headings.join(" "))
        },
        full_text: Some(tc.text.clone()),
        full_text_md: Some(raw.full_text_md.clone()),
        embedding,
        embedding_sparse: sparse_json,
        embedding_model: if model_id.is_empty() { None } else { Some(model_id) },
        chunk_index: tc.chunk_index,
        chunk_total,
        chunk_start_char: Some(tc.start_char as i32),
        chunk_end_char: Some(tc.end_char as i32),
        indexed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        source_hash: raw.source_hash.clone(),
        tags: raw.tags.clone(),
        // PLAN P7.4.3 + P7.6 — pack source-file mtime and volume id
        // into `metadata_json` so they round-trip without a schema
        // migration. Order is stable (mtime_unix first) so the tiny
        // hand-parser in `LocalIndex::indexed_mtime_for_uri` keeps
        // working — it only finds the `mtime_unix` key and reads digits
        // up to the next non-digit (which is `,` when volume_id is
        // present, `}` when it isn't).
        metadata_json: build_metadata_json(
            raw.mtime_unix,
            raw.file_size,
            raw.volume_id.as_deref(),
            raw.images_indexed,
        ),
        parent_dir: raw.parent_dir.clone(),
        volume_id: raw.volume_id.clone(),
        // Stage AA: store translation only on chunk_index=0 (the
        // representative chunk).  Sub-chunks skip it to avoid O(N)
        // replication; migration v104 nulls legacy copies.
        text_translated: if tc.chunk_index == 0 { raw.translated_text.clone() } else { None },
        text_translated_lang: if tc.chunk_index == 0 { raw.translated_to_lang.clone() } else { None },
        // P13.6 Step 7 — replicate the per-doc audio L2 metadata
        // across every chunk row (same wasteful-but-simple
        // convention as text_translated).  None when raw came
        // from a non-audio extractor.
        audio_duration_seconds: raw.audio_duration_seconds,
        audio_codec: raw.audio_codec.clone(),
        audio_sample_rate_hz: raw.audio_sample_rate_hz,
        audio_channels: raw.audio_channels,
        audio_bitrate_kbps: raw.audio_bitrate_kbps,
        // P13.6 Step 9 — image L2 carries through the same way.
        image_camera_make: raw.image_camera_make.clone(),
        image_camera_model: raw.image_camera_model.clone(),
        image_lens_model: raw.image_lens_model.clone(),
        image_taken_at_unix: raw.image_taken_at_unix,
        image_iso: raw.image_iso,
        // Stage AD — ColBERT per-token vectors, packed as LE f32 bytes.
        multivec_packed,
        multivec_n_tokens,
        // v106 — Source URL carried from RawDocument (extractor lifted
        // it from YAML frontmatter / XMP / dc:source).
        url: raw.url.clone(),
        embedding_omni: raw.embedding_omni.clone(),
        embedding_vit: raw.embedding_vit.clone(),
        // P22 — extractive summary: first 2–3 sentences (≤ 300 chars).
        // Only on the representative chunk (chunk_index = 0) to avoid
        // replication across sub-chunks.
        summary: if tc.chunk_index == 0 {
            super::summary::extractive_summary(&raw.full_text, 300)
        } else {
            None
        },
        // P26.8 — doc_status is set via index_set_doc_status after ingest;
        // new chunks always start with NULL.
        doc_status: None,
    }
}

fn build_metadata_json(
    mtime_unix: Option<i64>,
    file_size:  Option<i64>,
    volume_id:  Option<&str>,
    images_indexed: bool,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = mtime_unix {
        // mtime_unix (seconds) is read by the tiny hand-parser in
        // `indexed_mtime_for_uri`; it must stay first in the object so
        // the parser's digit-scan terminates at the comma.
        parts.push(format!(r#""mtime_unix":{m}"#));
        // fs_mtime (milliseconds) is what the Übersicht frontend reads
        // for the "Geändert" column (same as L1 rows).
        parts.push(format!(r#""fs_mtime":{}"#, m.saturating_mul(1000)));
    }
    if let Some(s) = file_size {
        parts.push(format!(r#""fs_size":{s}"#));
    }
    if let Some(v) = volume_id {
        // Volume ids are UUIDs / hex serials in practice (no quotes,
        // no backslashes), but escape defensively.
        let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
        parts.push(format!(r#""volume_id":"{escaped}""#));
    }
    // Only present when true: absence is what makes an older row (or a row
    // written with the feature off) look stale to the re-ingest guard.
    if images_indexed {
        parts.push(r#""images_indexed":true"#.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("{{{}}}", parts.join(",")))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::embedder::TextChunk;

    #[test]
    fn truncate_on_char_boundary_never_splits_a_character() {
        // 'ü' is two bytes, so a cap of 1 lands inside it.
        assert_eq!(truncate_on_char_boundary("üü", 1), "");
        assert_eq!(truncate_on_char_boundary("üü", 2), "ü");
        assert_eq!(truncate_on_char_boundary("üü", 3), "ü");
        assert_eq!(truncate_on_char_boundary("üü", 4), "üü");
        // Shorter than the cap is returned whole.
        assert_eq!(truncate_on_char_boundary("abc", 99), "abc");
        // Sweep every cap over mixed-width text; the contract is simply that
        // it does not panic and the result stays a prefix.
        let s = "Grundstück-Gebührenordnung-Fördermittel-Prüfung";
        for n in 0..=s.len() + 2 {
            let got = truncate_on_char_boundary(s, n);
            assert!(s.starts_with(got), "n={n} produced a non-prefix: {got:?}");
            assert!(got.len() <= n.max(0) || got.len() == s.len());
        }
    }

    fn sample_raw() -> RawDocument {
        RawDocument {
            full_text: "The quick brown fox jumps over the lazy dog.".to_owned(),
            full_text_md: "**The quick brown fox** jumps over the lazy dog.".to_owned(),
            headings: vec!["Introduction".to_owned()],
            title: Some("Test Doc".to_owned()),
            author: Some("Author A".to_owned()),
            year: Some(2024),
            filename: "test.pdf".to_owned(),
            ext: "pdf".to_owned(),
            language: "en".to_owned(),
            source_hash: "abc123def456".to_owned(),
            location_uri: "crisp+local://user@machine/test.pdf".to_owned(),
            owner_id: "user-uuid".to_owned(),
            tags: vec!["theology".to_owned()],
            mtime_unix: None,
            images_indexed: false,
            file_size:  None,
            volume_id:  None,
            parent_dir: None,
            translated_text: None,
            translated_to_lang: None,
            audio_duration_seconds: None,
            audio_codec: None,
            audio_sample_rate_hz: None,
            audio_channels: None,
            audio_bitrate_kbps: None,
            image_camera_make: None,
            image_camera_model: None,
            image_lens_model: None,
            image_taken_at_unix: None,
            image_iso: None,
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
            embedding_omni: None,
            embedding_vit: None,
        }
    }

    #[tokio::test]
    async fn text_only_ingest_is_searchable_without_an_embedder() {
        let dir = tempfile::tempdir().unwrap();
        let fts = std::sync::Arc::new(crate::index::FtsIndex::open_or_create(&dir.path().join("fts")).unwrap());
        let local = std::sync::Arc::new(crate::index::LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
        let pipeline = IngestPipeline::new(fts.clone(), local.clone(), None,
            IngestConfig { text_only: true, ..Default::default() });
        let mut raw = sample_raw();
        raw.url = Some("https://example.invalid/article".into());
        let stats = pipeline.ingest_document(raw.clone()).await.unwrap();
        assert_eq!(stats.chunk_count, 1);
        assert_eq!(stats.embed_time_ms, 0);
        assert_eq!(local.fetch_full_text(&doc_id_for(&raw)).await.unwrap(), raw.full_text);
        assert!(local.list_chunks_with_embeddings(0, 100).await.unwrap().is_empty());
        let batches = local.fetch_by_doc_ids(&[doc_id_for(&raw)]).await.unwrap();
        assert!(batches[0].column_by_name("embedding").unwrap().is_null(0));
        assert_eq!(fts.search("fox", &crate::index::schema::SearchFilters::default(), 10).unwrap().len(), 1);
        let engine = crate::index::search::SearchEngine::new(fts.clone(), local.clone(), None);
        let hits = engine.search_hybrid("fox", &crate::index::schema::SearchFilters::default(), 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url.as_deref(), Some("https://example.invalid/article"));
        pipeline.ingest_document(raw).await.unwrap();
        assert_eq!(fts.search("fox", &crate::index::schema::SearchFilters::default(), 10).unwrap().len(), 1);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
    }

    async fn test_indexes() -> (tempfile::TempDir, Arc<FtsIndex>, Arc<LocalIndex>, IngestPipeline) {
        let dir = tempfile::tempdir().unwrap();
        let fts = Arc::new(FtsIndex::open_or_create(&dir.path().join("fts")).unwrap());
        let local = Arc::new(LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
        let pipeline = IngestPipeline::new(fts.clone(), local.clone(), None,
            IngestConfig { text_only: true, ..Default::default() });
        (dir, fts, local, pipeline)
    }

    fn pending_for(raw: &RawDocument, replace_ids: Vec<String>) -> PendingFtsWrite {
        PendingFtsWrite {
            generation: uuid::Uuid::new_v4().to_string(),
            doc_ids: vec![doc_id_for(&raw)], replace_ids,
            inputs: vec![TantivyInputOwned {
                doc_id: doc_id_for(raw), owner_id: raw.owner_id.clone(),
                language: raw.language.clone(), title: raw.title.clone().unwrap_or_default(),
                headings: raw.headings.join(" "), body: raw.full_text.clone(),
                body_translated: raw.translated_text.clone(),
            }],
            receipt_rows: Vec::new(), delete_only: false, absent_occurrences: Vec::new(),
        }
    }

    fn one_chunk(raw: &RawDocument) -> DocumentChunk {
        build_doc_chunk(&super::super::embedder::TextChunk {
            text: raw.full_text.clone(), start_char: 0, end_char: raw.full_text.len(), chunk_index: 0,
        }, raw, 1, None, None, String::new(), None)
    }

    #[tokio::test]
    async fn reingest_replaces_legacy_duplicates_and_stale_chunks() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        pipeline.ingest_document(raw.clone()).await.unwrap();
        // Model an older append-only index containing both duplicate rows and
        // a tail chunk left over from a longer previous extraction.
        let mut tail = one_chunk(&raw);
        tail.id = chunk_row_id(&doc_id_for(&raw), 1);
        tail.chunk_index = 1;
        tail.chunk_total = 2;
        tail.full_text = Some("obsolete tail".into());
        local.append_legacy_rows_for_test(&[one_chunk(&raw), tail]).await.unwrap();
        let mut replacement = raw.clone();
        replacement.full_text = "Replacement".into();
        pipeline.reingest_document(replacement.clone()).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(local.fetch_full_text(&doc_id_for(&raw)).await.unwrap(), "Replacement");
        assert!(fts.search("fox", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(fts.search("replacement", &Default::default(), 10).unwrap().len(), 1);
        pipeline.ingest_document(replacement).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn failed_preparation_retains_last_good_document() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let original = sample_raw();
        pipeline.ingest_document(original.clone()).await.unwrap();
        let mut empty = original.clone();
        empty.full_text = " \n\t ".into();
        assert!(pipeline.reingest_document(empty).await.is_err());
        let missing_model = IngestPipeline::new(fts.clone(), local.clone(), None, IngestConfig::default());
        assert!(missing_model.reingest_document(original.clone()).await.is_err());
        assert_eq!(local.fetch_full_text(&doc_id_for(&original)).await.unwrap(), original.full_text);
        assert_eq!(fts.search("fox", &Default::default(), 10).unwrap().len(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn source_change_replaces_old_identity_without_touching_other_sources() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let original = sample_raw();
        let mut other = sample_raw();
        other.source_hash = "other-source".into();
        other.location_uri = "crisp+local://user@machine/other.pdf".into();
        other.full_text = "Unrelated document".into();
        pipeline.ingest_documents_batch(vec![original.clone(), other.clone()]).await.unwrap();
        let mut changed = original.clone();
        changed.source_hash = "new-content-hash".into();
        changed.full_text = "Changed document".into();
        pipeline.ingest_document(changed.clone()).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 2);
        assert_eq!(local.fetch_full_text(&doc_id_for(&original)).await.unwrap(), changed.full_text);
        assert_eq!(local.fetch_full_text(&doc_id_for(&other)).await.unwrap(), other.full_text);
        assert!(fts.search("fox", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(fts.search("changed", &Default::default(), 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn uncommitted_journal_intent_never_changes_searchable_text() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let original = sample_raw();
        pipeline.ingest_document(original.clone()).await.unwrap();
        let mut failed = original.clone();
        failed.full_text = "Uncommitted replacement".into();
        let pending = pending_for(&failed, vec![doc_id_for(&original)]);
        record_fts_intent(local.ingest_journal_path(), &pending).unwrap();
        repair_fts_writes(&fts, &local).await.unwrap();
        assert_eq!(fts.search("fox", &Default::default(), 10).unwrap().len(), 1);
        assert!(fts.search("uncommitted", &Default::default(), 10).unwrap().is_empty());
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn committed_vector_generation_replays_exact_full_text_once() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let original = sample_raw();
        pipeline.ingest_document(original.clone()).await.unwrap();
        let mut changed = original.clone();
        changed.source_hash = "replacement-hash".into();
        changed.full_text = "First passage with Unicode 日本語. Exact complete body contains endmarker.".into();
        let pending = pending_for(&changed, vec![doc_id_for(&original)]);
        record_fts_intent(local.ingest_journal_path(), &pending).unwrap();
        local.replace_documents(&mut [one_chunk(&changed)], &pending.generation).await.unwrap();
        // Simulate process death after Lance commit but before full-text commit.
        let reopened = LocalIndex::open_or_create(_dir.path(), 4).await.unwrap();
        repair_fts_writes(&fts, &reopened).await.unwrap();
        repair_fts_writes(&fts, &reopened).await.unwrap();
        assert_eq!(fts.search("endmarker", &Default::default(), 10).unwrap().len(), 1);
        assert!(fts.search("fox", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(fts.doc_count(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_fts_commit_is_durable_and_repaired_before_next_write() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let original = sample_raw();
        let held_writer = fts.writer().unwrap();
        let error = pipeline.ingest_document(original.clone()).await.unwrap_err();
        assert!(format!("{error:#}").contains("repair is pending"));
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(pending_fts_writes(local.ingest_journal_path()).unwrap().len(), 1);
        drop(held_writer);
        pipeline.ingest_document(original).await.unwrap();
        assert_eq!(fts.doc_count(), 1);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_refresh_keeps_text_and_exposes_retryable_failure() {
        let (_dir, _fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        pipeline.ingest_document(raw.clone()).await.unwrap();
        pipeline.ingest_l2_row(raw.source_hash.clone(), raw.location_uri.clone(),
            raw.owner_id.clone(), raw.filename.clone(), raw.ext.clone(), raw.source_hash.clone(),
            Some(123), Some(100), None, None, None, None, None, None, None,
            &super::super::task_failure::TaskFailureReason::EmptyText, "No text").await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(local.fetch_full_text(&doc_id_for(&raw)).await.unwrap(), raw.full_text);
        assert_eq!(local.document_metadata(&doc_id_for(&raw)).await.unwrap()["extraction_failure"]["reason"], "empty_text");
        local.clear_extraction_failure(&doc_id_for(&raw)).await.unwrap();
        let metadata = local.document_metadata(&doc_id_for(&raw)).await.unwrap();
        assert!(metadata.get("extraction_failure").is_none());
        assert!(metadata.get("mtime_unix").is_none());
        assert_eq!(metadata["level"], 3);
    }

    #[tokio::test]
    async fn metadata_ingestion_is_idempotent_and_cannot_downgrade_text() {
        let (_dir, _fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        let entry = L1FileEntry { doc_id: raw.source_hash.clone(), source_hash: raw.source_hash.clone(),
            location_uri: raw.location_uri.clone(), owner_id: raw.owner_id.clone(),
            filename: raw.filename.clone(), ext: raw.ext.clone(), parent_dir: "/".into(),
            size: 123, mtime_ms: 1, ctime_ms: 1, volume_id: None };
        pipeline.ingest_l1(&[entry.clone()]).await.unwrap();
        pipeline.ingest_l1(&[entry.clone()]).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        pipeline.ingest_document(raw.clone()).await.unwrap();
        pipeline.ingest_l1(&[entry]).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(local.fetch_full_text(&raw.source_hash).await.unwrap(), raw.full_text);
    }

    #[tokio::test]
    async fn reopened_pipeline_reports_and_repairs_pending_full_text_before_ready() {
        let (dir, fts, local, pipeline) = test_indexes().await;
        let held_writer = fts.writer().unwrap();
        assert!(pipeline.ingest_document(sample_raw()).await.is_err());
        let reopened_local = Arc::new(LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
        let reopened = IngestPipeline::new(fts.clone(), reopened_local, None,
            IngestConfig { text_only: true, ..Default::default() });
        assert!(reopened.ensure_ready().await.is_err());
        drop(held_writer);
        reopened.ensure_ready().await.unwrap();
        assert_eq!(fts.search("fox", &Default::default(), 10).unwrap().len(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrent_pipeline_instances_do_not_duplicate_or_drop_generations() {
        let (_dir, fts, local, first) = test_indexes().await;
        let second = IngestPipeline::new(fts.clone(), local.clone(), None,
            IngestConfig { text_only: true, ..Default::default() });
        let mut left = sample_raw();
        left.full_text = "First revision".into();
        let mut right = left.clone();
        right.full_text = "Second revision".into();
        let (a, b) = tokio::join!(first.ingest_document(left), second.ingest_document(right));
        a.unwrap(); b.unwrap();
        first.ensure_ready().await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(fts.doc_count(), 1);
        assert_eq!(fts.search("revision", &Default::default(), 10).unwrap().len(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_auxiliary_vector_retains_the_previous_index_and_writer() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        pipeline.ingest_document(raw.clone()).await.unwrap();
        for vector in [vec![], vec![f32::NAN; 768], vec![0.0; 767]] {
            let mut invalid = raw.clone();
            invalid.embedding_vit = Some(vector);
            assert!(pipeline.reingest_document(invalid).await.is_err());
            assert_eq!(local.fetch_full_text(&doc_id_for(&raw)).await.unwrap(), raw.full_text);
        }
        pipeline.ingest_document(raw).await.unwrap();
        assert_eq!(fts.search("fox", &Default::default(), 10).unwrap().len(), 1);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(pipeline.queue_depth(), 0);
    }

    #[tokio::test]
    async fn imported_keep_both_preserves_local_until_explicit_replacement() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let local_raw = sample_raw();
        pipeline.ingest_document(local_raw.clone()).await.unwrap();
        let mut remote = local_raw.clone();
        remote.source_hash = "remote-copy".into();
        remote.full_text = "Remote edition".into();
        let mut remote_chunk = one_chunk(&remote);
        remote_chunk.doc_id = "explicit-import-copy".into();
        remote_chunk.id = chunk_row_id(&remote_chunk.doc_id, 0);
        local.ingest_batch_with_fts(&[remote_chunk.clone()], &[], &fts).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 2);
        assert_eq!(fts.doc_count(), 2);
        pipeline.ingest_document(local_raw.clone()).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 2);
        assert_eq!(local.fetch_full_text(&remote_chunk.doc_id).await.unwrap(), remote.full_text);
        assert_eq!(fts.search("fox", &Default::default(), 10).unwrap().len(), 1);
        local.ingest_batch_with_fts(&[remote_chunk.clone()], &[doc_id_for(&local_raw)], &fts).await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert!(fts.search("fox", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(fts.search("edition", &Default::default(), 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn durable_delete_cannot_be_resurrected_by_a_pending_write() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        let held_writer = fts.writer().unwrap();
        assert!(pipeline.ingest_document(raw.clone()).await.is_err());
        drop(held_writer);
        local.delete_document_with_fts(&doc_id_for(&raw), &fts).await.unwrap();
        pipeline.ensure_ready().await.unwrap();
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 0);
        assert_eq!(fts.doc_count(), 0);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn partial_generation_receipts_are_retained_and_block_readiness() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let guard = local.read_guard(&fts).await.unwrap();
        let raw = sample_raw();
        let mut pending = pending_for(&raw, vec![doc_id_for(&raw)]);
        pending.doc_ids.push("missing-document".into());
        record_fts_intent(local.ingest_journal_path(), &pending).unwrap();
        local.replace_documents(&mut [one_chunk(&raw)], &pending.generation).await.unwrap();
        drop(guard);
        assert!(pipeline.ensure_ready().await.is_err());
        assert_eq!(pending_fts_writes(local.ingest_journal_path()).unwrap().len(), 1);
        assert_eq!(fts.doc_count(), 0);
    }

    #[tokio::test]
    async fn conditional_remote_delete_retains_newer_local_edits_and_independent_copies() {
        let (_dir, fts, local, _pipeline) = test_indexes().await;
        let mut raw = sample_raw();
        raw.full_text = "originalmarker".into();
        let mut original = one_chunk(&raw);
        let original_id = original.doc_id.clone();
        let mut copy_raw = raw.clone();
        copy_raw.source_hash = "kept-independent-copy".into();
        copy_raw.full_text = "retainedmarker".into();
        let mut copy = one_chunk(&copy_raw);
        copy.doc_id = format!("{original_id}_remote");
        copy.id = chunk_row_id(&copy.doc_id, 0);
        local.ingest_batch_with_fts(&[original.clone(), copy.clone()], &[], &fts).await.unwrap();
        let observed_version = local.table_ref().version().await.unwrap();
        original.title = Some("Updated local title".into());
        local.ingest_batch_with_fts(&[original], &[], &fts).await.unwrap();
        assert!(!local.delete_document_if_version_with_fts(&original_id, observed_version, &fts).await.unwrap());
        assert_eq!(fts.search("originalmarker", &Default::default(), 10).unwrap().len(), 1);
        let current_version = local.table_ref().version().await.unwrap();
        assert!(local.delete_document_if_version_with_fts(&original_id, current_version, &fts).await.unwrap());
        assert!(fts.search("originalmarker", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(local.fetch_full_text(&copy.doc_id).await.unwrap(), "retainedmarker");
        assert_eq!(fts.search("retainedmarker", &Default::default(), 10).unwrap().len(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn imported_replacements_reject_ambiguous_or_foreign_owner_identities() {
        let (_dir, fts, local, _pipeline) = test_indexes().await;
        let raw = sample_raw();
        let original = one_chunk(&raw);
        local.ingest_batch_with_fts(&[original.clone()], &[], &fts).await.unwrap();
        let before = local.table_ref().version().await.unwrap();
        let mut foreign = original.clone();
        foreign.owner_id = "different-owner".into();
        assert!(local.ingest_batch_with_fts(&[foreign], &[], &fts).await.is_err());
        assert_eq!(local.table_ref().version().await.unwrap(), before);

        let mut moved = original.clone();
        moved.location_uri = "/same-owner/new-location.txt".into();
        local.ingest_batch_with_fts(&[moved.clone()], &[], &fts).await.unwrap();
        assert_eq!(local.fetch_document_summary(&original.doc_id).await.unwrap().unwrap().location_uri, moved.location_uri);

        seed_shared_legacy(&local, &fts, &collision_sources()).await;
        let before = local.table_ref().version().await.unwrap();
        let rows_before = local.table_ref().count_rows(None).await.unwrap();
        let mut collision = original.clone();
        collision.doc_id = "shared-legacy".into();
        collision.id = chunk_row_id(&collision.doc_id, 0);
        assert!(local.ingest_batch_with_fts(&[collision], &[], &fts).await.is_err());
        assert!(local.ingest_batch_with_fts(&[moved], &["shared-legacy".into()], &fts).await.is_err());
        assert!(local.delete_document_with_fts("shared-legacy", &fts).await.is_err());
        assert_eq!(local.table_ref().version().await.unwrap(), before);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), rows_before);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn same_content_remote_metadata_obeys_policy_without_replacing_reviewed_rows() {
        use crate::sync::conflict::{resolve_conflict, ConflictPolicy, ConflictSide, Resolution};
        use arrow_array::{FixedSizeListArray, Float32Array, StringArray};
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let (_dir, fts, local, _pipeline) = test_indexes().await;
        let raw = sample_raw();
        let mut original = one_chunk(&raw);
        original.title = Some("Originaltitle".into());
        original.indexed_at = 10;
        original.embedding = Some(vec![0.25; 4]);
        original.metadata_json = Some(r#"{"level":3,"reviewed":true,"evidence":{"page":7},"user_note":"keep"}"#.into());
        local.ingest_batch_with_fts(&[original.clone()], &[], &fts).await.unwrap();
        let local_side = ConflictSide { doc_id: original.doc_id.clone(), source_hash: original.source_hash.clone(),
            updated_at: Some(10), title: original.title.clone() };
        let remote_side = ConflictSide { doc_id: original.doc_id.clone(), source_hash: original.source_hash.clone(),
            updated_at: Some(20), title: Some("Remotetitle".into()) };
        for policy in [ConflictPolicy::LocalWins, ConflictPolicy::KeepBoth] {
            assert_eq!(resolve_conflict(&local_side, &remote_side, policy), Resolution::UseLocal);
            let current = local.fetch_document_summary(&original.doc_id).await.unwrap().unwrap();
            assert_eq!(current.location_uri, original.location_uri);
            assert_eq!(current.title, original.title);
        }
        assert_eq!(resolve_conflict(&local_side, &remote_side, ConflictPolicy::NewestWins), Resolution::UseRemote);
        let before = local.table_ref().version().await.unwrap();
        let update = || RemoteMetadata { doc_id: &original.doc_id, source_hash: &original.source_hash,
            owner_id: &original.owner_id, location_uri: "/remote/moved.txt", title: Some("Remotetitle"),
            author: None, year: None, language: Some("de"), indexed_at: 20, origin: "https://sync.example/" };
        assert!(apply_remote_metadata_if_version(&local, &fts, update(), before).await.unwrap());
        let current = local.fetch_document_summary(&original.doc_id).await.unwrap().unwrap();
        assert_eq!(current.location_uri, "/remote/moved.txt");
        assert_eq!(current.title.as_deref(), Some("Remotetitle"));
        assert_eq!(current.indexed_at, 20);
        assert_eq!(current.source_hash, original.source_hash);
        assert_eq!(local.fetch_full_text(&original.doc_id).await.unwrap(), raw.full_text);
        assert!(fts.search("title:Originaltitle", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(fts.search("title:Remotetitle", &Default::default(), 10).unwrap().len(), 1);
        let batches: Vec<arrow_array::RecordBatch> = local.table_ref().query().execute().await.unwrap().try_collect().await.unwrap();
        for batch in batches {
            let metadata = batch.column_by_name("metadata_json").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let vectors = batch.column_by_name("embedding").unwrap().as_any().downcast_ref::<FixedSizeListArray>().unwrap();
            for row in 0..batch.num_rows() {
                let value: serde_json::Value = serde_json::from_str(metadata.value(row)).unwrap();
                assert_eq!(value["level"], 3);
                assert_eq!(value["reviewed"], true);
                assert_eq!(value["evidence"]["page"], 7);
                assert_eq!(value["user_note"], "keep");
                assert_eq!(value["sync_remote_url"], "https://sync.example");
                let vector = vectors.value(row);
                assert_eq!(vector.as_any().downcast_ref::<Float32Array>().unwrap().values().as_ref(), &[0.25; 4]);
            }
        }
        assert!(!apply_remote_metadata_if_version(&local, &fts, update(), before).await.unwrap());
        local.update_l2_fields(&original.doc_id, Some("Localreviewtitle"), None, None, None, None, None).await.unwrap();
        let edited = local.fetch_document_summary(&original.doc_id).await.unwrap().unwrap();
        assert!(edited.indexed_at > current.indexed_at);
        let edited_side = ConflictSide { doc_id: edited.doc_id, source_hash: edited.source_hash,
            updated_at: Some(edited.indexed_at), title: edited.title };
        assert_eq!(resolve_conflict(&edited_side, &remote_side, ConflictPolicy::NewestWins), Resolution::UseLocal);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[test]
    fn doc_id_scopes_the_source_fingerprint() {
        let raw = sample_raw();
        assert_ne!(doc_id_for(&raw), raw.source_hash);
        assert_eq!(doc_id_for(&raw), super::super::source_identity::occurrence_id(&raw.owner_id, &raw.location_uri, &raw.source_hash));
    }

    #[test]
    fn chunk_row_id_is_deterministic() {
        assert_eq!(chunk_row_id("doc1", 0), chunk_row_id("doc1", 0));
        assert_ne!(chunk_row_id("doc1", 0), chunk_row_id("doc1", 1));
    }

    #[test]
    fn metadata_json_packs_both_fields() {
        assert_eq!(build_metadata_json(None, None, None, false), None);
        assert_eq!(
            build_metadata_json(Some(1700000000), None, None, false).as_deref(),
            Some(r#"{"mtime_unix":1700000000,"fs_mtime":1700000000000}"#)
        );
        assert_eq!(
            build_metadata_json(None, Some(12345), None, false).as_deref(),
            Some(r#"{"fs_size":12345}"#)
        );
        assert_eq!(
            build_metadata_json(None, None, Some("ABCD-1234"), false).as_deref(),
            Some(r#"{"volume_id":"ABCD-1234"}"#)
        );
        assert_eq!(
            build_metadata_json(Some(42), Some(999), Some("ABCD-1234"), false).as_deref(),
            Some(r#"{"mtime_unix":42,"fs_mtime":42000,"fs_size":999,"volume_id":"ABCD-1234"}"#)
        );
    }

    #[test]
    fn metadata_json_keeps_mtime_parser_compatible() {
        // The hand-parser in LocalIndex::indexed_mtime_for_uri reads
        // digits after `"mtime_unix":` up to the next non-digit. Adding
        // fs_mtime and other keys after must not break that contract.
        let s = build_metadata_json(Some(1700000000), Some(0), Some("ABCD"), false).unwrap();
        let start = s.find("\"mtime_unix\"").unwrap();
        let after = &s[start + "\"mtime_unix\"".len()..];
        let after = after.trim_start().strip_prefix(':').unwrap().trim_start();
        let end = after.find(|c: char| !c.is_ascii_digit()).unwrap();
        assert_eq!(&after[..end], "1700000000");
    }

    #[test]
    fn build_doc_chunk_fields_correct() {
        let raw = sample_raw();
        let tc = TextChunk {
            text: "The quick brown fox".to_owned(),
            start_char: 0,
            end_char: 19,
            chunk_index: 0,
        };
        let dc = build_doc_chunk(&tc, &raw, 2, Some(vec![0.1; 4]), None, "bge-m3".to_owned(), None);
        assert_eq!(dc.doc_id, doc_id_for(&raw));
        assert_eq!(dc.source_hash, raw.source_hash);
        assert_eq!(dc.chunk_index, 0);
        assert_eq!(dc.chunk_total, 2);
        assert_eq!(dc.language, Some("en".to_owned()));
        assert_eq!(dc.tags, vec!["theology".to_owned()]);
        assert!(dc.embedding.is_some());
    }

    #[test]
    fn doc_id_tracks_owner_and_location_independently_of_changed_content() {
        let raw = sample_raw();
        let mut changed = raw.clone();
        changed.source_hash = "new bytes".into();
        assert_eq!(doc_id_for(&raw), doc_id_for(&changed));
        changed.location_uri = "file:///second-copy.pdf".into();
        assert_ne!(doc_id_for(&raw), doc_id_for(&changed));
        changed = raw.clone();
        changed.owner_id = "another owner".into();
        assert_ne!(doc_id_for(&raw), doc_id_for(&changed));
    }

    #[test]
    fn pack_multivec_round_trips() {
        let vecs: Vec<Vec<f32>> = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let (bytes, n) = pack_multivec(vecs.clone()).expect("pack should succeed");
        assert_eq!(n, 2);
        assert_eq!(bytes.len(), 2 * 2 * 4);
        // Verify first f32
        let first = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert!((first - 1.0).abs() < 1e-6);
    }

    #[test]
    fn merge_tags_dedups_case_insensitively_preserving_order() {
        let mut tags = vec!["theology".to_owned(), "person:Obama".to_owned()];
        merge_tags(
            &mut tags,
            vec![
                "person:obama".to_owned(),  // dup of existing (case-insensitive)
                "org:United Nations".to_owned(),
                "loc:Hawaii".to_owned(),
            ],
        );
        assert_eq!(
            tags,
            vec![
                "theology".to_owned(),
                "person:Obama".to_owned(),
                "org:United Nations".to_owned(),
                "loc:Hawaii".to_owned(),
            ]
        );
    }

    #[test]
    fn pack_multivec_empty_returns_none() {
        assert!(pack_multivec(vec![]).is_none());
        assert!(pack_multivec(vec![vec![]]).is_none());
    }

    #[test]
    fn build_doc_chunk_with_multivec_populates_fields() {
        let raw = sample_raw();
        let tc = crate::index::embedder::TextChunk {
            text: "hello".to_owned(),
            start_char: 0,
            end_char: 5,
            chunk_index: 0,
        };
        let multivec = vec![vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]];
        let dc = build_doc_chunk(&tc, &raw, 1, None, None, "bge-m3".to_owned(), Some(multivec));
        assert!(dc.multivec_packed.is_some());
        assert_eq!(dc.multivec_n_tokens, Some(2));
    }

    #[test]
    fn lance_batch_size_scales_with_embed_batch() {
        // Verify the 16x multiplier produces a LanceDB-friendly batch size.
        let small = IngestConfig { batch_size: 32, ..Default::default() };
        let large = IngestConfig { batch_size: 64, ..Default::default() };
        let lance_small = small.batch_size.saturating_mul(16).max(256);
        let lance_large = large.batch_size.saturating_mul(16).max(256);
        assert_eq!(lance_small, 512, "32 × 16 = 512");
        assert_eq!(lance_large, 1024, "64 × 16 = 1024");
        // Minimum floor
        let tiny = IngestConfig { batch_size: 1, ..Default::default() };
        let lance_tiny = tiny.batch_size.saturating_mul(16).max(256);
        assert_eq!(lance_tiny, 256, "floor must be 256");
    }

    #[test]
    fn metadata_json_records_images_indexed_only_when_true() {
        // Presence means "written with image indexing on"; absence is what the
        // re-ingest guard reads as "may still need a backfill".
        let plain = build_metadata_json(Some(42), Some(10), None, false).unwrap();
        assert!(plain.contains(r#""mtime_unix":42"#));
        assert!(!plain.contains("images_indexed"));

        let on = build_metadata_json(Some(42), Some(10), None, true).unwrap();
        assert!(on.contains(r#""images_indexed":true"#));
        // mtime stays first so the digit-scan parser keeps terminating there.
        assert!(on.starts_with(r#"{"mtime_unix":42"#));
    }

    #[tokio::test]
    async fn metadata_refresh_updates_lexical_fields_and_preserves_each_chunk() {
        use arrow_array::{Array, FixedSizeListArray, Float32Array, Int32Array, StringArray};
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let (_dir, fts, local, _pipeline) = test_indexes().await;
        let mut raw = sample_raw();
        raw.title = Some("Oldtitle".into());
        let doc_id = doc_id_for(&raw);
        raw.full_text = "first overlap last".into();
        let mut first = build_doc_chunk(&TextChunk {
            text: "first overlap".into(), start_char: 0, end_char: "first overlap".len(), chunk_index: 0,
        }, &raw, 2, Some(vec![0.25; 4]), None, "test".into(), None);
        let mut second = build_doc_chunk(&TextChunk {
            text: "overlap last".into(), start_char: "first ".len(), end_char: raw.full_text.len(), chunk_index: 1,
        }, &raw, 2, Some(vec![0.5; 4]), None, "test".into(), None);
        first.metadata_json = Some(r#"{"chunk_note":"first","mtime_unix":42,"level":3}"#.into());
        second.metadata_json = Some(r#"{"chunk_note":"second","mtime_unix":42}"#.into());
        local.ingest_batch_with_fts(&[first, second], &[], &fts).await.unwrap();
        local.update_l2_fields(&doc_id, Some("Updatedtitle"), Some("O'Reilly"),
            Some(2026), Some("de"), Some(4), Some(r#"{"reviewed":true,"level":2}"#)).await.unwrap();

        assert!(fts.search("title:Oldtitle", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(fts.search("title:Updatedtitle", &Default::default(), 10).unwrap().len(), 1);
        assert_eq!(fts.search(r#""first overlap last""#, &Default::default(), 10).unwrap().len(), 1);
        // Language filtering is applied by result hydration. Verify that the
        // lexical store's exact language field also advances with the update.
        let reader = fts.index.reader().unwrap();
        let searcher = reader.searcher();
        let language_count = |language: &str| searcher.search(
            &tantivy::query::TermQuery::new(
                tantivy::Term::from_field_text(fts.fields.language, language),
                tantivy::schema::IndexRecordOption::Basic,
            ), &tantivy::collector::Count).unwrap();
        assert_eq!(language_count("de"), 1);
        assert_eq!(language_count("en"), 0);

        let batches: Vec<arrow_array::RecordBatch> = local.table_ref().query()
            .only_if(format!("doc_id = '{}'", doc_id)).execute().await.unwrap()
            .try_collect().await.unwrap();
        assert_eq!(batches.iter().map(|batch| batch.num_rows()).sum::<usize>(), 2);
        for batch in batches {
            let indices = batch.column_by_name("chunk_index").unwrap().as_any().downcast_ref::<Int32Array>().unwrap();
            let metadata = batch.column_by_name("metadata_json").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let body = batch.column_by_name("full_text").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let vectors = batch.column_by_name("embedding").unwrap().as_any().downcast_ref::<FixedSizeListArray>().unwrap();
            for row in 0..batch.num_rows() {
                let first = indices.value(row) == 0;
                let value: serde_json::Value = serde_json::from_str(metadata.value(row)).unwrap();
                assert_eq!(value["chunk_note"], if first { "first" } else { "second" });
                assert_eq!(value["mtime_unix"], 42);
                assert_eq!(value["reviewed"], true);
                assert_eq!(value["level"], 3, "a stale L2 update cannot downgrade extracted chunks");
                assert_eq!(body.value(row), if first { "first overlap" } else { "overlap last" });
                let vector = vectors.value(row);
                let vector = vector.as_any().downcast_ref::<Float32Array>().unwrap();
                assert!(vector.values().iter().all(|value| *value == if first { 0.25 } else { 0.5 }));
            }
        }
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_metadata_fts_write_recovers_before_reopened_search() {
        let (dir, fts, local, pipeline) = test_indexes().await;
        let mut raw = sample_raw();
        raw.title = Some("Oldtitle".into());
        let doc_id = doc_id_for(&raw);
        pipeline.ingest_document(raw.clone()).await.unwrap();
        let held_writer = fts.writer().unwrap();
        let error = local.update_l2_fields(&doc_id, Some("Repairedtitle"), None,
            None, Some("de"), None, Some(r#"{"reviewed":true}"#)).await.unwrap_err();
        assert!(format!("{error:#}").contains("full-text repair is pending"));
        assert_eq!(pending_fts_writes(local.ingest_journal_path()).unwrap().len(), 1);
        drop(held_writer);

        let reopened = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let reopened_fts = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
        let _guard = reopened.read_guard(&reopened_fts).await.unwrap();
        assert!(reopened_fts.search("title:Oldtitle", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(reopened_fts.search("title:Repairedtitle", &Default::default(), 10).unwrap().len(), 1);
        assert_eq!(reopened_fts.search("fox", &Default::default(), 10).unwrap().len(), 1);
        assert_eq!(reopened.fetch_full_text(&doc_id).await.unwrap(), raw.full_text);
        assert_eq!(reopened.document_metadata(&doc_id).await.unwrap()["reviewed"], true);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn metadata_only_refresh_receipt_recovers_and_invalid_patch_keeps_previous_state() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        pipeline.ingest_l1(&[L1FileEntry {
            doc_id: raw.source_hash.clone(), source_hash: raw.source_hash.clone(),
            location_uri: raw.location_uri.clone(), owner_id: raw.owner_id.clone(),
            filename: raw.filename, ext: raw.ext, parent_dir: "/".into(), size: 12,
            mtime_ms: 1000, ctime_ms: 1000, volume_id: None,
        }]).await.unwrap();
        let before = local.document_metadata(&raw.source_hash).await.unwrap();
        assert!(local.update_l2_fields(&raw.source_hash, Some("Invalid"), None,
            None, None, None, Some("[]")).await.is_err());
        assert_eq!(local.document_metadata(&raw.source_hash).await.unwrap(), before);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());

        let held_writer = fts.writer().unwrap();
        assert!(local.update_l2_fields(&raw.source_hash, Some("Metadata title"), None,
            None, None, None, Some(r#"{"reviewed":true}"#)).await.is_err());
        assert_eq!(pending_fts_writes(local.ingest_journal_path()).unwrap().len(), 1);
        drop(held_writer);
        pipeline.ensure_ready().await.unwrap();
        let metadata = local.document_metadata(&raw.source_hash).await.unwrap();
        assert_eq!(metadata["reviewed"], true);
        assert_eq!(metadata["fs_size"], 12);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn available_sources_are_filtered_before_limit_on_real_rows() {
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};
        let (_dir, _fts, local, pipeline) = test_indexes().await;
        for (hash, volume) in [("offline", Some("missing")), ("mounted", Some("drive'one")), ("unbound", None)] {
            let mut raw = sample_raw();
            raw.source_hash = hash.into();
            raw.location_uri = format!("/source/{hash}.txt");
            raw.volume_id = volume.map(str::to_owned);
            pipeline.ingest_document(raw).await.unwrap();
        }
        let filters = super::super::schema::SearchFilters {
            mounted_volume_ids: Some(vec!["drive'one".into()]), ..Default::default()
        };
        let predicate = filters.to_lance_sql().unwrap();
        assert_eq!(local.table_ref().count_rows(Some(predicate.clone())).await.unwrap(), 2);
        let batches: Vec<arrow_array::RecordBatch> = local.table_ref().query()
            .only_if(predicate).limit(1).execute().await.unwrap().try_collect().await.unwrap();
        let ids = batches[0].column_by_name("source_hash").unwrap().as_any().downcast_ref::<arrow_array::StringArray>().unwrap();
        assert_ne!(ids.value(0), "offline");
        let offline = super::super::schema::SearchFilters { mounted_volume_ids: Some(vec![]), ..Default::default() };
        assert_eq!(local.table_ref().count_rows(offline.to_lance_sql()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn representative_lookup_includes_metadata_rows_and_keeps_owner_scope() {
        let (_dir, _fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        pipeline.ingest_l1(&[L1FileEntry {
            doc_id: raw.source_hash.clone(), source_hash: raw.source_hash.clone(),
            location_uri: raw.location_uri.clone(), owner_id: raw.owner_id.clone(),
            filename: raw.filename.clone(), ext: raw.ext.clone(), parent_dir: "/".into(), size: 12,
            mtime_ms: 1000, ctime_ms: 1000, volume_id: None,
        }]).await.unwrap();
        let metadata = local.fetch_document_summary(&raw.source_hash).await.unwrap().unwrap();
        assert_eq!(metadata.chunk_index, -1);
        assert_eq!(metadata.location_uri, raw.location_uri);
        assert!(local.fetch_document_summary_for_uri(&raw.location_uri, "another owner").await.unwrap().is_none());
        pipeline.ingest_document(raw.clone()).await.unwrap();
        let full = local.fetch_document_summary_for_uri(&raw.location_uri, &raw.owner_id).await.unwrap().unwrap();
        assert_eq!(full.chunk_index, 0);
        assert_eq!(full.doc_id, raw.source_hash);
    }

    #[tokio::test]
    async fn legacy_user_skip_decisions_remain_explicitly_skipped() {
        let (_dir, _fts, local, _pipeline) = test_indexes().await;
        for (i, message) in ["skipped via GUI", "manually skipped via CLI"].iter().enumerate() {
            let mut raw = sample_raw();
            raw.source_hash = format!("legacy-{i}");
            raw.location_uri = format!("/legacy/{i}");
            let mut chunk = one_chunk(&raw);
            chunk.metadata_json = Some(serde_json::json!({"extraction_failure":{"reason":"unsupported","msg":message}}).to_string());
            local.ingest_batch(&[chunk]).await.unwrap();
            assert!(local.indexed_state_for_uri(&raw.location_uri).await.unwrap().unwrap().extraction_skip);
        }
    }


    #[tokio::test]
    async fn queued_file_version_is_reverified_under_the_writer_lease() {
        let (directory, fts, local, pipeline) = test_indexes().await;
        let path = directory.path().join("source.txt");
        std::fs::write(&path, b"first version").unwrap();
        let mut first = sample_raw();
        first.source_hash = hash_local_source(&path).unwrap();
        first.location_uri = path.to_string_lossy().into_owned();
        first.full_text = "Original verified passage".into();
        pipeline.ingest_verified_document(first.clone(), path.clone()).await.unwrap();

        let guard = local.read_guard(&fts).await.unwrap();
        let pipeline = Arc::new(pipeline);
        let queued = tokio::spawn({
            let pipeline = pipeline.clone(); let raw = first.clone(); let path = path.clone();
            async move { pipeline.ingest_verified_document(raw, path).await }
        });
        for _ in 0..100 {
            if pipeline.queue_depth() > 0 { break; }
            tokio::task::yield_now().await;
        }
        assert_eq!(pipeline.queue_depth(), 1);
        // Same byte count, changed after preparation and while the writer waits.
        std::fs::write(&path, b"other version").unwrap();
        drop(guard);
        assert!(queued.await.unwrap().is_err());
        assert_eq!(local.fetch_full_text(&doc_id_for(&first)).await.unwrap(), first.full_text);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());

        let mut latest = first.clone();
        latest.source_hash = hash_local_source(&path).unwrap();
        latest.full_text = "Current verified passage".into();
        pipeline.ingest_verified_document(latest.clone(), path.clone()).await.unwrap();
        assert!(pipeline.ingest_verified_document(first, path.clone()).await.is_err());
        assert_eq!(local.fetch_full_text(&doc_id_for(&latest)).await.unwrap(), latest.full_text);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert!(fts.search("original", &Default::default(), 10).unwrap().is_empty());
        std::fs::remove_file(path.clone()).unwrap();
        assert!(pipeline.ingest_verified_document(latest.clone(), path).await.is_err());
        assert_eq!(local.fetch_full_text(&doc_id_for(&latest)).await.unwrap(), latest.full_text);
    }

    #[tokio::test]
    async fn explicit_byte_receipt_supports_text_derived_document_identities() {
        let (directory, _fts, local, pipeline) = test_indexes().await;
        let path = directory.path().join("manual.txt");
        std::fs::write(&path, b"source bytes").unwrap();
        let raw = sample_raw();
        let fingerprint = hash_local_source(&path).unwrap();
        assert_ne!(raw.source_hash, fingerprint);
        pipeline.ingest_documents_batch_verified(vec![raw.clone()], vec![(path.clone(), fingerprint.clone())]).await.unwrap();
        assert_eq!(local.fetch_full_text(&doc_id_for(&raw)).await.unwrap(), raw.full_text);
        std::fs::write(&path, b"changed file").unwrap();
        let mut changed = raw.clone(); changed.full_text = "Must not replace the last verified body".into();
        assert!(pipeline.ingest_documents_batch_verified(vec![changed], vec![(path, fingerprint)]).await.is_err());
        assert_eq!(local.fetch_full_text(&doc_id_for(&raw)).await.unwrap(), raw.full_text);
    }

    #[tokio::test]
    async fn identical_content_keeps_each_owner_and_location_and_batch_result_order() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let mut first = sample_raw(); first.owner_id = "alice".into(); first.location_uri = "/same/source.txt".into();
        first.mtime_unix = Some(12);
        let mut second = first.clone(); second.location_uri = "/copy/source.txt".into();
        let mut other_owner = first.clone(); other_owner.owner_id = "bob".into();
        assert!(local.indexed_state_for_owner_uri(&first.location_uri, "bob").await.unwrap().is_none());
        let mut latest_first = first.clone(); latest_first.full_text = "Newest revision marker".into();
        let stats = pipeline.ingest_documents_batch(vec![first.clone(), second.clone(), other_owner.clone(), latest_first.clone()]).await.unwrap();
        assert_eq!(stats.doc_ids.len(), 4);
        assert_eq!(stats.doc_ids[0], stats.doc_ids[3]);
        assert_ne!(stats.doc_ids[0], stats.doc_ids[1]);
        assert_ne!(stats.doc_ids[0], stats.doc_ids[2]);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 3);
        assert_eq!(local.fetch_full_text(&stats.doc_ids[0]).await.unwrap(), latest_first.full_text);
        for raw in [&first, &second, &other_owner] {
            let summary = local.fetch_document_summary_for_uri(&raw.location_uri, &raw.owner_id).await.unwrap().unwrap();
            assert_eq!(summary.source_hash, raw.source_hash);
        }
        assert!(local.indexed_state_for_owner_uri(&first.location_uri, "charlie").await.unwrap().is_none());
        assert_eq!(local.indexed_state_for_owner_uri(&first.location_uri, "bob").await.unwrap().unwrap().source_hash.as_deref(), Some(first.source_hash.as_str()));
        let bob = super::super::schema::SearchFilters { owner_id: Some("bob".into()), ..Default::default() };
        assert!(fts.search("newest", &bob, 10).unwrap().is_empty());
        assert_eq!(fts.search("fox", &bob, 10).unwrap().len(), 1);
    }

    async fn seed_shared_legacy(local: &LocalIndex, fts: &FtsIndex, raws: &[RawDocument]) {
        let rows = raws.iter().enumerate().map(|(index, raw)| {
            let mut row = one_chunk(raw); row.doc_id = "shared-legacy".into(); row.id = chunk_row_id(&row.doc_id, 0);
            row.embedding = Some(vec![index as f32 + 1.0; 4]);
            row.metadata_json = Some(serde_json::json!({"user_note":format!("retained-{index}")}).to_string());
            row
        }).collect::<Vec<_>>();
        local.append_legacy_rows_for_test(&rows).await.unwrap();
        let mut writer = fts.writer().unwrap();
        for raw in raws {
            fts.add_document(&mut writer, TantivyInput { doc_id: "shared-legacy", owner_id: &raw.owner_id,
                language: &raw.language, title: raw.title.as_deref().unwrap_or(""), headings: "", body: &raw.full_text, body_translated: None }).unwrap();
        }
        writer.commit().unwrap();
    }

    fn collision_sources() -> Vec<RawDocument> {
        let mut first = sample_raw(); first.owner_id = "alice".into(); first.location_uri = "/shared/location.txt".into(); first.full_text = "selectedmarker".into();
        let mut other_owner = first.clone(); other_owner.owner_id = "bob".into(); other_owner.full_text = "bobsecretmarker".into();
        let mut other_path = first.clone(); other_path.location_uri = "/independent/location.txt".into(); other_path.full_text = "copysecretmarker".into();
        vec![first, other_owner, other_path]
    }

    #[tokio::test]
    async fn shared_legacy_refresh_recovers_each_untouched_occurrence_without_text_leakage() {
        use arrow_array::{Array, FixedSizeListArray, Float32Array};
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};
        let (dir, fts, local, pipeline) = test_indexes().await;
        let raws = collision_sources();
        seed_shared_legacy(&local, &fts, &raws).await;
        let mut refresh = raws[0].clone(); refresh.full_text = "refreshedmarker".into();
        let held_writer = fts.writer().unwrap();
        assert!(pipeline.ingest_document(refresh.clone()).await.is_err());
        assert_eq!(pending_fts_writes(local.ingest_journal_path()).unwrap().len(), 1);
        drop(held_writer);
        let reopened_local = Arc::new(LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
        let reopened = IngestPipeline::new(fts.clone(), reopened_local.clone(), None, IngestConfig { text_only: true, ..Default::default() });
        reopened.ensure_ready().await.unwrap();
        assert_eq!(reopened_local.table_ref().count_rows(None).await.unwrap(), 3);
        let summaries = futures::future::try_join_all(raws.iter().map(|raw| reopened_local.fetch_document_summary_for_uri(&raw.location_uri, &raw.owner_id))).await.unwrap();
        let ids = summaries.iter().map(|s| s.as_ref().unwrap().doc_id.clone()).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 3);
        assert_eq!(fts.doc_count(), 3);
        let alice = super::super::schema::SearchFilters { owner_id: Some("alice".into()), ..Default::default() };
        let bob = super::super::schema::SearchFilters { owner_id: Some("bob".into()), ..Default::default() };
        assert!(fts.search("bobsecretmarker", &alice, 10).unwrap().is_empty());
        assert!(fts.search("refreshedmarker", &bob, 10).unwrap().is_empty());
        assert!(fts.search("refreshedmarker PRE/5 copysecretmarker", &alice, 10).unwrap().is_empty());
        assert_eq!(fts.search("copysecretmarker", &alice, 10).unwrap().len(), 1);
        for index in [1, 2] {
            let id = &summaries[index].as_ref().unwrap().doc_id;
            assert_eq!(reopened_local.fetch_full_text(id).await.unwrap(), raws[index].full_text);
            assert_eq!(reopened_local.document_metadata(id).await.unwrap()["user_note"], format!("retained-{index}"));
            let batches: Vec<arrow_array::RecordBatch> = reopened_local.table_ref().query().only_if(format!("doc_id = '{id}'"))
                .execute().await.unwrap().try_collect().await.unwrap();
            let vectors = batches[0].column_by_name("embedding").unwrap().as_any().downcast_ref::<FixedSizeListArray>().unwrap();
            let vector = vectors.value(0); let vector = vector.as_any().downcast_ref::<Float32Array>().unwrap();
            assert_eq!(vector.values().as_ref(), &[index as f32 + 1.0; 4]);
        }
        let refreshed = reopened.ingest_document(refresh).await.unwrap();
        assert_eq!(refreshed.doc_ids[0], summaries[0].as_ref().unwrap().doc_id);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn scoped_delete_repairs_lexical_state_and_preserves_all_other_legacy_sources() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raws = collision_sources(); seed_shared_legacy(&local, &fts, &raws).await;
        assert!(local.delete_document_with_fts("shared-legacy", &fts).await.is_err());
        assert!(local.update_location("shared-legacy", "/wrong.txt").await.is_err());
        let held_writer = fts.writer().unwrap();
        assert!(local.delete_occurrence_with_fts("shared-legacy", &raws[0].owner_id, &raws[0].location_uri, &fts).await.is_err());
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 2);
        drop(held_writer);
        pipeline.ensure_ready().await.unwrap();
        assert!(local.fetch_document_summary_for_uri(&raws[0].location_uri, &raws[0].owner_id).await.unwrap().is_none());
        assert!(fts.search("selectedmarker", &Default::default(), 10).unwrap().is_empty());
        for raw in &raws[1..] {
            let summary = local.fetch_document_summary_for_uri(&raw.location_uri, &raw.owner_id).await.unwrap().unwrap();
            assert_eq!(local.fetch_full_text(&summary.doc_id).await.unwrap(), raw.full_text);
            assert_eq!(local.document_occurrences(&summary.doc_id).await.unwrap().len(), 1);
        }
        assert_eq!(fts.doc_count(), 2);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn move_refresh_retains_identity_reviews_and_frees_the_original_location() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let mut raw = sample_raw(); raw.full_text = "Vendor: Acme".into();
        let mut old = one_chunk(&raw); old.doc_id = "stable-legacy".into(); old.id = chunk_row_id(&old.doc_id, 0); old.doc_status = Some("reviewed".into());
        let mut fields = super::super::fields::from_extraction(&old.doc_id, &raw.full_text, "test-model", vec![("vendor".into(), "Acme".into(), 0.9)]);
        fields[0].review_status = "corrected".into(); fields[0].normalized_value = "Acme Limited".into(); fields[0].revision = 4;
        super::super::fields::attach(&mut old.metadata_json, &fields);
        local.ingest_batch_with_fts(&[old], &[], &fts).await.unwrap();
        let result = pipeline.ingest_document(raw.clone()).await.unwrap();
        assert_eq!(result.doc_ids, vec!["stable-legacy"]);
        let moved_uri = "/moved/reviewed.txt";
        local.update_location("stable-legacy", moved_uri).await.unwrap();
        let mut moved = raw.clone(); moved.location_uri = moved_uri.into();
        assert_eq!(pipeline.ingest_document(moved.clone()).await.unwrap().doc_ids, vec!["stable-legacy"]);
        let reviewed = super::super::fields::read(&local.document_metadata("stable-legacy").await.unwrap());
        assert_eq!(reviewed, fields);
        assert_eq!(local.fetch_document_summary("stable-legacy").await.unwrap().unwrap().doc_status.as_deref(), Some("reviewed"));
        let original = pipeline.ingest_document(raw.clone()).await.unwrap().doc_ids[0].clone();
        assert_ne!(original, "stable-legacy");
        // Move a canonical identity, then create a new occurrence at its old
        // location. The occupied canonical ID must never overwrite the move.
        local.update_location(&original, "/another/moved.txt").await.unwrap();
        let replacement = pipeline.ingest_document(raw.clone()).await.unwrap().doc_ids[0].clone();
        assert_ne!(replacement, original);
        assert_eq!(pipeline.ingest_document(raw).await.unwrap().doc_ids[0], replacement);
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 3);
        assert_eq!(local.document_occurrences(&original).await.unwrap()[0].1, "/another/moved.txt");
    }

    #[tokio::test]
    async fn batch_refresh_splits_every_selected_legacy_occurrence_once() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let mut raws = collision_sources(); seed_shared_legacy(&local, &fts, &raws).await;
        for (index, raw) in raws.iter_mut().enumerate() { raw.full_text = format!("batchrevision{index}"); }
        let result = pipeline.ingest_documents_batch(raws.clone()).await.unwrap();
        assert_eq!(result.doc_ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
        for (index, id) in result.doc_ids.iter().enumerate() { assert_eq!(local.fetch_full_text(id).await.unwrap(), raws[index].full_text); }
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 3);
        assert_eq!(fts.doc_count(), 3);
    }

    #[tokio::test]
    async fn ambiguous_same_source_import_identities_fail_before_mutation() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raw = sample_raw();
        let mut left = one_chunk(&raw); left.doc_id = "explicit-one".into(); left.id = chunk_row_id(&left.doc_id, 0);
        let mut right = left.clone(); right.doc_id = "explicit-two".into(); right.id = chunk_row_id(&right.doc_id, 0);
        local.ingest_batch_with_fts(&[left, right], &[], &fts).await.unwrap();
        assert_eq!(local.document_ids_for_source(&raw.owner_id, &raw.location_uri).await.unwrap(), vec!["explicit-one", "explicit-two"]);
        assert!(pipeline.ingest_document(raw).await.unwrap_err().to_string().contains("Multiple stored identities"));
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 2);
        assert_eq!(fts.doc_count(), 2);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_refresh_metadata_does_not_cross_legacy_owner_or_location() {
        let (_dir, fts, local, pipeline) = test_indexes().await;
        let raws = collision_sources(); seed_shared_legacy(&local, &fts, &raws).await;
        let raw = &raws[0];
        pipeline.ingest_l2_row("shared-legacy".into(), raw.location_uri.clone(), raw.owner_id.clone(),
            raw.filename.clone(), raw.ext.clone(), "failed-new-bytes".into(), Some(123), Some(100), None, None,
            None, None, None, None, None, &super::super::task_failure::TaskFailureReason::EmptyText, "No text").await.unwrap();
        assert!(local.indexed_state_for_owner_uri(&raw.location_uri, &raw.owner_id).await.unwrap().unwrap().extraction_failed);
        for other in &raws[1..] {
            assert!(!local.indexed_state_for_owner_uri(&other.location_uri, &other.owner_id).await.unwrap().unwrap().extraction_failed);
        }
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn selected_source_receipt_rejects_a_new_canonical_sibling_at_the_writer_gate() {
        let (dir, fts, local, pipeline) = test_indexes().await;
        let path = dir.path().join("selected.txt"); std::fs::write(&path, b"unchanged source bytes").unwrap();
        let mut raw = sample_raw(); raw.source_hash = hash_local_source(&path).unwrap(); raw.location_uri = path.to_string_lossy().into_owned();
        let mut legacy = one_chunk(&raw); legacy.doc_id = "selected-legacy".into(); legacy.id = chunk_row_id(&legacy.doc_id, 0);
        local.ingest_batch_with_fts(&[legacy], &[], &fts).await.unwrap();
        assert_eq!(pipeline.ingest_verified_document_for_occurrence(raw.clone(), path.clone(), "selected-legacy".into()).await.unwrap().doc_ids, vec!["selected-legacy"]);
        let pipeline = Arc::new(pipeline);
        let guard = local.read_guard(&fts).await.unwrap();
        let queued = tokio::spawn({ let pipeline = pipeline.clone(); let raw = raw.clone(); let path = path.clone();
            async move { pipeline.ingest_verified_document_for_occurrence(raw, path, "selected-legacy".into()).await } });
        for _ in 0..100 { if pipeline.queue_depth() > 0 { break; } tokio::task::yield_now().await; }
        assert_eq!(pipeline.queue_depth(), 1);
        // A separate import completes while this extraction waits for the
        // lease. Exercise the same atomic import primitive under our lease.
        let mut canonical = one_chunk(&raw); canonical.full_text = Some("independent imported marker".into());
        local.replace_imported_rows(&mut [canonical.clone()], &[], "imported-while-queued").await.unwrap();
        let pending = PendingFtsWrite { generation: "fixture".into(), doc_ids: vec![canonical.doc_id.clone()], replace_ids: vec![canonical.doc_id.clone()],
            inputs: vec![TantivyInputOwned { doc_id: canonical.doc_id.clone(), owner_id: raw.owner_id.clone(), language: raw.language.clone(),
                title: String::new(), headings: String::new(), body: canonical.full_text.clone().unwrap(), body_translated: None }],
            receipt_rows: Vec::new(), delete_only: false, absent_occurrences: Vec::new() };
        apply_fts_write(&fts, &pending).unwrap();
        drop(guard);
        let error = queued.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("selected source identity changed"), "{error}");
        assert_eq!(local.fetch_full_text("selected-legacy").await.unwrap(), raw.full_text);
        assert_eq!(local.fetch_full_text(&canonical.doc_id).await.unwrap(), canonical.full_text.unwrap());
        assert_eq!(fts.doc_count(), 2);
        assert!(pending_fts_writes(local.ingest_journal_path()).unwrap().is_empty());
    }

}
