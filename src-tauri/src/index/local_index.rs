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
    index::{scalar::BTreeIndexBuilder, vector::IvfPqIndexBuilder, Index},
    query::{ExecutableQuery, QueryBase},
    table::NewColumnTransform,
    Connection, DistanceType, Table,
};

use super::embedder::SparseVector;
use super::schema::{build_schema, DocumentChunk, SearchFilters, SearchResult};
use super::IndexBackend;

// ── Constant ───────────────────────────────────────────────────────────────

const TABLE_NAME: &str = "documents";

/// One row returned by `list_failed_extractions`.
pub struct FailedExtractionRow {
    pub doc_id:       String,
    pub location_uri: String,
    pub filename:     Option<String>,
    pub reason:       String,
    pub retryable:    bool,
}

// ── Struct ─────────────────────────────────────────────────────────────────

pub struct LocalIndex {
    // Kept alive to maintain the LanceDB connection for the table lifetime.
    _db: Connection,
    table: Table,
    pub dims: usize,
}

// ── Constructor ────────────────────────────────────────────────────────────

impl LocalIndex {
    /// Borrow the underlying LanceDB table handle.  Exposed so the
    /// migration framework (`crate::migrations` / `index::migrations`)
    /// can call `add_columns` etc. against the same handle this
    /// index opened, without re-resolving the LanceDB URI.
    pub fn table_ref(&self) -> &Table {
        &self.table
    }

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

        migrate_add_parent_dir_column(&table)
            .await
            .context("schema v2: adding parent_dir column")?;
        migrate_add_volume_id_column(&table)
            .await
            .context("schema v3: adding volume_id column")?;

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
            let metadata_col = str_col_opt(batch, "metadata_json");
            let indexed_at_col = ts_ms_col_opt(batch, "indexed_at");
            let volume_id_col = str_col_opt(batch, "volume_id");
            let source_hash_col = str_col_opt(batch, "source_hash");
            let text_translated_col = str_col_opt(batch, "text_translated");
            let text_translated_lang_col = str_col_opt(batch, "text_translated_lang");

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

                let volume_id = str_col_val_opt(&volume_id_col, i).or_else(|| {
                    metadata_col
                        .as_ref()
                        .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                        .and_then(parse_volume_id_from_metadata)
                });

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
                    metadata_json: str_col_val_opt(&metadata_col, i),
                    catalog_source: None,
                    volume_id,
                    indexed_at: indexed_at_col.map(|c| c.value(i)).unwrap_or(0),
                    source_hash: str_col_val_opt(&source_hash_col, i).unwrap_or_default(),
                    text_translated: str_col_val_opt(&text_translated_col, i),
                    text_translated_lang: str_col_val_opt(&text_translated_lang_col, i),
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

