/// Ingest pipeline: text extraction output → chunks → embeddings → indexes.
///
/// `IngestPipeline` owns a shared `FtsIndex`, `LocalIndex`, and `Embedder`.
/// `ingest_document` is the single entry point: given a `RawDocument` with
/// already-extracted text, it chunks, embeds, and writes to both indexes.
use std::sync::Arc;
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
    pub mtime_unix: Option<u32>,

    /// Stable id of the volume the source file lives on (PLAN P7.6).
    /// Populated by the `volume::volume_id_for_path` helper at ingest
    /// time. Stored alongside `mtime_unix` in `metadata_json` so a
    /// future search-time filter can hide rows from currently-unmounted
    /// volumes without a schema migration. `None` for non-file ingests
    /// or when the platform helper fails (best-effort enrichment).
    pub volume_id: Option<String>,
}

// ── IngestStats ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestStats {
    pub chunk_count: usize,
    pub embed_time_ms: u64,
    pub write_time_ms: u64,
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
}

impl IngestPipeline {
    pub fn new(
        fts: Arc<FtsIndex>,
        vector: Arc<LocalIndex>,
        embedder: Option<Arc<Mutex<Embedder>>>,
        config: IngestConfig,
    ) -> Self {
        IngestPipeline {
            fts,
            vector,
            embedder,
            config,
        }
    }

