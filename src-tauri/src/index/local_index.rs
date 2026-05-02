/// LanceDB local backend implementing `IndexBackend`.
///
/// One LanceDB table `documents` holds all indexed chunks.
/// Schema is defined in `schema.rs` (see `build_schema`).
///
/// Arrow / RecordBatch construction uses `arrow_array` types directly so that
/// column order exactly matches the schema returned by `build_schema`.
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use arrow_array::{
    builder::{ListBuilder, StringBuilder},
    Float32Array,
};
use arrow_array::{
    Array, FixedSizeListArray, Int32Array, RecordBatch, StringArray, TimestampMillisecondArray,
};
use arrow_schema::Schema;
use async_trait::async_trait;
use futures_util::TryStreamExt;
use lancedb::{
    connect,
    index::{vector::IvfPqIndexBuilder, Index},
    query::{ExecutableQuery, QueryBase},
    Connection, DistanceType, Table,
};

use super::embedder::SparseVector;
use super::schema::{build_schema, DocumentChunk, SearchFilters, SearchResult};
use super::IndexBackend;

// ── Constant ───────────────────────────────────────────────────────────────

const TABLE_NAME: &str = "documents";

// ── Struct ─────────────────────────────────────────────────────────────────

pub struct LocalIndex {
    // Kept alive to maintain the LanceDB connection for the table lifetime.
    _db: Connection,
    table: Table,
    pub dims: usize,
}

// ── Constructor ────────────────────────────────────────────────────────────

impl LocalIndex {
    /// Open the LanceDB table, creating it (empty) if it does not yet exist.
    pub async fn open_or_create(data_dir: &Path, dims: usize) -> Result<Self> {
        let lance_dir = data_dir.join("lance");
        std::fs::create_dir_all(&lance_dir)
            .with_context(|| format!("creating LanceDB dir {}", lance_dir.display()))?;

        let uri = lance_dir.to_string_lossy().into_owned();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to LanceDB at {uri}"))?;

        let table = match db.open_table(TABLE_NAME).execute().await {
            Ok(t) => t,
            Err(e) if is_table_not_found(&e) => {
                let schema = build_schema(dims);
                db.create_empty_table(TABLE_NAME, schema)
                    .execute()
                    .await
                    .context("creating empty LanceDB table")?
            }
            Err(e) => return Err(e).context("opening LanceDB table"),
        };

        Ok(LocalIndex {
            _db: db,
            table,
            dims,
        })
    }

    // ── Ingest ─────────────────────────────────────────────────────────────

    /// Ingest a single chunk. Prefer `ingest_batch` for throughput.
    pub async fn ingest_chunk(&self, chunk: &DocumentChunk) -> Result<()> {
        self.ingest_batch(std::slice::from_ref(chunk)).await
    }

    /// Batch-insert a slice of chunks into LanceDB.
    pub async fn ingest_batch(&self, chunks: &[DocumentChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let schema = build_schema(self.dims);
        let batch = chunks_to_record_batch(chunks, self.dims, &schema)?;

        let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.table
            .add(reader)
            .execute()
            .await
            .context("LanceDB add")?;
        Ok(())
    }

    // ── Search ─────────────────────────────────────────────────────────────

    /// ANN vector search.
    pub async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let mut vq = self
            .table
            .vector_search(embedding)?
            .distance_type(DistanceType::Cosine)
            .limit(limit);

        if let Some(sql) = filters.to_lance_sql() {
            vq = vq.only_if(sql);
        }

        let batches: Vec<RecordBatch> = vq.execute().await?.try_collect().await?;
        record_batches_to_search_results(&batches)
    }

