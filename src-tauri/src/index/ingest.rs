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
    pub embedder: Arc<Mutex<Embedder>>,
    pub config: IngestConfig,
}

impl IngestPipeline {
    pub fn new(
        fts: Arc<FtsIndex>,
        vector: Arc<LocalIndex>,
        embedder: Arc<Mutex<Embedder>>,
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
                let mut emb = self.embedder.lock().await;
                emb.embed_full(texts)?
            };

            let model_id = {
                let emb = self.embedder.lock().await;
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
#[derive(Debug, Clone, serde::Deserialize)]
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
        metadata_json: None,
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
}
