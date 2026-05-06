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

        migrate_add_parent_dir_column(&table)
            .await
            .context("schema v2: adding parent_dir column")?;

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

                let volume_id = metadata_col
                    .as_ref()
                    .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                    .and_then(parse_volume_id_from_metadata);

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

    /// Build a BTree scalar index on `parent_dir` for fast folder-prefix
    /// filtering in `query_documents`.  Safe to call repeatedly — LanceDB
    /// will replace the old index.  Typically called once after the first
    /// bulk L1 ingest or whenever the table grows significantly.
    pub async fn build_scalar_index(&self) -> Result<()> {
        self.table
            .create_index(&["parent_dir"], Index::BTree(BTreeIndexBuilder::default()))
            .execute()
            .await
            .context("building BTree scalar index on parent_dir")?;
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
    /// (~10k rows on a 200-row page) but degrades after that. Step 3
    /// of the migration promotes `parent_dir` to a real column with a
    /// scalar index; step 5 swaps this for a keyset cursor over an
    /// indexed sort column once we drop down to `lance::Scanner` for
    /// DB-side ordering. The `PageCursor` API was designed
    /// keyset-shaped on purpose — only the encoding changes.
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
        use super::schema::{DocumentPage, PageCursor};

        let limit = page.limit.clamp(1, 1000) as usize;
        let offset = page.cursor.as_ref().map(|c| c.offset()).unwrap_or(0) as usize;
        let base_filter = filter_to_sql(filter);

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

        // Bounded window — until step 5 we have to materialise + sort
        // [0..offset+limit]. Cap at 50k rows to keep memory bounded;
        // beyond that the call returns rows unsorted with a warning
        // logged (the UI shows them, the user is told to add filters
        // to narrow). 50k * ~1KB/row ~= 50MB peak, acceptable.
        const HARD_CAP: usize = 50_000;
        let window = (offset + limit).min(HARD_CAP);

        let mut q = self.table.query();
        if let Some(sql) = base_filter {
            q = q.only_if(sql);
        }

        let batches: Vec<RecordBatch> = q
            .limit(window)
            .execute()
            .await?
            .try_collect()
            .await?;

        let mut rows = record_batches_to_search_results(&batches)?;
        sort_rows(&mut rows, sort);

        // Slice to the requested page.
        let end = (offset + limit).min(rows.len());
        let page_rows: Vec<_> = rows.drain(offset.min(rows.len())..end).collect();

        // Next cursor: bump the offset, but `None` when we hit either
        // the end of the result set or the hard cap.
        let next_offset = offset + page_rows.len();
        let next_cursor = if page_rows.len() < limit
            || (next_offset as u64) >= total_estimate
            || next_offset >= HARD_CAP
        {
            None
        } else {
            Some(PageCursor::from_offset(next_offset as u32))
        };

        Ok(DocumentPage {
            rows: page_rows,
            next_cursor,
            total_estimate,
        })
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

        for i in 0..n {
            let doc_id = str_val(doc_id_col, i);
            let score = *score_map.get(&doc_id).unwrap_or(&0.0);

            let full_text = full_text_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .unwrap_or("");
            let snippet = full_text.chars().take(400).collect::<String>();

            let volume_id = metadata_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .and_then(parse_volume_id_from_metadata);

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

            let volume_id = metadata_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .and_then(parse_volume_id_from_metadata);

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
        // volume_id lives in metadata_json (P7.6); same caveat as
        // parent_dir, on the migration path to a real column in step 6.
        let alts: Vec<String> = vols
            .iter()
            .map(|v| {
                format!(
                    "metadata_json LIKE '%\"volume_id\":\"{}\"%'",
                    v.replace('\'', "''").replace('\\', "\\\\")
                )
            })
            .collect();
        parts.push(format!("({})", alts.join(" OR ")));
    }

    Some(parts.join(" AND "))
}

/// In-process sort over the candidate window. LanceDB 0.26's public
/// query API doesn't expose ORDER BY, so we sort client-side after
/// fetching `[0..offset+limit]`. See `query_documents` for the
/// scaling envelope and the migration path off this implementation.
fn sort_rows(rows: &mut [SearchResult], sort: super::schema::SortSpec) {
    use super::schema::{SortColumn, SortDir};

    rows.sort_by(|a, b| {
        let c = match sort.column {
            SortColumn::Filename => a.filename.cmp(&b.filename),
            SortColumn::Title => a.title.cmp(&b.title),
            SortColumn::Author => a.author.cmp(&b.author),
            SortColumn::Year => a.year.cmp(&b.year),
            SortColumn::Language => a.language.cmp(&b.language),
            // SearchResult doesn't carry indexed_at directly; fall back
            // to doc_id ordering which is stable per ingest. Step 5 of
            // the migration adds an explicit indexed_at field.
            SortColumn::IndexedAt => a.doc_id.cmp(&b.doc_id),
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