    /// Fetch the best-matching chunk per doc_id for FTS result display.
    ///
    /// Fetches up to `chunks_per_doc` chunks for each doc_id and selects the
    /// chunk whose `full_text` contains the most occurrences of `query_terms`.
    /// Falls back to `chunk_index = 0` when no terms are given.
    pub async fn fetch_best_chunk_per_doc(
        &self,
        doc_ids: &[String],
        query_terms: &[&str],
        score_map: &std::collections::HashMap<String, f32>,
    ) -> Result<Vec<SearchResult>> {
        if doc_ids.is_empty() {
            return Ok(vec![]);
        }

        let quoted: Vec<String> = doc_ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();

        let (filter, per_doc) = if query_terms.is_empty() {
            // No terms to score → just grab chunk_index=0
            (
                format!("doc_id IN ({}) AND chunk_index = 0", quoted.join(", ")),
                1usize,
            )
        } else {
            // Fetch several chunks so we can pick the best one
            (format!("doc_id IN ({})", quoted.join(", ")), 8usize)
        };

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .limit(doc_ids.len() * per_doc)
            .execute()
            .await?
            .try_collect()
            .await?;

        let all = batches_to_search_results_with_scores(&batches, score_map)?;

        if query_terms.is_empty() {
            return Ok(all);
        }

        // For each doc_id keep the chunk with the highest query-term hit count.
        let mut best: std::collections::HashMap<String, (SearchResult, usize)> =
            std::collections::HashMap::new();

        for result in all {
            let text_lower = result.snippet.to_lowercase();
            let hits = query_terms
                .iter()
                .filter(|&&t| text_lower.contains(t))
                .count();

            let is_better = match best.get(&result.doc_id) {
                None => true,
                Some((_, prev)) => hits > *prev,
            };
            if is_better {
                best.insert(result.doc_id.clone(), (result, hits));
            }
        }

        Ok(best.into_values().map(|(r, _)| r).collect())
    }

    /// Score a candidate doc-id pool by sparse dot product.
    ///
    /// LanceDB has no native sparse vector index, so this scans the full
    /// candidate set rather than the corpus — but it's only invoked from
    /// `SearchEngine::search_hybrid` where the pool is the union of FTS+ANN
    /// hits (typically <200 docs). For larger corpora, sparse retrieval as
    /// the *primary* modality would need a dedicated inverted index; this
    /// implementation is intentionally a third RRF channel that refines an
    /// already-scoped candidate set.
    ///
    /// Returns one `SearchResult` per matching doc_id, scored by sparse
    /// dot product (higher = better), best chunk per doc kept.
    pub async fn search_sparse_in_pool(
        &self,
        query: &SparseVector,
        candidate_doc_ids: &[String],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if candidate_doc_ids.is_empty() || query.indices.is_empty() {
            return Ok(vec![]);
        }
        let quoted: Vec<String> = candidate_doc_ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        let mut filter = format!("doc_id IN ({})", quoted.join(", "));
        if let Some(extra) = filters.to_lance_sql() {
            filter = format!("({}) AND ({})", filter, extra);
        }
        // 8 chunks per doc lets us pick the best per-doc chunk without
        // scanning the full doc.
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .limit(candidate_doc_ids.len() * 8)
            .execute()
            .await?
            .try_collect()
            .await?;

        // Walk each row, deserialize sparse JSON, score against query.
        let mut best: std::collections::HashMap<String, (SearchResult, f32)> =
            std::collections::HashMap::new();

        for batch in &batches {
            let n = batch.num_rows();
            let sparse_col = str_col_opt(batch, "embedding_sparse");
            // If the column is missing entirely, the corpus was indexed
            // without sparse — bail out gracefully.
            let Some(sparse_col) = sparse_col else {
                return Ok(vec![]);
            };

            let doc_id_col = str_col(batch, "doc_id")?;
            let location_uri_col = str_col(batch, "location_uri")?;
            let owner_id_col = str_col(batch, "owner_id")?;
            let title_col = str_col_opt(batch, "title");
            let author_col = str_col_opt(batch, "author");
            let year_col = i32_col_opt(batch, "year");
            let filename_col = str_col_opt(batch, "filename");
            let ext_col = str_col_opt(batch, "ext");
            let language_col = str_col_opt(batch, "language");
            let chunk_idx_col = i32_col(batch, "chunk_index")?;
            let full_text_col = str_col_opt(batch, "full_text");

            for i in 0..n {
                if sparse_col.is_null(i) {
                    continue;
                }
                let json_str = sparse_col.value(i);
                let Ok(parsed): std::result::Result<serde_json::Value, _> =
                    serde_json::from_str(json_str)
                else {
                    continue;
                };
                let Some(doc_sparse) = SparseVector::from_json(&parsed) else {
                    continue;
                };
                let score = sparse_dot(query, &doc_sparse);
                if score <= 0.0 {
                    continue;
                }

                let full_text = full_text_col
                    .as_ref()
                    .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                    .unwrap_or("");
                let snippet = full_text.chars().take(400).collect::<String>();

                let result = SearchResult {
                    doc_id: str_val(doc_id_col, i),
                    location_uri: str_val(location_uri_col, i),
                    owner_id: str_val(owner_id_col, i),
                    title: str_col_val_opt(&title_col, i),
                    author: str_col_val_opt(&author_col, i),
                    year: year_col.as_ref().and_then(|c| {
                        if c.is_null(i) {
                            None
                        } else {
                            Some(c.value(i))
                        }
                    }),
                    filename: str_col_val_opt(&filename_col, i),
                    ext: str_col_val_opt(&ext_col, i),
                    language: str_col_val_opt(&language_col, i),
                    snippet,
                    score,
                    chunk_index: chunk_idx_col.value(i),
                };
                let doc_id = result.doc_id.clone();
                let is_better = match best.get(&doc_id) {
                    None => true,
                    Some((_, prev)) => score > *prev,
                };
                if is_better {
                    best.insert(doc_id, (result, score));
                }
            }
        }

        let mut results: Vec<SearchResult> = best.into_values().map(|(r, _)| r).collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }

