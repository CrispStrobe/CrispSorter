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
            writer_tx,
            pending,
        }
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
    pub async fn ingest_documents_batch(&self, raws: Vec<RawDocument>) -> Result<IngestStats> {
        if raws.is_empty() {
            return Ok(IngestStats { chunk_count: 0, embed_time_ms: 0, write_time_ms: 0 });
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
                let (dense, sparse) = {
                    use super::embedder::EmbedRole;
                    let mut emb = embedder.lock().await;
                    emb.embed_full(texts, EmbedRole::Passage)?
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
                    all_chunks.push(build_doc_chunk(
                        text_chunk,
                        raw,
                        chunk_total,
                        embedding,
                        sparse_json,
                        model_id.clone(),
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
                }
            })
            .collect();

        let total = chunks.len();
        // L1 has no Tantivy entries (no full-text body yet).
        self.submit_and_await(chunks, vec![], total, 0).await
    }
}

/// One file's filesystem-only metadata for the L1 ingest path.
///
/// Frontend sends camelCase (Tauri convention) so the deserialisation
/// needs the rename: `docId` -> `doc_id`, `mtimeMs` -> `mtime_ms`, etc.
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
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Derive a stable doc_id from the source_hash.
/// (The source_hash is SHA-256 of the original file bytes, supplied by the caller.)
pub fn doc_id_for(raw: &RawDocument) -> String {
    raw.source_hash.clone()
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

/// Convert a `TextChunk` + `RawDocument` metadata + embedding into a `DocumentChunk`.
fn build_doc_chunk(
    tc: &super::embedder::TextChunk,
    raw: &RawDocument,
    chunk_total: i32,
    embedding: Option<Vec<f32>>,
    sparse_json: Option<String>,
    model_id: String,
) -> DocumentChunk {
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
        metadata_json: build_metadata_json(raw.mtime_unix, raw.volume_id.as_deref()),
        parent_dir: raw.parent_dir.clone(),
    }
}

fn build_metadata_json(mtime_unix: Option<i64>, volume_id: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = mtime_unix {
        parts.push(format!(r#""mtime_unix":{m}"#));
    }
    if let Some(v) = volume_id {
        // Volume ids are UUIDs / hex serials in practice (no quotes,
        // no backslashes), but escape defensively in case a future
        // platform helper returns something exotic.
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
            volume_id: None,
            parent_dir: None,
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
        assert_eq!(build_metadata_json(None, None), None);
        assert_eq!(
            build_metadata_json(Some(1700000000), None).as_deref(),
            Some(r#"{"mtime_unix":1700000000}"#)
        );
        assert_eq!(
            build_metadata_json(None, Some("ABCD-1234")).as_deref(),
            Some(r#"{"volume_id":"ABCD-1234"}"#)
        );
        assert_eq!(
            build_metadata_json(Some(42), Some("ABCD-1234")).as_deref(),
            Some(r#"{"mtime_unix":42,"volume_id":"ABCD-1234"}"#)
        );
    }

    #[test]
    fn metadata_json_keeps_mtime_parser_compatible() {
        // The hand-parser in LocalIndex::indexed_mtime_for_uri reads
        // digits after `"mtime_unix":` up to the next non-digit. Adding
        // a second key after must not break that contract.
        let s = build_metadata_json(Some(1700000000), Some("ABCD")).unwrap();
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
        let dc = build_doc_chunk(&tc, &raw, 2, Some(vec![0.1; 4]), None, "bge-m3".to_owned());
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
}
