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

use super::embedder::{chunk_text, Embedder};
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
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            chunk_max_words: 1500,
            chunk_stride: 200,
            batch_size: 32,
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
}

// ── IngestStats ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestStats {
    pub chunk_count: usize,
    pub embed_time_ms: u64,
    pub write_time_ms: u64,
}

// ── Writer task types ────────────────────────────────────────────────────────

/// Owned Tantivy inputs that can be sent across the channel boundary.
struct TantivyInputOwned {
    doc_id: String,
    owner_id: String,
    language: String,
    title: String,
    headings: String,
    body: String,
    /// MT-pass output replicated for the FTS write — `None` when the
    /// extractor didn't translate (no `--translate-to`, source lang
    /// unknown, or MT failed).  Wired into the `body_translated`
    /// Tantivy field when the on-disk schema has it.
    body_translated: Option<String>,
}

/// One unit of work for the background writer task.
struct WriterJob {
    all_chunks: Vec<DocumentChunk>,
    /// Empty for L1 writes (no full-text in Tantivy for metadata-only rows).
    tantivy_inputs: Vec<TantivyInputOwned>,
    total_chunk_count: usize,
    embed_time_ms: u64,
    reply: tokio::sync::oneshot::Sender<Result<IngestStats>>,
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
        let lance_batch_size = config.batch_size.saturating_mul(4).max(64);

        tokio::spawn(async move {
            while let Some(job) = writer_rx.recv().await {
                let result: Result<IngestStats> = (async {
                    let write_start = Instant::now();

                    // LanceDB: batch inserts.
                    for batch in job.all_chunks.chunks(lance_batch_size) {
                        vector_w
                            .ingest_batch(batch)
                            .await
                            .context("LanceDB write")?;
                    }

                    // Tantivy: one commit for all docs in this job.
                    // Skipped for L1 jobs where tantivy_inputs is empty.
                    if !job.tantivy_inputs.is_empty() {
                        let mut writer =
                            fts_w.writer().context("opening Tantivy writer")?;
                        for input in &job.tantivy_inputs {
                            fts_w.add_document(
                                &mut writer,
                                TantivyInput {
                                    doc_id: &input.doc_id,
                                    owner_id: &input.owner_id,
                                    language: &input.language,
                                    title: &input.title,
                                    headings: &input.headings,
                                    body: &input.body,
                                    body_translated: input.body_translated.as_deref(),
                                },
                            )?;
                        }
                        writer.commit().context("Tantivy commit")?;
                    }

                    let write_time_ms = write_start.elapsed().as_millis() as u64;
                    Ok(IngestStats {
                        chunk_count: job.total_chunk_count,
                        embed_time_ms: job.embed_time_ms,
                        write_time_ms,
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

    /// Enqueue a write job and block until the writer task completes it.
    async fn submit_and_await(
        &self,
        all_chunks: Vec<DocumentChunk>,
        tantivy_inputs: Vec<TantivyInputOwned>,
        total_chunk_count: usize,
        embed_time_ms: u64,
    ) -> Result<IngestStats> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.pending.fetch_add(1, Ordering::Relaxed);
        if let Err(_e) = self
            .writer_tx
            .send(WriterJob {
                all_chunks,
                tantivy_inputs,
                total_chunk_count,
                embed_time_ms,
                reply: reply_tx,
            })
            .await
        {
            // Roll back the optimistic queue-depth increment when the writer
            // task is already gone; otherwise the UI can get stuck showing a
            // phantom pending write until the next re-init.
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return Err(anyhow::anyhow!(
                "Writer task has stopped — index may need re-init"
            ));
        }
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

    /// Bulk ingest of N documents with coalesced LanceDB writes + one Tantivy commit.
    ///
    /// Embedding runs inline on the caller's task (GPU/CPU-bound).
    /// The resulting chunks are submitted to the background writer task, which
    /// serialises all LanceDB + Tantivy mutations so concurrent callers never
    /// race on the indexes. Callers await the result via a oneshot channel.
    pub async fn ingest_documents_batch(&self, mut raws: Vec<RawDocument>) -> Result<IngestStats> {
        if raws.is_empty() {
            return Ok(IngestStats { chunk_count: 0, embed_time_ms: 0, write_time_ms: 0 });
        }

        // ── P19 NER phase ───────────────────────────────────────────────
        // Run GLiNER once per document on the (truncated) full_text and merge
        // the resulting entity tags into raw.tags BEFORE chunk rows are built,
        // so every chunk of a doc carries the same entity tags (chunk_index
        // convention).  No-op when NER is disabled or the feature is off.
        if let Some(ner) = &self.ner {
            for raw in raws.iter_mut() {
                let entity_tags = ner.extract_tags(&raw.full_text).await;
                if !entity_tags.is_empty() {
                    merge_tags(&mut raw.tags, entity_tags);
                }
            }
        }

        let embedder = self.embedder.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Embedding (L3) is disabled. Switch the index config to \
                 `use_vector = true` (Settings → Search Index → \
                 Vektor-Embeddings verwenden) and re-init."
            )
        })?;

        let mut all_chunks: Vec<DocumentChunk> = Vec::new();
        let mut tantivy_inputs: Vec<TantivyInputOwned> = Vec::with_capacity(raws.len());
        let mut total_chunk_count: usize = 0;

        // ── Embedding phase ─────────────────────────────────────────────
        let embed_start = Instant::now();
        for raw in &raws {
            let chunks = chunk_text(
                &raw.full_text,
                self.config.chunk_max_words,
                self.config.chunk_stride,
                &[],
            );
            let chunk_total = chunks.len() as i32;
            total_chunk_count += chunks.len();

            for batch in chunks.chunks(self.config.batch_size) {
                let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
                let (dense, sparse, multivecs) = {
                    use super::embedder::EmbedRole;
                    let mut emb = embedder.lock().await;
                    let (dense, sparse) = emb.embed_full(texts.clone(), EmbedRole::Passage)?;
                    // Stage AD: ColBERT multi-vector encoding (BGE-M3 only).
                    let multivecs = if emb.has_colbert() {
                        emb.embed_multivec(texts)?
                    } else {
                        vec![vec![]; batch.len()]
                    };
                    (dense, sparse, multivecs)
                };
                let model_id = {
                    let emb = embedder.lock().await;
                    format!("{:?}", emb.model())
                };
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
        let embed_time_ms = embed_start.elapsed().as_millis() as u64;

        // ── Write phase: submit to background writer task ───────────────
        self.submit_and_await(all_chunks, tantivy_inputs, total_chunk_count, embed_time_ms)
            .await
    }

    /// Re-ingest: delete all existing chunks for a document, then ingest fresh.
    pub async fn reingest_document(&self, raw: RawDocument) -> Result<IngestStats> {
        let doc_id = doc_id_for(&raw);

        // Remove old data.
        self.vector.delete_doc(&doc_id).await?;
        {
            let mut writer = self.fts.writer()?;
            self.fts.delete_document(&mut writer, &doc_id)?;
            writer.commit()?;
        }

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
                }
            })
            .collect();

        let total = chunks.len();
        // L1 has no Tantivy entries (no full-text body yet).
        self.submit_and_await(chunks, vec![], total, 0).await
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
                "msg": &failure_msg[..failure_msg.len().min(512)],
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
        };

        self.submit_and_await(vec![chunk], vec![], 1, 0).await
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

/// Derive a stable doc_id from the source_hash.
/// (The source_hash is SHA-256 of the original file bytes, supplied by the caller.)
pub fn doc_id_for(raw: &RawDocument) -> String {
    raw.source_hash.clone()
}

/// P19 — merge NER entity tags into a document's existing tags, deduping
/// case-insensitively while preserving order (existing tags first, then new
/// entity tags in score order). Keeps the first-seen casing.
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
        embedding_model: Some(model_id),
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
        metadata_json: build_metadata_json(raw.mtime_unix, raw.file_size, raw.volume_id.as_deref()),
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
    }
}

fn build_metadata_json(
    mtime_unix: Option<i64>,
    file_size:  Option<i64>,
    volume_id:  Option<&str>,
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
        }
    }