    /// Full ingest pipeline for one document.
    ///
    /// Steps:
    /// 1. Split `raw.full_text` into overlapping `TextChunk`s.
    /// 2. For each batch of chunks, embed dense vectors.
    /// 3. Write batch to LanceDB.
    /// 4. Write a single whole-document row to Tantivy (doc_id, owner_id, headings, body).
    /// 5. Return timing stats.
    pub async fn ingest_document(&self, raw: RawDocument) -> Result<IngestStats> {
        // L3 = Volltext + Embedding. Without an embedder we can't satisfy
        // the contract. The frontend should never reach this from an L1 / L2
        // code path; if it does, surface the misconfiguration clearly.
        let embedder = self.embedder.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Embedding (L3) is disabled. Switch the index config to \
                 `use_vector = true` (Settings → Search Index → \
                 Vektor-Embeddings verwenden) and re-init."
            )
        })?;

        let chunks = chunk_text(
            &raw.full_text,
            self.config.chunk_max_words,
            self.config.chunk_stride,
            &[],
        );

        let chunk_count = chunks.len();
        let chunk_total = chunk_count as i32;

        // ── Embedding phase ──────────────────────────────────────────────
        let embed_start = Instant::now();
        let mut all_doc_chunks: Vec<DocumentChunk> = Vec::with_capacity(chunk_count);

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

                let doc_chunk = build_doc_chunk(
                    text_chunk,
                    &raw,
                    chunk_total,
                    embedding,
                    sparse_json,
                    model_id.clone(),
                );
                all_doc_chunks.push(doc_chunk);
            }
        }
        let embed_time_ms = embed_start.elapsed().as_millis() as u64;

        // ── Write phase ──────────────────────────────────────────────────
        let write_start = Instant::now();

        // LanceDB: batch-insert all chunks at once (or in sub-batches).
        for batch in all_doc_chunks.chunks(self.config.batch_size) {
            self.vector
                .ingest_batch(batch)
                .await
                .context("LanceDB write")?;
        }

        // Tantivy: one whole-document row.
        let headings_joined = raw.headings.join(" ");
        let doc_id = doc_id_for(&raw);
        {
            let mut writer = self.fts.writer().context("opening Tantivy writer")?;
            self.fts.add_document(
                &mut writer,
                TantivyInput {
                    doc_id: &doc_id,
                    owner_id: &raw.owner_id,
                    language: &raw.language,
                    title: raw.title.as_deref().unwrap_or(""),
                    headings: &headings_joined,
                    body: &raw.full_text,
                },
            )?;
            writer.commit().context("Tantivy commit")?;
        }

        let write_time_ms = write_start.elapsed().as_millis() as u64;

        Ok(IngestStats {
            chunk_count,
            embed_time_ms,
            write_time_ms,
        })
    }

    /// PLAN P11 step 1 -- bulk ingest of N documents with coalesced
    /// LanceDB writes + a single Tantivy commit.
    ///
    /// `ingest_document` is fine for the foreground "user just dragged
    /// in a file" case where each doc has its own UI feedback. For
    /// programmatic ingest paths (the cb-manifest L1 import in
    /// P12 step 13, the producer/consumer pipeline once it grows
    /// chunked dispatching, the future POST /v1/ingest/batch path on
    /// the server) we want N docs to share *one* Arrow record batch
    /// to LanceDB and *one* Tantivy commit; per-doc the
    /// commit overhead is dominant for small chunks.
    ///
    /// Behaviour mirrors `ingest_document` exactly per-doc -- text
    /// chunking, embedding via `Embedder::embed_full(...,
    /// EmbedRole::Passage)`, doc_id derivation, sparse + model_id
    /// metadata. Errors abort the whole batch (no partial writes);
    /// callers that want per-doc error tolerance should split their
    /// input themselves.
    ///
    /// Returns aggregated stats across the whole batch:
    ///   chunk_count   = sum of chunks across all docs
    ///   embed_time_ms = wall-clock for all embedding work
    ///   write_time_ms = wall-clock for the LanceDB + Tantivy phase
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

        // Owned view of what Tantivy needs per doc, so we can write
        // them in one commit at the end without juggling borrows
        // across the embed loop.
        struct TantivyInputOwned {
            doc_id: String,
            owner_id: String,
            language: String,
            title: String,
            headings: String,
            body: String,
        }

        let mut all_chunks: Vec<DocumentChunk> = Vec::new();
        let mut tantivy_inputs: Vec<TantivyInputOwned> = Vec::with_capacity(raws.len());
        let mut total_chunk_count: usize = 0;

        // ── Embedding phase (per-doc serial; the per-batch embed loop
        //    inside is what the embedder backend can parallelise). ──
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

        // ── Coalesced write phase ───────────────────────────────────
        let write_start = Instant::now();

        // LanceDB: one Arrow record batch per `batch_size * 4` chunks.
        // The 4× factor amortises the Arrow encoding overhead more
        // aggressively than the per-doc default (which is sized for
        // streaming ingest, not bulk).
        let lance_batch_size = self.config.batch_size.saturating_mul(4).max(64);
        for batch in all_chunks.chunks(lance_batch_size) {
            self.vector
                .ingest_batch(batch)
                .await
                .context("LanceDB batch write")?;
        }

        // Tantivy: ONE commit with N add_document calls. Tantivy commits
        // are the expensive part (segment merges); a single commit for
        // the whole batch is the main throughput win.
        {
            let mut writer = self.fts.writer().context("opening Tantivy writer")?;
            for input in &tantivy_inputs {
                self.fts.add_document(
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
            writer.commit().context("Tantivy batch commit")?;
        }
        let write_time_ms = write_start.elapsed().as_millis() as u64;

        Ok(IngestStats {
            chunk_count: total_chunk_count,
            embed_time_ms,
            write_time_ms,
        })
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
    /// No text extraction, no embedding. The row uses `chunk_index = -1` and
    /// stashes filesystem metadata + the analysis level in `metadata_json`.
    /// Subsequent L2/L3 runs upgrade the same row by `doc_id` (via re-ingest).
    pub async fn ingest_l1(&self, files: &[L1FileEntry]) -> Result<IngestStats> {
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut chunks: Vec<DocumentChunk> = Vec::with_capacity(files.len());
        for f in files {
            let meta = serde_json::json!({
                "level":      1,
                "fs_size":    f.size,
                "fs_mtime":   f.mtime_ms,
                "fs_ctime":   f.ctime_ms,
                "parent_dir": f.parent_dir,
            });
            let doc_id = f.doc_id.clone();
            chunks.push(DocumentChunk {
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
            });
        }

        let write_start = Instant::now();
        for batch in chunks.chunks(self.config.batch_size) {
            self.vector
                .ingest_batch(batch)
                .await
                .context("LanceDB write (L1)")?;
        }
        let write_time_ms = write_start.elapsed().as_millis() as u64;

        Ok(IngestStats {
            chunk_count: chunks.len(),
            embed_time_ms: 0,
            write_time_ms,
        })
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
    }
}

fn build_metadata_json(mtime_unix: Option<u32>, volume_id: Option<&str>) -> Option<String> {
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