    /// Fetch one representative row per doc_id (chunk_index = 0) for result hydration.
    ///
    /// Using `chunk_index = 0` guarantees exactly one row per document regardless
    /// of how many chunks were ingested, which avoids the N×chunks limit problem
    /// and makes result deduplication trivial.
    pub async fn fetch_by_doc_ids(&self, doc_ids: &[String]) -> Result<Vec<RecordBatch>> {
        if doc_ids.is_empty() {
            return Ok(vec![]);
        }

        let quoted: Vec<String> = doc_ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        let filter = format!("doc_id IN ({}) AND chunk_index = 0", quoted.join(", "));

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .limit(doc_ids.len())
            .execute()
            .await?
            .try_collect()
            .await?;
        Ok(batches)
    }

    // ── Mutation ───────────────────────────────────────────────────────────

    /// Delete all rows for a document (all chunks).
    pub async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        let expr = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        self.table.delete(&expr).await.context("LanceDB delete")?;
        Ok(())
    }

    /// Update the `location_uri` column for all rows of a document.
    pub async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        let new_val = format!("'{}'", new_uri.replace('\'', "''"));
        self.table
            .update()
            .only_if(filter)
            .column("location_uri", new_val)
            .execute()
            .await
            .context("LanceDB update_location")?;
        Ok(())
    }

    /// Update `location_uri` by matching the old URI (no doc_id required).
    pub async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()> {
        let filter = format!("location_uri = '{}'", old_uri.replace('\'', "''"));
        let new_val = format!("'{}'", new_uri.replace('\'', "''"));
        self.table
            .update()
            .only_if(filter)
            .column("location_uri", new_val)
            .execute()
            .await
            .context("LanceDB update_location_by_uri")?;
        Ok(())
    }

    // ── Index building ─────────────────────────────────────────────────────

    /// Build an IVF-PQ ANN index on the `embedding` column.
    /// Call this once after the initial bulk ingest (≥ 256 rows recommended).
    pub async fn build_vector_index(&self) -> Result<()> {
        self.table
            .create_index(
                &["embedding"],
                Index::IvfPq(
                    IvfPqIndexBuilder::default()
                        .distance_type(DistanceType::Cosine)
                        .num_partitions(256)
                        .num_sub_vectors(self.dims as u32 / 8),
                ),
            )
            .execute()
            .await
            .context("building IVF-PQ index")?;
        Ok(())
    }

    // ── Stats ──────────────────────────────────────────────────────────────

    pub async fn count(&self) -> Result<usize> {
        Ok(self.table.count_rows(None).await?)
    }

    /// Number of unique documents (rows with chunk_index = 0).
    pub async fn count_docs(&self) -> Result<usize> {
        Ok(self
            .table
            .count_rows(Some("chunk_index = 0".to_owned()))
            .await?)
    }

    /// List all indexed documents: one row per document (chunk_index = 0).
    /// Suitable for the "Index-Inhalt" viewer in the frontend.
    pub async fn list_documents(&self, limit: usize) -> Result<Vec<SearchResult>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index = 0")
            .limit(limit)
            .execute()
            .await?
            .try_collect()
            .await?;
        record_batches_to_search_results(&batches)
    }
}