    /// Same as [`Self::fetch_by_doc_ids`] but appends `extra_sql` to the
    /// `doc_id IN (...) AND chunk_index = 0` predicate via ` AND `.  Used
    /// by the CLI search-with-filters path (P13.7 Step 6): the BM25 stage
    /// produces a candidate doc_id set, the filters get pushed to LanceDB
    /// as a SQL fragment instead of post-hoc Rust filtering.  Empty
    /// `extra_sql` is a no-op.
    pub async fn fetch_by_doc_ids_filtered(
        &self,
        doc_ids: &[String],
        extra_sql: Option<&str>,
    ) -> Result<Vec<RecordBatch>> {
        if doc_ids.is_empty() {
            return Ok(vec![]);
        }
        let quoted: Vec<String> = doc_ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        let mut filter = format!(
            "doc_id IN ({}) AND chunk_index = 0",
            quoted.join(", ")
        );
        if let Some(extra) = extra_sql {
            let trimmed = extra.trim();
            if !trimmed.is_empty() {
                filter.push_str(" AND ");
                filter.push_str(trimmed);
            }
        }
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

    /// Wrapper around [`Self::fetch_by_doc_ids_filtered`] that returns
    /// the converted SearchResult vec.
    pub async fn fetch_search_results_by_ids_filtered(
        &self,
        doc_ids: &[String],
        extra_sql: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let batches = self.fetch_by_doc_ids_filtered(doc_ids, extra_sql).await?;
        record_batches_to_search_results(&batches)
    }

    /// Fetch `SearchResult`s for a set of doc IDs — combines `fetch_by_doc_ids`
    /// with the private batch→result conversion. Useful in CLI and test contexts
    /// that can't call the private `record_batches_to_search_results` directly.
    pub async fn fetch_search_results_by_ids(
        &self,
        doc_ids: &[String],
    ) -> Result<Vec<SearchResult>> {
        let batches = self.fetch_by_doc_ids(doc_ids).await?;
        record_batches_to_search_results(&batches)
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

    /// Remove `extraction_failure` from `metadata_json` so the next background
    /// ingest run re-attempts extraction. Only meaningful for retryable reasons
    /// (Timeout / Other); the caller should check `is_retryable()` first.
    /// Also sets `level` back to 1 so the row doesn't look like a completed L2.
    pub async fn clear_extraction_failure(&self, doc_id: &str) -> Result<()> {
        // Read the current metadata_json.
        let pred = format!(
            "doc_id = '{}' AND chunk_index <= 0",
            doc_id.replace('\'', "''")
        );
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(&pred)
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;
        let current_meta: Option<serde_json::Value> = batches
            .iter()
            .find_map(|b| {
                let idx = b.schema().index_of("metadata_json").ok()?;
                let arr = b.column(idx)
                    .as_any()
                    .downcast_ref::<arrow_array::StringArray>()?;
                let raw = arr.value(0);
                serde_json::from_str(raw).ok()
            });
        let new_meta = if let Some(mut m) = current_meta {
            m.as_object_mut().map(|o| {
                o.remove("extraction_failure");
                o.insert("level".to_owned(), serde_json::Value::from(1i64));
            });
            m.to_string()
        } else {
            r#"{"level":1}"#.to_owned()
        };
        self.table
            .update()
            .only_if(pred)
            .column("metadata_json", format!("'{}'", new_meta.replace('\'', "''")))
            .execute()
            .await
            .context("clear_extraction_failure update")?;
        Ok(())
    }

    /// Patch L2 metadata fields on an existing row. Pass `None` to leave a
    /// column untouched. `metadata_json_merge` is JSON that gets shallow-merged
    /// into the existing `metadata_json` (the caller is responsible for
    /// computing the final blob — see `index_promote_l2`).
    pub async fn update_l2_fields(
        &self,
        doc_id: &str,
        title: Option<&str>,
        author: Option<&str>,
        year: Option<i32>,
        language: Option<&str>,
        page_count: Option<i32>,
        metadata_json: Option<&str>,
    ) -> Result<()> {
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        let mut update = self.table.update().only_if(filter);

        if let Some(t) = title {
            update = update.column("title", format!("'{}'", t.replace('\'', "''")));
        }
        if let Some(a) = author {
            update = update.column("author", format!("'{}'", a.replace('\'', "''")));
        }
        if let Some(y) = year {
            update = update.column("year", y.to_string());
        }
        if let Some(lang) = language {
            update = update.column("language", format!("'{}'", lang.replace('\'', "''")));
        }
        if let Some(pc) = page_count {
            update = update.column("page_count", pc.to_string());
        }
        if let Some(meta) = metadata_json {
            update = update.column("metadata_json", format!("'{}'", meta.replace('\'', "''")));
        }

        update.execute().await.context("LanceDB update_l2_fields")?;
        Ok(())
    }

    // ── Index building ─────────────────────────────────────────────────────

    /// Build an IVF-PQ ANN index on the `embedding` column.
    ///
    /// `num_partitions` — number of Voronoi cells. `None` auto-scales to
    ///   `sqrt(row_count)` clamped to [64, 65536], which is the standard
    ///   heuristic. Pass an explicit value to override.
    /// `sample_rate` — K-Means trains on `sample_rate × num_partitions`
    ///   randomly-sampled rows. Default 256. Raise to 512-1024 for very
    ///   large tables to improve centroid quality at the cost of more RAM.
    ///
    /// Call this once after initial bulk ingest (≥ num_partitions rows).
    pub async fn build_vector_index(
        &self,
        num_partitions: Option<u32>,
        sample_rate: Option<u32>,
    ) -> Result<()> {
        let row_count = self.count().await?.max(1);
        let partitions = num_partitions.unwrap_or_else(|| {
            let auto = (row_count as f64).sqrt() as u32;
            auto.clamp(64, 65536)
        });
        let sr = sample_rate.unwrap_or(256);
        self.table
            .create_index(
                &["embedding"],
                Index::IvfPq(
                    IvfPqIndexBuilder::default()
                        .distance_type(DistanceType::Cosine)
                        .num_partitions(partitions)
                        .num_sub_vectors(self.dims as u32 / 8)
                        .sample_rate(sr),
                ),
            )
            .execute()
            .await
            .context("building IVF-PQ index")?;
        Ok(())
    }

    /// Build BTree scalar indexes on `parent_dir` and `volume_id` for fast
    /// filtering in `query_documents`. Safe to call repeatedly — LanceDB
    /// will replace old indexes. Typically called once after the first
    /// bulk L1 ingest or whenever the table grows significantly.
    pub async fn build_scalar_index(&self) -> Result<()> {
        self.table
            .create_index(&["parent_dir"], Index::BTree(BTreeIndexBuilder::default()))
            .execute()
            .await
            .context("building BTree scalar index on parent_dir")?;
        self.table
            .create_index(&["volume_id"], Index::BTree(BTreeIndexBuilder::default()))
            .execute()
            .await
            .context("building BTree scalar index on volume_id")?;
        Ok(())
    }

    // ── Stats ──────────────────────────────────────────────────────────────

    pub async fn count(&self) -> Result<usize> {
        Ok(self.table.count_rows(None).await?)
    }

    /// Number of unique documents.
    ///
    /// L3 docs have a `chunk_index = 0` row; L1 docs have only `chunk_index = -1`.
    /// `chunk_index <= 0` counts each unique doc exactly once.
    pub async fn count_docs(&self) -> Result<usize> {
        Ok(self
            .table
            .count_rows(Some("chunk_index <= 0".to_owned()))
            .await?)
    }

    /// List all indexed documents: one representative row per document.
    /// Suitable for the catalog viewer in the frontend.
    ///
    /// Uses `chunk_index <= 0` so both L3 docs (their first chunk) and L1
    /// metadata-only rows (chunk_index = -1) are returned.
    pub async fn list_documents(&self, limit: usize) -> Result<Vec<SearchResult>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index <= 0")
            .limit(limit)
            .execute()
            .await?
            .try_collect()
            .await?;
        record_batches_to_search_results(&batches)
    }

    /// PLAN P9 step 1 — paginated, filterable, sortable browse of the
    /// documents table. Replaces the load-the-whole-table fetch in the
    /// Catalog overview pane with a windowed read.
    ///
    /// **Pagination model — current and future.** LanceDB 0.26's public
    /// Rust query API doesn't expose `ORDER BY`, so this implementation
    /// fetches `min(total, offset + limit)` rows, sorts the window
    /// in-process, and slices to `[offset..offset+limit]`. That's
    /// correct but linear in `offset` — fine for the first ~50 pages
    /// (~10k rows on a 200-row page) but degrades after that. Step 5
    /// swaps this for a keyset cursor over an indexed sort column once
    /// we drop down to `lance::Scanner` for DB-side ordering. The
    /// `PageCursor` API was designed keyset-shaped on purpose — only
    /// the encoding changes.
    ///
    /// `total_estimate` comes from `count_rows(filter)` — a scalar
    /// query against the same predicate, so it's cheap (no row
    /// materialisation) and lets the UI render "342k matches" without
    /// listing them.
    pub async fn query_documents(
        &self,
        filter: &super::schema::DocumentFilter,
        sort: super::schema::SortSpec,
        page: super::schema::PageSpec,
    ) -> Result<super::schema::DocumentPage> {
        use lance::dataset::scanner::ColumnOrdering;
        use super::schema::{DocumentPage, PageCursor, SortColumn, SortDir};

        let limit = page.limit.clamp(1, 1000) as usize;
        let offset = page.cursor.as_ref().map(|c| c.offset()).unwrap_or(0) as usize;
        let base_filter = filter_to_sql(filter);

        // count_rows is a cheap metadata scan — no row materialisation.
        let total_estimate = self
            .table
            .count_rows(base_filter.clone())
            .await
            .context("count_rows for total_estimate")? as u64;

        if offset as u64 >= total_estimate {
            return Ok(DocumentPage {
                rows: vec![],
                next_cursor: None,
                total_estimate,
            });
        }

        // P9 step 5 — drop to lance::Scanner for DB-side ORDER BY + LIMIT + OFFSET.
        // Dataset::scan() clones the dataset into an Arc internally, so the read
        // guard can be released immediately after calling scan().
        let mut scanner = {
            let guard = self
                .table
                .dataset()
                .ok_or_else(|| anyhow!("local index: not a native LanceDB table"))?
                .get()
                .await
                .context("acquiring lance dataset read guard")?;
            guard.scan()
        };

        if let Some(ref sql) = base_filter {
            scanner.filter(sql).context("scanner filter")?;
        }

        let col_name = match sort.column {
            SortColumn::Filename  => "filename",
            SortColumn::Title     => "title",
            SortColumn::Author    => "author",
            SortColumn::Year      => "year",
            SortColumn::Language  => "language",
            SortColumn::IndexedAt => "indexed_at",
            SortColumn::ParentDir => "parent_dir",
        };
        let ordering = match sort.direction {
            SortDir::Asc  => ColumnOrdering::asc_nulls_last(col_name.to_owned()),
            SortDir::Desc => ColumnOrdering::desc_nulls_last(col_name.to_owned()),
        };
        scanner
            .order_by(Some(vec![ordering]))
            .context("scanner order_by")?;

        scanner
            .limit(Some(limit as i64), Some(offset as i64))
            .context("scanner limit")?;

        let batches: Vec<RecordBatch> = scanner
            .try_into_stream()
            .await
            .context("scanner try_into_stream")?
            .try_collect()
            .await
            .context("collecting scanner batches")?;

        let rows = record_batches_to_search_results(&batches)?;

        let next_offset = offset + rows.len();
        let next_cursor = if rows.len() < limit || (next_offset as u64) >= total_estimate {
            None
        } else {
            Some(PageCursor::from_offset(next_offset as u32))
        };

        Ok(DocumentPage {
            rows,
            next_cursor,
            total_estimate,
        })
    }

    /// PLAN P9 step 4 — enumerate the immediate subdirectories of `parent`.
    ///
    /// Returns one [`FolderChild`] per unique path component that immediately
    /// follows `parent` in the `parent_dir` column, with the total count of
    /// L1/L3 rows in that subtree. Callers can use this to build a lazy-
    /// loaded folder tree without fetching row payloads.
    ///
    /// `parent = ""` returns the top-level path roots (e.g. `/Users`,
    /// `/Volumes/Archive`) from all indexed rows. `parent = "/Users/alice"`
    /// returns `Documents`, `Downloads`, etc.
    ///
    /// The BTree scalar index on `parent_dir` means the `LIKE 'parent%'`
    /// predicate is index-accelerated — only the relevant leaf pages are
    /// scanned, not the whole table.
    pub async fn folder_children(
        &self,
        parent: &str,
        owner_id: Option<&str>,
    ) -> Result<Vec<super::schema::FolderChild>> {
        use super::schema::FolderChild;
        use std::collections::HashMap;

        // Build the LanceDB predicate: chunk_index <= 0 limits to metadata rows;
        // the parent_dir LIKE clause narrows to the subtree (index-assisted).
        let mut pred = "chunk_index <= 0".to_owned();
        if !parent.is_empty() {
            let escaped = parent.replace('\'', "''").replace('%', "\\%").replace('_', "\\_");
            pred.push_str(&format!(" AND parent_dir LIKE '{}%'", escaped));
        }
        if let Some(oid) = owner_id.filter(|s| !s.is_empty()) {
            pred.push_str(&format!(" AND owner_id = '{}'", oid.replace('\'', "''")));
        }

        // We only need the parent_dir column — project it to minimise data transfer.
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(&pred)
            .select(lancedb::query::Select::Columns(vec!["parent_dir".to_owned()]))
            .limit(200_000)   // safety cap: 200k distinct rows at ~50 B/row ≈ 10 MB
            .execute()
            .await?
            .try_collect()
            .await?;

        // Group by immediate child segment. For parent="/Users/alice", a row
        // with parent_dir="/Users/alice/Documents/Papers" contributes 1 to "Documents".
        // A row with parent_dir="/Users/alice" itself is a direct-child doc and
        // produces no subfolder entry.
        let prefix = if parent.is_empty() {
            String::new()
        } else {
            format!("{}/", parent)
        };

        let mut counts: HashMap<String, u64> = HashMap::new();
        for batch in &batches {
            let Some(col) = str_col_opt(batch, "parent_dir") else { continue };
            for i in 0..batch.num_rows() {
                if col.is_null(i) {
                    continue;
                }
                let pd = col.value(i);
                // Skip rows whose parent_dir IS the parent (direct children —
                // they live in this folder, not in a subfolder of it).
                let rest = if prefix.is_empty() {
                    pd
                } else {
                    match pd.strip_prefix(&prefix) {
                        Some(r) => r,
                        None => continue,
                    }
                };
                // The immediate child is the first path component of `rest`.
                // For Unix paths from root (empty prefix), `rest` starts with
                // "/" — take everything up to the next "/" after the leading one.
                let child_name = if prefix.is_empty() {
                    // e.g. rest = "/Users/alice/Documents" → child = "/Users"
                    let without_slash = rest.trim_start_matches('/');
                    let seg = without_slash.split('/').next().unwrap_or("");
                    if seg.is_empty() { continue; }
                    // Re-add the leading slash so the path is usable as a prefix.
                    format!("/{}", seg)
                } else {
                    let seg = rest.split('/').next().unwrap_or("");
                    if seg.is_empty() { continue; }
                    seg.to_owned()
                };

                *counts.entry(child_name).or_insert(0) += 1;
            }
        }

        let mut children: Vec<FolderChild> = counts
            .into_iter()
            .map(|(name, doc_count)| {
                let path = if prefix.is_empty() {
                    name.clone()   // already has leading "/"
                } else {
                    format!("{}{}", prefix, name)
                };
                FolderChild { name, path, doc_count }
            })
            .collect();
        children.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(children)
    }

    /// PLAN P7.4.3 — read the source-file mtime stored in the documents
    /// table for this `location_uri`, if any.
    ///
    /// Looks up the chunk_index = 0 row matching `location_uri`, parses
    /// `metadata_json` as `{"mtime_unix": <secs>}`, returns the value.
    /// `None` when the row is missing or `metadata_json` is empty / has
    /// no `mtime_unix` key (e.g. rows ingested before P7.4.3 landed,
    /// or rows from `index_ingest_document` which doesn't carry source
    /// mtime).
    ///
    /// Background ingest uses this to mtime-skip files that haven't
    /// changed since last index — saves the read + extract + embed
    /// cost on the common "no new content" case.
    /// Same as `extraction_failure_reason_for_uri` but looks up by `doc_id`.
    pub async fn extraction_failure_reason_for_uri_by_doc_id(
        &self,
        doc_id: &str,
    ) -> Result<Option<String>> {
        let pred = format!(
            "doc_id = '{}' AND chunk_index <= 0",
            doc_id.replace('\'', "''")
        );
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(&pred)
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;
        for batch in &batches {
            if let Some(meta_idx) = batch.schema().index_of("metadata_json").ok() {
                let col = batch.column(meta_idx);
                let arr = col
                    .as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .ok_or_else(|| anyhow!("metadata_json column not StringArray"))?;
                for i in 0..batch.num_rows() {
                    if arr.is_null(i) { continue; }
                    let Ok(v): std::result::Result<serde_json::Value, _> =
                        serde_json::from_str(arr.value(i))
                    else { continue; };
                    if let Some(reason) = v
                        .get("extraction_failure")
                        .and_then(|f| f.get("reason"))
                        .and_then(|r| r.as_str())
                    {
                        return Ok(Some(reason.to_owned()));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Return the `extraction_failure.reason` tag for a URI if one exists,
    /// so `bg_ingest` can skip non-retryable failures (Drm / Corrupt /
    /// Unsupported) on subsequent scans without re-attempting extraction.
    pub async fn extraction_failure_reason_for_uri(
        &self,
        location_uri: &str,
    ) -> Result<Option<String>> {
        let pred = format!(
            "location_uri = '{}' AND chunk_index = 0",
            location_uri.replace('\'', "''")
        );
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(&pred)
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;
        for batch in &batches {
            if let Some(meta_idx) = batch.schema().index_of("metadata_json").ok() {
                let col = batch.column(meta_idx);
                let arr = col
                    .as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .ok_or_else(|| anyhow!("metadata_json column not StringArray"))?;
                for i in 0..batch.num_rows() {
                    if arr.is_null(i) {
                        continue;
                    }
                    let json = arr.value(i);
                    let Ok(v): std::result::Result<serde_json::Value, _> =
                        serde_json::from_str(json)
                    else {
                        continue;
                    };
                    if let Some(reason) = v
                        .get("extraction_failure")
                        .and_then(|f| f.get("reason"))
                        .and_then(|r| r.as_str())
                    {
                        return Ok(Some(reason.to_owned()));
                    }
                }
            }
        }
        Ok(None)
    }

    pub async fn indexed_mtime_for_uri(&self, location_uri: &str) -> Result<Option<i64>> {
        let pred = format!(
            "location_uri = '{}' AND chunk_index = 0",
            location_uri.replace('\'', "''")
        );
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(&pred)
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;
        for batch in &batches {
            if let Some(meta_idx) = batch.schema().index_of("metadata_json").ok() {
                let col = batch.column(meta_idx);
                let arr = col
                    .as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .ok_or_else(|| anyhow!("metadata_json column not StringArray"))?;
                for i in 0..batch.num_rows() {
                    if arr.is_null(i) {
                        continue;
                    }
                    let json = arr.value(i);
                    // Tiny hand-parse — avoids serde_json dep cost for a
                    // single integer field with a fixed key. Any change
                    // beyond `{"mtime_unix": N, ...}` shape needs a real
                    // parser.
                    if let Some(start) = json.find("\"mtime_unix\"") {
                        let after = &json[start + "\"mtime_unix\"".len()..];
                        // Skip optional whitespace + colon + whitespace.
                        let rest = after.trim_start();
                        let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
                        // Read the integer up to the next non-digit.
                        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
                        if end > 0 {
                            if let Ok(v) = rest[..end].parse::<i64>() {
                                return Ok(Some(v));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    // ── P10 — failed-extraction helpers ────────────────────────────────────

    /// Scan for all rows that carry an `extraction_failure.reason` blob.
    /// When `retryable_only = true` only Timeout / Other are returned.
    pub async fn list_failed_extractions(
        &self,
        retryable_only: bool,
    ) -> Result<Vec<FailedExtractionRow>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index <= 0")
            .execute()
            .await?
            .try_collect()
            .await?;

        let mut out = Vec::new();
        for batch in &batches {
            let Some(meta_idx) = batch.schema().index_of("metadata_json").ok() else { continue };
            let Some(doc_id_idx) = batch.schema().index_of("doc_id").ok() else { continue };
            let Some(uri_idx) = batch.schema().index_of("location_uri").ok() else { continue };
            let fname_idx = batch.schema().index_of("filename").ok();

            let meta_arr = batch.column(meta_idx).as_any().downcast_ref::<StringArray>();
            let doc_arr  = batch.column(doc_id_idx).as_any().downcast_ref::<StringArray>();
            let uri_arr  = batch.column(uri_idx).as_any().downcast_ref::<StringArray>();

            let (Some(meta_arr), Some(doc_arr), Some(uri_arr)) = (meta_arr, doc_arr, uri_arr)
            else { continue };

            for i in 0..batch.num_rows() {
                if meta_arr.is_null(i) { continue; }
                let Ok(v): std::result::Result<serde_json::Value, _> =
                    serde_json::from_str(meta_arr.value(i))
                else { continue; };
                let Some(reason) = v
                    .get("extraction_failure")
                    .and_then(|f| f.get("reason"))
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_owned())
                else { continue; };

                use crate::index::task_failure::TaskFailureReason;
                let tfr = match reason.as_str() {
                    "timeout" => TaskFailureReason::Timeout,
                    "other"   => TaskFailureReason::Other,
                    _         => {
                        if retryable_only { continue; }
                        TaskFailureReason::Corrupt
                    }
                };
                let retryable = tfr.is_retryable();
                if retryable_only && !retryable { continue; }

                let filename: Option<String> = fname_idx.and_then(|fi| {
                    let arr = batch.column(fi).as_any().downcast_ref::<StringArray>()?;
                    if arr.is_null(i) { None } else { Some(arr.value(i).to_owned()) }
                });

                out.push(FailedExtractionRow {
                    doc_id:       if doc_arr.is_null(i) { String::new() } else { doc_arr.value(i).to_owned() },
                    location_uri: if uri_arr.is_null(i) { String::new() } else { uri_arr.value(i).to_owned() },
                    filename,
                    reason,
                    retryable,
                });
            }
        }
        Ok(out)
    }

    /// Clear `extraction_failure` for all retryable rows (Timeout / Other).
    /// Returns the number of rows cleared.
    pub async fn retry_all_failed_extractions(&self) -> Result<usize> {
        let rows = self.list_failed_extractions(true).await?;
        for row in &rows {
            self.clear_extraction_failure(&row.doc_id).await?;
        }
        Ok(rows.len())
    }

    // ── P7.7 — .cidx export / import ──────────────────────────────────────

    /// Export a per-volume (or full) slice of the documents table to a
    /// portable LanceDB directory at `dest_path` (the `.cidx` dir).
    ///
    /// The exported directory has the same structure as the main index
    /// (`dest_path/documents.lance/`) so it can be opened with the same
    /// `LocalIndex::open_cidx` API.
    ///
    /// `volume_id` — export only rows for this volume. `None` = full snapshot.
    /// `include_embeddings` — when `false` (default) the embedding columns
    ///   are stripped; the snapshot supports FTS + columnar browse offline.
    /// `include_fts` — when `true` (default `false`), a Tantivy FTS index is
    ///   built at `dest_path/fts/` from the exported rows' title + full_text.
    ///   Enables offline BM25 full-text search on the `.cidx` archive.
    ///   Implies exporting `full_text` + `headings_text` columns.
    pub async fn export_cidx(
        &self,
        dest_path: &Path,
        volume_id: Option<&str>,
        include_embeddings: bool,
        include_fts: bool,
    ) -> Result<usize> {
        // Build query.
        let filter_sql: Option<String> = volume_id.map(|v| {
            format!("volume_id = '{}'", v.replace('\'', "''"))
        });
        let mut meta_cols: Vec<&str> = vec![
            "id", "doc_id", "location_uri", "owner_id",
            "filename", "title", "author", "year", "ext",
            "language", "page_count", "chunk_index", "chunk_total",
            "chunk_start_char", "chunk_end_char",
            "indexed_at", "source_hash", "tags",
            "metadata_json", "parent_dir", "volume_id",
        ];
        // Include text columns when building FTS.
        if include_fts {
            meta_cols.push("full_text");
            meta_cols.push("headings_text");
        }
        let meta_columns: Vec<String> = meta_cols.into_iter().map(String::from).collect();

        let mut q = self.table.query();
        if let Some(ref sql) = filter_sql {
            q = q.only_if(sql.as_str());
        }
        if !include_embeddings {
            q = q.select(lancedb::query::Select::Columns(meta_columns));
        }
        let batches: Vec<RecordBatch> = q.execute().await?.try_collect().await?;
        let row_count: usize = batches.iter().map(|b| b.num_rows()).sum();
        if row_count == 0 {
            return Err(anyhow!("no rows matched the export filter (volume_id={volume_id:?})"));
        }

        // Write into a fresh LanceDB at dest_path.
        std::fs::create_dir_all(dest_path)
            .with_context(|| format!("creating cidx dir {}", dest_path.display()))?;
        let dest_uri = dest_path.to_string_lossy().into_owned();
        let db_out = connect(&dest_uri).execute().await
            .with_context(|| format!("opening output DB at {dest_uri}"))?;

        // Clone schema (Arc<Schema>) before consuming batches.
        let schema = batches[0].schema().clone();
        let batches_for_write = batches.clone(); // cheap Arc clones
        let reader = arrow_array::RecordBatchIterator::new(
            batches_for_write.into_iter().map(Ok),
            schema,
        );
        db_out.create_table("documents", Box::new(reader))
            .execute()
            .await
            .context("writing documents table to .cidx")?;

        // ── FTS companion ────────────────────────────────────────────────
        if include_fts {
            let fts_dir = dest_path.join("fts");
            let fts = super::fts_index::FtsIndex::open_or_create(&fts_dir)
                .context("creating .cidx FTS index")?;
            let mut writer = fts.writer().context("opening FTS writer")?;
            let mut fts_docs = 0usize;

            for batch in &batches {
                let doc_id_col  = batch.schema().index_of("doc_id").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                let owner_col   = batch.schema().index_of("owner_id").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                let lang_col    = batch.schema().index_of("language").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                let title_col   = batch.schema().index_of("title").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                let heads_col   = batch.schema().index_of("headings_text").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                let text_col    = batch.schema().index_of("full_text").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                // text_translated is post-v100-migration only — older
                // archives don't have the column, hence the optional Some
                // and the per-row null-guard below.
                let text_trans_col = batch.schema().index_of("text_translated").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                let cidx_col    = batch.schema().index_of("chunk_index").ok()
                    .and_then(|i| batch.column(i).as_any().downcast_ref::<arrow_array::Int32Array>());

                let (Some(doc_id_col), Some(owner_col)) = (doc_id_col, owner_col)
                else { continue };

                for i in 0..batch.num_rows() {
                    // Only index chunk_index = 0 (first/only chunk per doc).
                    if let Some(cidx) = cidx_col.as_ref() {
                        if !cidx.is_null(i) && cidx.value(i) != 0 { continue; }
                    }
                    if doc_id_col.is_null(i) { continue; }
                    let doc_id  = doc_id_col.value(i).to_owned();
                    let owner   = if owner_col.is_null(i) { String::new() } else { owner_col.value(i).to_owned() };
                    let lang    = lang_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i)).unwrap_or("").to_owned();
                    let title   = title_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i)).unwrap_or("").to_owned();
                    let heads   = heads_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i)).unwrap_or("").to_owned();
                    let body    = text_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i)).unwrap_or("").to_owned();
                    let body_translated = text_trans_col
                        .as_ref()
                        .filter(|c| !c.is_null(i))
                        .map(|c| c.value(i));

                    fts.add_document(&mut writer, super::fts_index::TantivyInput {
                        doc_id: &doc_id,
                        owner_id: &owner,
                        language: &lang,
                        title: &title,
                        headings: &heads,
                        body: &body,
                        body_translated,
                    })?;
                    fts_docs += 1;
                }
            }
            writer.commit().context("committing .cidx FTS index")?;
            eprintln!("[cidx] FTS: indexed {fts_docs} documents in {}", fts_dir.display());
        }

        Ok(row_count)
    }

    /// Open an existing `.cidx` archive (a LanceDB-structured directory)
    /// and return a `LocalIndex` wrapping it. Read-only in practice since
    /// the ingest pipeline isn't wired; `query_documents` / `search_*` all work.
    pub async fn open_cidx(cidx_path: &Path) -> Result<Self> {
        let uri = cidx_path.to_string_lossy().into_owned();
        let db = connect(&uri).execute().await
            .with_context(|| format!("opening .cidx at {}", cidx_path.display()))?;
        let table = db.open_table("documents").execute().await
            .with_context(|| format!("opening documents table in .cidx at {}", cidx_path.display()))?;
        // dims=0: .cidx is read-only; the dim value only matters for new
        // table creation (not applicable here) and embedding operations
        // (not performed on snapshots).
        Ok(Self { _db: db, table, dims: 0 })
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
        let metadata_col = str_col_opt(batch, "metadata_json");
        let indexed_at_col = ts_ms_col_opt(batch, "indexed_at");
        let volume_id_col = str_col_opt(batch, "volume_id");
        let source_hash_col = str_col_opt(batch, "source_hash");
        // P13.5 Phase 8 batch — surface translation alongside the
        // existing per-doc text columns.  See record_batches_to_search_results
        // for the longer doc comment on null-tolerance for pre-v100 rows.
        let text_translated_col = str_col_opt(batch, "text_translated");
        let text_translated_lang_col = str_col_opt(batch, "text_translated_lang");

        for i in 0..n {
            let doc_id = str_val(doc_id_col, i);
            let score = *score_map.get(&doc_id).unwrap_or(&0.0);

            let full_text = full_text_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .unwrap_or("");
            let snippet = full_text.chars().take(400).collect::<String>();

            let volume_id = str_col_val_opt(&volume_id_col, i).or_else(|| {
                metadata_col
                    .as_ref()
                    .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                    .and_then(parse_volume_id_from_metadata)
            });

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
                metadata_json: str_col_val_opt(&metadata_col, i),
                catalog_source: None,
                volume_id,
                indexed_at: indexed_at_col.map(|c| c.value(i)).unwrap_or(0),
                source_hash: str_col_val_opt(&source_hash_col, i).unwrap_or_default(),
                text_translated: str_col_val_opt(&text_translated_col, i),
                text_translated_lang: str_col_val_opt(&text_translated_lang_col, i),
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

    // parent_dir (P9 step 3 — scalar-indexed for folder-prefix filter)
    let parent_dirs: StringArray = chunks.iter().map(|c| c.parent_dir.as_deref()).collect();
    // volume_id (P9 step 7 — scalar-indexed for volume-availability filter)
    let volume_ids: StringArray = chunks.iter().map(|c| c.volume_id.as_deref()).collect();
    // P13.5 Phase 8 batch — translated text + its target language.
    let text_translateds: StringArray = chunks.iter().map(|c| c.text_translated.as_deref()).collect();
    let text_translated_langs: StringArray = chunks.iter().map(|c| c.text_translated_lang.as_deref()).collect();
    // P13.6 Step 7 — audio L2 metadata columns added by migration v101.
    // Five nullable columns; non-audio rows pass through as nulls.
    let audio_duration_seconds: arrow_array::Float64Array =
        chunks.iter().map(|c| c.audio_duration_seconds).collect();
    let audio_codecs: StringArray = chunks.iter().map(|c| c.audio_codec.as_deref()).collect();
    let audio_sample_rate_hzs: arrow_array::Int32Array =
        chunks.iter().map(|c| c.audio_sample_rate_hz).collect();
    let audio_channelss: arrow_array::Int32Array =
        chunks.iter().map(|c| c.audio_channels).collect();
    let audio_bitrate_kbpss: arrow_array::Int32Array =
        chunks.iter().map(|c| c.audio_bitrate_kbps).collect();
    // P13.6 Step 9 — image L2 (EXIF) columns added by migration v102.
    // Five nullable columns; non-image rows pass through as nulls.
    let image_camera_makes: StringArray =
        chunks.iter().map(|c| c.image_camera_make.as_deref()).collect();
    let image_camera_models: StringArray =
        chunks.iter().map(|c| c.image_camera_model.as_deref()).collect();
    let image_lens_models: StringArray =
        chunks.iter().map(|c| c.image_lens_model.as_deref()).collect();
    let image_taken_at_unixs: arrow_array::Int64Array =
        chunks.iter().map(|c| c.image_taken_at_unix).collect();
    let image_isos: arrow_array::Int32Array =
        chunks.iter().map(|c| c.image_iso).collect();

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
            Arc::new(parent_dirs),
            Arc::new(volume_ids),
            Arc::new(text_translateds),
            Arc::new(text_translated_langs),
            Arc::new(audio_duration_seconds),
            Arc::new(audio_codecs),
            Arc::new(audio_sample_rate_hzs),
            Arc::new(audio_channelss),
            Arc::new(audio_bitrate_kbpss),
            Arc::new(image_camera_makes),
            Arc::new(image_camera_models),
            Arc::new(image_lens_models),
            Arc::new(image_taken_at_unixs),
            Arc::new(image_isos),
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
        let metadata_col = str_col_opt(batch, "metadata_json");
        let indexed_at_col = ts_ms_col_opt(batch, "indexed_at");
        let volume_id_col = str_col_opt(batch, "volume_id");
        // P13/A3: surface source_hash so the images dup view can
        // group by SHA-256 without a second batch fetch.  Optional
        // column lookup so older Lance datasets without the column
        // still load.
        let source_hash_col = str_col_opt(batch, "source_hash");
        // P13.5 Phase 8 batch — surface the translated text + its
        // target language so the search UI can render the
        // alternate-language view inline.  Optional column lookup
        // — Lance datasets predating the AddTextTranslatedColumns
        // migration (v100) don't have these columns; str_col_opt
        // returns None so existing rows just appear untranslated.
        let text_translated_col = str_col_opt(batch, "text_translated");
        let text_translated_lang_col = str_col_opt(batch, "text_translated_lang");

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

            // Prefer the dedicated column; fall back to metadata_json for
            // rows ingested before the column was added.
            let volume_id = str_col_val_opt(&volume_id_col, i).or_else(|| {
                metadata_col
                    .as_ref()
                    .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                    .and_then(parse_volume_id_from_metadata)
            });

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
                metadata_json: str_col_val_opt(&metadata_col, i),
                catalog_source: None,
                volume_id,
                indexed_at: indexed_at_col.map(|c| c.value(i)).unwrap_or(0),
                source_hash: str_col_val_opt(&source_hash_col, i).unwrap_or_default(),
                text_translated: str_col_val_opt(&text_translated_col, i),
                text_translated_lang: str_col_val_opt(&text_translated_lang_col, i),
            });
        }
    }

    Ok(results)
}

/// Tiny hand-parser for `"volume_id":"<id>"` inside `metadata_json`.
/// Mirrors `indexed_mtime_for_uri`'s style — avoids a serde_json dep
/// for a single string field with a known shape. Volume ids are
/// UUIDs / hex serials in practice (no special characters needing
/// unescape), but we tolerate `\"` and `\\` to match the writer in
/// `index/ingest.rs::build_metadata_json`.
/// Tiny hand-parser for `"parent_dir":"<path>"` in `metadata_json`.
/// Used by `sort_rows` in tests.
#[cfg(test)]
fn parse_parent_dir_from_metadata(json: &str) -> Option<String> {
    let key = "\"parent_dir\"";
    let start = json.find(key)?;
    let after = &json[start + key.len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    let end = after.find('"').unwrap_or(after.len());
    Some(after[..end].to_owned())
}

fn parse_volume_id_from_metadata(json: &str) -> Option<String> {
    let key = "\"volume_id\"";
    let start = json.find(key)?;
    let after = &json[start + key.len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    // Read until the next unescaped `"`.
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            other => out.push(other),
        }
    }
    None
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

fn ts_ms_col_opt<'a>(batch: &'a RecordBatch, name: &str) -> Option<&'a TimestampMillisecondArray> {
    batch
        .column_by_name(name)?
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
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

// ── Schema migration helpers ───────────────────────────────────────────────

/// Add the `parent_dir` column to an existing table that predates P9 step 3.
/// No-op if the column is already present (e.g. freshly-created tables).
async fn migrate_add_parent_dir_column(table: &Table) -> Result<()> {
    let schema = table
        .schema()
        .await
        .context("reading table schema for migration")?;
    if schema.field_with_name("parent_dir").is_ok() {
        return Ok(());
    }
    let col_schema = Arc::new(arrow_schema::Schema::new(vec![
        arrow_schema::Field::new("parent_dir", arrow_schema::DataType::Utf8, true),
    ]));
    table
        .add_columns(NewColumnTransform::AllNulls(col_schema), None)
        .await
        .context("adding parent_dir column (schema v2)")?;
    eprintln!("[index] migrated LanceDB table: added parent_dir column");
    Ok(())
}

/// Add the `volume_id` column to an existing table that predates P9 step 7.
/// No-op if the column is already present.
async fn migrate_add_volume_id_column(table: &Table) -> Result<()> {
    let schema = table
        .schema()
        .await
        .context("reading table schema for migration")?;
    if schema.field_with_name("volume_id").is_ok() {
        return Ok(());
    }
    let col_schema = Arc::new(arrow_schema::Schema::new(vec![
        arrow_schema::Field::new("volume_id", arrow_schema::DataType::Utf8, true),
    ]));
    table
        .add_columns(NewColumnTransform::AllNulls(col_schema), None)
        .await
        .context("adding volume_id column (schema v3)")?;
    eprintln!("[index] migrated LanceDB table: added volume_id column");
    Ok(())
}

// ── PLAN P9 query helpers ─────────────────────────────────────────────────

/// Translate a `DocumentFilter` into the predicate string we hand to
/// LanceDB's `only_if`. Returns `None` when the filter is wide open
/// (callers may pass that to `count_rows(None)` to count every row).
///
/// The implicit `chunk_index <= 0` clause is always added so we list one
/// row per document (L1 metadata rows + L3 representative rows) and
/// never return chunk-level rows from the catalog overview.
fn filter_to_sql(f: &super::schema::DocumentFilter) -> Option<String> {
    let mut parts: Vec<String> = vec!["chunk_index <= 0".to_owned()];

    if let Some(prefix) = f.parent_dir_prefix.as_ref().filter(|s| !s.is_empty()) {
        // parent_dir is now a first-class column with a BTree scalar index
        // (P9 step 3). Escape single-quotes for SQL safety; escape LIKE
        // wildcards (%_) so they are treated as literals within the prefix.
        let escaped = prefix
            .replace('\'', "''")
            .replace('%', "\\%")
            .replace('_', "\\_");
        parts.push(format!("parent_dir LIKE '{}%'", escaped));
    }
    if !f.ext.is_empty() {
        let lits: Vec<String> = f
            .ext
            .iter()
            .map(|e| format!("'{}'", e.to_lowercase().replace('\'', "''")))
            .collect();
        parts.push(format!("ext IN ({})", lits.join(", ")));
    }
    if let Some(ymin) = f.year_min {
        parts.push(format!("year >= {}", ymin));
    }
    if let Some(ymax) = f.year_max {
        parts.push(format!("year <= {}", ymax));
    }
    if let Some(lang) = f.language.as_ref().filter(|s| !s.is_empty()) {
        parts.push(format!("language = '{}'", lang.replace('\'', "''")));
    }
    if let Some(level) = f.level {
        match level {
            1 => parts.push("chunk_index = -1".to_owned()),
            // L2 == L1 row that has L2 metadata. Cheap heuristic: any
            // metadata_json key beyond fs_*. We pin to a marker the
            // L2 promotion writer always emits.
            2 => parts.push(
                "chunk_index = -1 AND metadata_json LIKE '%\"l2_extracted\":true%'".to_owned(),
            ),
            3 => parts.push("chunk_index = 0".to_owned()),
            _ => {}
        }
    }
    if let Some(sub) = f.name_substring.as_ref().filter(|s| !s.is_empty()) {
        let pat = sub.replace('\'', "''").replace('%', "\\%");
        // Search both filename and title (LIKE in LanceDB is
        // case-insensitive in 0.26 — verified by the existing
        // metadata_json LIKE patterns elsewhere in the codebase).
        parts.push(format!(
            "(filename LIKE '%{}%' OR title LIKE '%{}%')",
            pat, pat
        ));
    }
    if let Some(ids) = f.doc_ids.as_ref().filter(|v| !v.is_empty()) {
        let lits: Vec<String> = ids
            .iter()
            .map(|d| format!("'{}'", d.replace('\'', "''")))
            .collect();
        parts.push(format!("doc_id IN ({})", lits.join(", ")));
    }
    if let Some(oid) = f.owner_id.as_ref().filter(|s| !s.is_empty()) {
        parts.push(format!("owner_id = '{}'", oid.replace('\'', "''")));
    }
    if let Some(vols) = f.volume_ids.as_ref().filter(|v| !v.is_empty()) {
        // volume_id is now a first-class column (P9 step 7).
        let lits: Vec<String> = vols
            .iter()
            .map(|v| format!("'{}'", v.replace('\'', "''")))
            .collect();
        parts.push(format!("volume_id IN ({})", lits.join(", ")));
    }

    Some(parts.join(" AND "))
}

/// In-process sort over the candidate window. LanceDB 0.26's public
/// query API doesn't expose ORDER BY, so we sort client-side after
/// fetching `[0..offset+limit]`. See `query_documents` for the
/// scaling envelope and the migration path off this implementation.
#[cfg(test)]
fn sort_rows(rows: &mut [SearchResult], sort: super::schema::SortSpec) {
    use super::schema::{SortColumn, SortDir};

    rows.sort_by(|a, b| {
        let c = match sort.column {
            SortColumn::Filename => a.filename.cmp(&b.filename),
            SortColumn::Title => a.title.cmp(&b.title),
            SortColumn::Author => a.author.cmp(&b.author),
            SortColumn::Year => a.year.cmp(&b.year),
            SortColumn::Language => a.language.cmp(&b.language),
            SortColumn::IndexedAt => a.indexed_at.cmp(&b.indexed_at),
            // parent_dir is a real column but SearchResult doesn't expose
            // it yet; parse from metadata_json as a fallback. L1 rows
            // always have it there; L3 rows without it sort last.
            SortColumn::ParentDir => {
                let a_pd = a.metadata_json.as_deref().and_then(parse_parent_dir_from_metadata);
                let b_pd = b.metadata_json.as_deref().and_then(parse_parent_dir_from_metadata);
                a_pd.cmp(&b_pd)
            }
        };
        // Tiebreak on doc_id so the cursor predicate has a stable
        // partner for cross-page consistency.
        let c = c.then_with(|| a.doc_id.cmp(&b.doc_id));
        match sort.direction {
            SortDir::Asc => c,
            SortDir::Desc => c.reverse(),
        }
    });
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

#[cfg(test)]
mod query_documents_tests {
    use super::*;
    use crate::index::schema::{DocumentFilter, SortColumn, SortDir, SortSpec};

    fn mk_result(doc_id: &str, filename: Option<&str>, year: Option<i32>) -> SearchResult {
        SearchResult {
            doc_id: doc_id.to_owned(),
            location_uri: String::new(),
            owner_id: String::new(),
            title: None,
            author: None,
            year,
            filename: filename.map(|s| s.to_owned()),
            ext: None,
            language: None,
            snippet: String::new(),
            score: 0.0,
            chunk_index: 0,
            metadata_json: None,
            catalog_source: None,
            volume_id: None,
            indexed_at: 0,
            source_hash: String::new(),
            text_translated: None,
            text_translated_lang: None,
        }
    }

    #[test]
    fn filter_sql_always_constrains_to_doc_rows() {
        let f = DocumentFilter::default();
        let sql = filter_to_sql(&f).unwrap();
        assert!(sql.contains("chunk_index <= 0"));
    }

    #[test]
    fn filter_sql_combines_extension_and_year_range() {
        let f = DocumentFilter {
            ext: vec!["pdf".to_owned(), "docx".to_owned()],
            year_min: Some(2000),
            year_max: Some(2020),
            ..Default::default()
        };
        let sql = filter_to_sql(&f).unwrap();
        assert!(sql.contains("ext IN ('pdf', 'docx')"));
        assert!(sql.contains("year >= 2000"));
        assert!(sql.contains("year <= 2020"));
    }

    #[test]
    fn filter_sql_level_translates_to_chunk_index_predicate() {
        for (level, expected) in [(1u8, "chunk_index = -1"), (3, "chunk_index = 0")] {
            let f = DocumentFilter {
                level: Some(level),
                ..Default::default()
            };
            let sql = filter_to_sql(&f).unwrap();
            assert!(
                sql.contains(expected),
                "level {level} should produce {expected:?}: {sql}"
            );
        }
    }

    #[test]
    fn filter_sql_escapes_single_quotes_in_owner_id() {
        let f = DocumentFilter {
            owner_id: Some("o'malley".to_owned()),
            ..Default::default()
        };
        let sql = filter_to_sql(&f).unwrap();
        assert!(sql.contains("owner_id = 'o''malley'"));
    }

    #[test]
    fn filter_sql_parent_dir_uses_column_not_json_like() {
        let f = DocumentFilter {
            parent_dir_prefix: Some("/Users/alice/Documents".to_owned()),
            ..Default::default()
        };
        let sql = filter_to_sql(&f).unwrap();
        // Must use the real column predicate (P9 step 3).
        assert!(
            sql.contains("parent_dir LIKE '/Users/alice/Documents%'"),
            "expected column predicate, got: {sql}"
        );
        // Must NOT fall back to the old JSON LIKE hack.
        assert!(
            !sql.contains("metadata_json LIKE"),
            "must not use metadata_json LIKE, got: {sql}"
        );
    }

    #[test]
    fn filter_sql_parent_dir_escapes_like_wildcards() {
        let f = DocumentFilter {
            parent_dir_prefix: Some("/weird%path_here".to_owned()),
            ..Default::default()
        };
        let sql = filter_to_sql(&f).unwrap();
        assert!(
            sql.contains(r"parent_dir LIKE '/weird\%path\_here%'"),
            "LIKE wildcards inside prefix must be escaped: {sql}"
        );
    }

    #[test]
    fn sort_rows_orders_year_descending_with_doc_id_tiebreak() {
        let mut rows = vec![
            mk_result("a", Some("a.pdf"), Some(2020)),
            mk_result("b", Some("b.pdf"), Some(2024)),
            mk_result("c", Some("c.pdf"), Some(2024)),
            mk_result("d", Some("d.pdf"), Some(2010)),
        ];
        sort_rows(
            &mut rows,
            SortSpec {
                column: SortColumn::Year,
                direction: SortDir::Desc,
            },
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.doc_id.as_str()).collect();
        // 2024 group sorted by doc_id descending (`c` before `b`), then 2020,
        // then 2010.
        assert_eq!(ids, vec!["c", "b", "a", "d"]);
    }
}

#[cfg(test)]
mod volume_id_parse_tests {
    use super::parse_volume_id_from_metadata;

    #[test]
    fn parses_volume_id_alone() {
        assert_eq!(
            parse_volume_id_from_metadata(r#"{"volume_id":"ABCD-1234"}"#),
            Some("ABCD-1234".to_owned())
        );
    }

    #[test]
    fn parses_volume_id_after_mtime() {
        // Same packing order the writer in build_metadata_json uses.
        assert_eq!(
            parse_volume_id_from_metadata(
                r#"{"mtime_unix":1700000000,"volume_id":"12345678-1234-1234-1234-123456789ABC"}"#
            ),
            Some("12345678-1234-1234-1234-123456789ABC".to_owned())
        );
    }

    #[test]
    fn missing_volume_id_returns_none() {
        assert_eq!(
            parse_volume_id_from_metadata(r#"{"mtime_unix":1700000000}"#),
            None
        );
    }

    #[test]
    fn empty_metadata_returns_none() {
        assert_eq!(parse_volume_id_from_metadata(""), None);
    }

    #[test]
    fn handles_escaped_quote_inside_id() {
        // Defensive — volume ids in practice are UUIDs / hex, but the
        // writer escapes `"` as `\"` for safety. The reader has to
        // honour that.
        assert_eq!(
            parse_volume_id_from_metadata(r#"{"volume_id":"weird\"id"}"#),
            Some(r#"weird"id"#.to_owned())
        );
    }
}