    #[test]
    fn doc_id_is_source_hash() {
        let raw = sample_raw();
        assert_eq!(doc_id_for(&raw), "abc123def456");
    }

    #[test]
    fn chunk_row_id_is_deterministic() {
        assert_eq!(chunk_row_id("doc1", 0), chunk_row_id("doc1", 0));
        assert_ne!(chunk_row_id("doc1", 0), chunk_row_id("doc1", 1));
    }

    #[test]
    fn metadata_json_packs_both_fields() {
        assert_eq!(build_metadata_json(None, None, None), None);
        assert_eq!(
            build_metadata_json(Some(1700000000), None, None).as_deref(),
            Some(r#"{"mtime_unix":1700000000,"fs_mtime":1700000000000}"#)
        );
        assert_eq!(
            build_metadata_json(None, Some(12345), None).as_deref(),
            Some(r#"{"fs_size":12345}"#)
        );
        assert_eq!(
            build_metadata_json(None, None, Some("ABCD-1234")).as_deref(),
            Some(r#"{"volume_id":"ABCD-1234"}"#)
        );
        assert_eq!(
            build_metadata_json(Some(42), Some(999), Some("ABCD-1234")).as_deref(),
            Some(r#"{"mtime_unix":42,"fs_mtime":42000,"fs_size":999,"volume_id":"ABCD-1234"}"#)
        );
    }

    #[test]
    fn metadata_json_keeps_mtime_parser_compatible() {
        // The hand-parser in LocalIndex::indexed_mtime_for_uri reads
        // digits after `"mtime_unix":` up to the next non-digit. Adding
        // fs_mtime and other keys after must not break that contract.
        let s = build_metadata_json(Some(1700000000), Some(0), Some("ABCD")).unwrap();
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
        assert_eq!(dc.doc_id, "abc123def456");
        assert_eq!(dc.chunk_index, 0);
        assert_eq!(dc.chunk_total, 2);
        assert_eq!(dc.language, Some("en".to_owned()));
        assert_eq!(dc.tags, vec!["theology".to_owned()]);
        assert!(dc.embedding.is_some());
    }

    #[test]
    fn doc_id_distinguishes_two_raws_by_source_hash() {
        // P11 step 1 sanity: when ingest_documents_batch dispatches N
        // RawDocuments through the embed loop, each must produce a
        // distinct doc_id even when filename / location / owner are
        // identical -- that's the only thing keeping a re-ingest of
        // the same path under different content from collapsing into
        // one row in LanceDB.
        let mut raw_a = sample_raw();
        let mut raw_b = sample_raw();
        raw_a.source_hash = "aaaaaaaa".into();
        raw_b.source_hash = "bbbbbbbb".into();
        assert_ne!(doc_id_for(&raw_a), doc_id_for(&raw_b));
        // Same source_hash on two RawDocuments -> same doc_id (the
        // dedup contract every ingest path relies on).
        let raw_c = sample_raw();
        let raw_d = sample_raw();
        assert_eq!(doc_id_for(&raw_c), doc_id_for(&raw_d));
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
}