// ── IndexBackend impl ──────────────────────────────────────────────────────

#[async_trait]
impl IndexBackend for LocalIndex {
    async fn ingest(&self, doc: DocumentChunk) -> Result<()> {
        self.ingest_chunk(&doc).await
    }

    async fn search_text(
        &self,
        _query: &str,
        _filters: &SearchFilters,
        _limit: usize,
    ) -> Result<Vec<SearchResult>> {
        Err(anyhow!(
            "use FtsIndex for text search; wire via SearchEngine"
        ))
    }

    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_vector(embedding, filters, limit).await
    }

    async fn search_hybrid(
        &self,
        _query: &str,
        _embedding: &[f32],
        _filters: &SearchFilters,
        _limit: usize,
    ) -> Result<Vec<SearchResult>> {
        Err(anyhow!("use SearchEngine::search_hybrid for hybrid search"))
    }

    async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        self.delete_doc(doc_id).await
    }

    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        self.update_location(doc_id, new_uri).await
    }

    async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()> {
        self.update_location_by_uri(old_uri, new_uri).await
    }
}

// ── Public helpers used by search.rs ─────────────────────────────────────

/// Like `record_batches_to_search_results` but uses a pre-computed score map
/// (from Tantivy BM25 or RRF) rather than deriving score from `_distance`.
/// Called by `SearchEngine` after fetching metadata rows by doc_id list.
pub fn batches_to_search_results_with_scores(
    batches: &[RecordBatch],
    score_map: &std::collections::HashMap<String, f32>,
) -> Result<Vec<SearchResult>> {
    let mut results = Vec::new();

    for batch in batches {
        let n = batch.num_rows();
        let doc_id_col = str_col(batch, "doc_id")?;
        let location_uri_col = str_col(batch, "location_uri")?;
        let owner_id_col = str_col(batch, "owner_id")?;
        let title_col = str_col_opt(batch, "title");
        let author_col = str_col_opt(batch, "author");
        let year_col = i32_col_opt(batch, "year");
        let filename_col = str_col_opt(batch, "filename");
        let ext_col = str_col_opt(batch, "ext");
        let language_col = str_col_opt(batch, "language");
        let chunk_idx_col = i32_col(batch, "chunk_index")?;
        let full_text_col = str_col_opt(batch, "full_text");

        for i in 0..n {
            let doc_id = str_val(doc_id_col, i);
            let score = *score_map.get(&doc_id).unwrap_or(&0.0);

            let full_text = full_text_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .unwrap_or("");
            let snippet = full_text.chars().take(400).collect::<String>();

            results.push(SearchResult {
                doc_id,
                location_uri: str_val(location_uri_col, i),
                owner_id: str_val(owner_id_col, i),
                title: str_col_val_opt(&title_col, i),
                author: str_col_val_opt(&author_col, i),
                year: year_col.as_ref().and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i))
                    }
                }),
                filename: str_col_val_opt(&filename_col, i),
                ext: str_col_val_opt(&ext_col, i),
                language: str_col_val_opt(&language_col, i),
                snippet,
                score,
                chunk_index: chunk_idx_col.value(i),
            });
        }
    }

    Ok(results)
}

// ── Arrow helpers ──────────────────────────────────────────────────────────

/// Convert a slice of `DocumentChunk` into a single `RecordBatch` matching
/// the schema produced by `build_schema(dims)`. Column order must match exactly.
fn chunks_to_record_batch(
    chunks: &[DocumentChunk],
    dims: usize,
    schema: &Arc<Schema>,
) -> Result<RecordBatch> {
    let n = chunks.len();

    // ── Identity ─────────────────────────────────────────────────────────
    let ids: StringArray = chunks.iter().map(|c| Some(c.id.as_str())).collect();
    let doc_ids: StringArray = chunks.iter().map(|c| Some(c.doc_id.as_str())).collect();
    let location_uris: StringArray = chunks
        .iter()
        .map(|c| Some(c.location_uri.as_str()))
        .collect();
    let owner_ids: StringArray = chunks.iter().map(|c| Some(c.owner_id.as_str())).collect();

    // ── Document metadata ─────────────────────────────────────────────────
    let filenames: StringArray = chunks.iter().map(|c| c.filename.as_deref()).collect();
    let titles: StringArray = chunks.iter().map(|c| c.title.as_deref()).collect();
    let authors: StringArray = chunks.iter().map(|c| c.author.as_deref()).collect();
    let years: Int32Array = chunks.iter().map(|c| c.year).collect();
    let exts: StringArray = chunks.iter().map(|c| c.ext.as_deref()).collect();
    let languages: StringArray = chunks.iter().map(|c| c.language.as_deref()).collect();
    let page_counts: Int32Array = chunks.iter().map(|c| c.page_count).collect();

    // ── Text content ──────────────────────────────────────────────────────
    let headings: StringArray = chunks.iter().map(|c| c.headings_text.as_deref()).collect();
    let full_texts: StringArray = chunks.iter().map(|c| c.full_text.as_deref()).collect();
    let full_mds: StringArray = chunks.iter().map(|c| c.full_text_md.as_deref()).collect();

    // ── Embedding ─────────────────────────────────────────────────────────
    // FixedSizeList<Float32>
    let embedding_col: Arc<dyn Array> = {
        let flat: Vec<Option<f32>> = chunks
            .iter()
            .flat_map(|c| match &c.embedding {
                Some(v) => v.iter().map(|&x| Some(x)).collect::<Vec<_>>(),
                None => vec![None; dims],
            })
            .collect();
        Arc::new(FixedSizeListArray::from_iter_primitive::<
            arrow_array::types::Float32Type,
            _,
            _,
        >(
            flat.chunks(dims).map(|chunk| Some(chunk.iter().copied())),
            dims as i32,
        ))
    };

    let emb_sparse: StringArray = chunks
        .iter()
        .map(|c| c.embedding_sparse.as_deref())
        .collect();
    let emb_models: StringArray = chunks
        .iter()
        .map(|c| c.embedding_model.as_deref())
        .collect();

    // ── Chunking ──────────────────────────────────────────────────────────
    let chunk_indexes: Int32Array = chunks.iter().map(|c| Some(c.chunk_index)).collect();
    let chunk_totals: Int32Array = chunks.iter().map(|c| Some(c.chunk_total)).collect();
    let chunk_start_chars: Int32Array = chunks.iter().map(|c| c.chunk_start_char).collect();
    let chunk_end_chars: Int32Array = chunks.iter().map(|c| c.chunk_end_char).collect();

    // ── Provenance ────────────────────────────────────────────────────────
    let indexed_ats: TimestampMillisecondArray =
        chunks.iter().map(|c| Some(c.indexed_at)).collect();
    let source_hashes: StringArray = chunks
        .iter()
        .map(|c| Some(c.source_hash.as_str()))
        .collect();

    // tags: List<Utf8>
    let tags_col: Arc<dyn Array> = {
        let mut lb = ListBuilder::new(StringBuilder::new());
        for chunk in chunks {
            for tag in &chunk.tags {
                lb.values().append_value(tag);
            }
            lb.append(true);
        }
        Arc::new(lb.finish())
    };

    // metadata_json
    let metadata_jsons: StringArray = chunks.iter().map(|c| c.metadata_json.as_deref()).collect();

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ids) as Arc<dyn Array>,
            Arc::new(doc_ids),
            Arc::new(location_uris),
            Arc::new(owner_ids),
            Arc::new(filenames),
            Arc::new(titles),
            Arc::new(authors),
            Arc::new(years),
            Arc::new(exts),
            Arc::new(languages),
            Arc::new(page_counts),
            Arc::new(headings),
            Arc::new(full_texts),
            Arc::new(full_mds),
            embedding_col,
            Arc::new(emb_sparse),
            Arc::new(emb_models),
            Arc::new(chunk_indexes),
            Arc::new(chunk_totals),
            Arc::new(chunk_start_chars),
            Arc::new(chunk_end_chars),
            Arc::new(indexed_ats),
            Arc::new(source_hashes),
            tags_col,
            Arc::new(metadata_jsons),
        ],
    )
    .context("building RecordBatch")?;

    let _ = n; // used implicitly via iterators
    Ok(batch)
}

/// Extract `SearchResult` values from a stream of `RecordBatch`es returned by
/// a LanceDB vector query.
fn record_batches_to_search_results(batches: &[RecordBatch]) -> Result<Vec<SearchResult>> {
    let mut results = Vec::new();

    for batch in batches {
        let n = batch.num_rows();
        let doc_id_col = str_col(batch, "doc_id")?;
        let location_uri_col = str_col(batch, "location_uri")?;
        let owner_id_col = str_col(batch, "owner_id")?;
        let title_col = str_col_opt(batch, "title");
        let author_col = str_col_opt(batch, "author");
        let year_col = i32_col_opt(batch, "year");
        let filename_col = str_col_opt(batch, "filename");
        let ext_col = str_col_opt(batch, "ext");
        let language_col = str_col_opt(batch, "language");
        let chunk_idx_col = i32_col(batch, "chunk_index")?;
        let full_text_col = str_col_opt(batch, "full_text");

        // LanceDB appends a `_distance` column for vector queries.
        let score_col = f32_col_opt(batch, "_distance");

        for i in 0..n {
            let full_text = full_text_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .unwrap_or("");

            let snippet = full_text.chars().take(400).collect::<String>();

            // Convert cosine distance → similarity score (0..1, higher = better).
            let distance = score_col.as_ref().map(|c| c.value(i)).unwrap_or(1.0);
            let score = 1.0 - distance.clamp(0.0, 2.0) / 2.0;

            results.push(SearchResult {
                doc_id: str_val(doc_id_col, i),
                location_uri: str_val(location_uri_col, i),
                owner_id: str_val(owner_id_col, i),
                title: str_col_val_opt(&title_col, i),
                author: str_col_val_opt(&author_col, i),
                year: year_col.as_ref().and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i))
                    }
                }),
                filename: str_col_val_opt(&filename_col, i),
                ext: str_col_val_opt(&ext_col, i),
                language: str_col_val_opt(&language_col, i),
                snippet,
                score,
                chunk_index: chunk_idx_col.value(i),
            });
        }
    }

    Ok(results)
}

// ── Column extraction helpers ──────────────────────────────────────────────

fn str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    let col = batch
        .column_by_name(name)
        .ok_or_else(|| anyhow!("missing column: {name}"))?;
    col.as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("column {name} is not StringArray"))
}

fn str_col_opt<'a>(batch: &'a RecordBatch, name: &str) -> Option<&'a StringArray> {
    batch
        .column_by_name(name)?
        .as_any()
        .downcast_ref::<StringArray>()
}

fn i32_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int32Array> {
    let col = batch
        .column_by_name(name)
        .ok_or_else(|| anyhow!("missing column: {name}"))?;
    col.as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("column {name} is not Int32Array"))
}

fn i32_col_opt<'a>(batch: &'a RecordBatch, name: &str) -> Option<&'a Int32Array> {
    batch
        .column_by_name(name)?
        .as_any()
        .downcast_ref::<Int32Array>()
}

fn f32_col_opt<'a>(batch: &'a RecordBatch, name: &str) -> Option<&'a Float32Array> {
    batch
        .column_by_name(name)?
        .as_any()
        .downcast_ref::<Float32Array>()
}

fn str_val(arr: &StringArray, i: usize) -> String {
    if arr.is_null(i) {
        String::new()
    } else {
        arr.value(i).to_owned()
    }
}

fn str_col_val_opt(arr: &Option<&StringArray>, i: usize) -> Option<String> {
    arr.as_ref().and_then(|c| {
        if c.is_null(i) {
            None
        } else {
            Some(c.value(i).to_owned())
        }
    })
}

// ── Error helpers ──────────────────────────────────────────────────────────

fn is_table_not_found(e: &lancedb::Error) -> bool {
    // LanceDB returns TableNotFound when the table doesn't exist yet.
    matches!(e, lancedb::Error::TableNotFound { .. })
}

// ── Sparse scoring ─────────────────────────────────────────────────────────

/// Dot product of two sparse vectors. BGE-M3 / SPLADE outputs are sorted by
/// token id, but this implementation sorts on the fly when needed so the
/// scoring is correct regardless. Hot path uses the merge form when both
/// inputs are already sorted (the common case).
pub(super) fn sparse_dot(a: &SparseVector, b: &SparseVector) -> f32 {
    if a.indices.is_empty() || b.indices.is_empty() {
        return 0.0;
    }

    let a_sorted = is_sorted_ascending(&a.indices);
    let b_sorted = is_sorted_ascending(&b.indices);

    if a_sorted && b_sorted {
        // Two-pointer merge — O(|a| + |b|).
        let mut score = 0.0f32;
        let (mut i, mut j) = (0usize, 0usize);
        while i < a.indices.len() && j < b.indices.len() {
            match a.indices[i].cmp(&b.indices[j]) {
                std::cmp::Ordering::Equal => {
                    score += a.values[i] * b.values[j];
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        return score;
    }

    // Fallback: hash join — O(|a| + |b|) but allocates a small map.
    let map: std::collections::HashMap<u32, f32> =
        a.indices.iter().copied().zip(a.values.iter().copied()).collect();
    let mut score = 0.0f32;
    for (idx, val) in b.indices.iter().zip(b.values.iter()) {
        if let Some(&w) = map.get(idx) {
            score += w * val;
        }
    }
    score
}

fn is_sorted_ascending(v: &[u32]) -> bool {
    v.windows(2).all(|w| w[0] <= w[1])
}

#[cfg(test)]
mod sparse_tests {
    use super::*;
    use crate::index::embedder::SparseVector;

    fn sv(indices: Vec<u32>, values: Vec<f32>) -> SparseVector {
        SparseVector { indices, values }
    }

    #[test]
    fn dot_disjoint_is_zero() {
        let a = sv(vec![1, 2, 3], vec![1.0, 1.0, 1.0]);
        let b = sv(vec![4, 5, 6], vec![1.0, 1.0, 1.0]);
        assert_eq!(sparse_dot(&a, &b), 0.0);
    }

    #[test]
    fn dot_full_overlap_is_sum_of_products() {
        let a = sv(vec![1, 2, 3], vec![1.0, 2.0, 3.0]);
        let b = sv(vec![1, 2, 3], vec![4.0, 5.0, 6.0]);
        assert!((sparse_dot(&a, &b) - 32.0).abs() < 1e-6); // 4 + 10 + 18
    }

    #[test]
    fn dot_partial_overlap() {
        let a = sv(vec![1, 3, 5], vec![1.0, 1.0, 1.0]);
        let b = sv(vec![3, 5, 7], vec![2.0, 3.0, 4.0]);
        assert!((sparse_dot(&a, &b) - 5.0).abs() < 1e-6); // 2 + 3
    }

    #[test]
    fn dot_works_when_unsorted() {
        // Force the hash-join path by giving b an out-of-order index list.
        let a = sv(vec![1, 2, 3], vec![1.0, 1.0, 1.0]);
        let b = sv(vec![3, 1, 2], vec![3.0, 1.0, 2.0]);
        assert!((sparse_dot(&a, &b) - 6.0).abs() < 1e-6);
    }

    #[test]
    fn dot_empty_inputs_zero() {
        let a = sv(vec![], vec![]);
        let b = sv(vec![1, 2], vec![1.0, 1.0]);
        assert_eq!(sparse_dot(&a, &b), 0.0);
        assert_eq!(sparse_dot(&b, &a), 0.0);
    }
}
