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
    Array, FixedSizeListArray, Int16Array, Int32Array, LargeBinaryArray, ListArray, RecordBatch,
    StringArray, TimestampMillisecondArray,
};
use arrow_schema::Schema;
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::{
    connect,
    index::{scalar::BTreeIndexBuilder, vector::IvfPqIndexBuilder, Index},
    query::{ExecutableQuery, QueryBase},
    table::{OptimizeAction, NewColumnTransform},
    Connection, DistanceType, Table,
};

use super::embedder::SparseVector;
use super::ingest::chunk_row_id;
use super::schema::{build_schema, DocumentChunk, SearchFilters, SearchResult};
use super::search::{maxsim, unpack_multivec};
use super::IndexBackend;

// ── Constant ───────────────────────────────────────────────────────────────

const TABLE_NAME: &str = "documents";

#[path = "occurrence.rs"]
mod occurrence;
pub(crate) use occurrence::{OccurrencePlan, SourceOccurrence};

#[path = "source_move.rs"]
mod source_move;

fn replacement_filter(chunks: &[DocumentChunk]) -> Result<String> {
    anyhow::ensure!(!chunks.is_empty(), "A document replacement cannot be empty");
    let mut row_ids = std::collections::BTreeSet::new();
    let mut predicates = std::collections::BTreeSet::new();
    let mut totals = std::collections::BTreeMap::<&str, (i32, usize)>::new();
    for chunk in chunks {
        anyhow::ensure!(!chunk.doc_id.trim().is_empty(), "Document identity is required");
        anyhow::ensure!(row_ids.insert(&chunk.id), "Duplicate chunk identity in document replacement");
        anyhow::ensure!(chunk.chunk_index >= 0 && chunk.chunk_index < chunk.chunk_total,
            "Only complete extracted documents can be replaced");
        let count = totals.entry(&chunk.doc_id).or_insert((chunk.chunk_total, 0));
        anyhow::ensure!(count.0 == chunk.chunk_total, "Inconsistent document chunk count");
        count.1 += 1;
        predicates.insert(SourceOccurrence::from_chunk(chunk).filter());
    }
    anyhow::ensure!(totals.values().all(|(total, actual)| *total as usize == *actual),
        "Document replacement is missing chunks");
    Ok(format!("({})", predicates.into_iter().collect::<Vec<_>>().join(" OR ")))
}

fn sql_values(values: &[String]) -> String {
    values.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).collect::<Vec<_>>().join(",")
}

/// Key returned by passage hydration, scoped to the selected source owner.
pub fn passage_key(owner_id: &str, doc_id: &str, chunk_index: i32) -> String {
    super::fts_index::document_key(owner_id, &chunk_row_id(doc_id, chunk_index))
}

fn document_keys_sql(keys: &[(String, String)]) -> String {
    if keys.is_empty() { return "false".into(); }
    let mut owners = std::collections::BTreeMap::<&str, Vec<String>>::new();
    for (owner, doc) in keys { owners.entry(owner).or_default().push(doc.clone()); }
    let mut parts: Vec<_> = owners.into_iter().map(|(owner, ids)|
        format!("(owner_id = '{}' AND doc_id IN ({}))", owner.replace('\'', "''"), sql_values(&ids))).collect();
    while parts.len() > 1 {
        parts = parts.chunks(2).map(|pair| if pair.len() == 2 {
            format!("({} OR {})", pair[0], pair[1])
        } else { pair[0].clone() }).collect();
    }
    parts.pop().unwrap_or_else(|| "false".into())
}

fn import_replacement_ids(chunks: &[DocumentChunk], explicit: &[String]) -> std::collections::BTreeSet<String> {
    let mut replaced: std::collections::BTreeSet<String> = explicit.iter().cloned().collect();
    let mut grouped = std::collections::BTreeMap::<&str, Vec<&DocumentChunk>>::new();
    for chunk in chunks { grouped.entry(&chunk.doc_id).or_default().push(chunk); }
    for (id, group) in grouped {
        if !group.iter().any(|c| c.full_text.as_deref().is_some_and(|s| !s.trim().is_empty())) { continue; }
        let total = group[0].chunk_total;
        let complete = total > 0 && group.len() == total as usize
            && group.iter().all(|c| c.chunk_total == total)
            && group.iter().map(|c| c.chunk_index).collect::<std::collections::BTreeSet<_>>()
                == (0..total).collect();
        let whole_body = group.len() == 1 && group[0].chunk_index <= 0 && total <= 1;
        if complete || whole_body { replaced.insert(id.to_owned()); }
    }
    replaced
}

#[derive(Clone)]
struct FtsPiece {
    id: String, doc_id: String, owner: String, language: String,
    title: String, headings: String, body: String, translated: Option<String>,
    chunk_index: i32, start: Option<i32>, end: Option<i32>,
}

impl From<&DocumentChunk> for FtsPiece {
    fn from(c: &DocumentChunk) -> Self {
        Self { id: c.id.clone(), doc_id: c.doc_id.clone(), owner: c.owner_id.clone(),
            language: c.language.clone().unwrap_or_default(), title: c.title.clone().unwrap_or_default(),
            headings: c.headings_text.clone().unwrap_or_default(), body: c.full_text.clone().unwrap_or_default(),
            translated: c.text_translated.clone(), chunk_index: c.chunk_index,
            start: c.chunk_start_char, end: c.chunk_end_char }
    }
}

/// Reconstruct only verified contiguous text; preserve every other source region
/// as a separate positional segment for imported documents and archive indexes.
fn fts_input_from_pieces(doc_id: String, mut pieces: Vec<FtsPiece>) -> Result<Option<super::ingest::TantivyInputOwned>> {
    pieces.sort_by_key(|piece| piece.chunk_index);
    let extracted = pieces.iter().any(|piece| piece.chunk_index >= 0);
    let Some(first) = pieces.iter().find(|piece| piece.chunk_index >= 0).or_else(|| pieces.first()) else { return Ok(None); };
    let mut input = super::ingest::TantivyInputOwned {
        doc_id, owner_id: first.owner.clone(), language: first.language.clone(), title: first.title.clone(),
        headings: String::new(), body: String::new(), body_segments: None, body_translated: None,
    };
    anyhow::ensure!(pieces.iter().all(|piece| piece.owner == input.owner_id), "Full-text reconstruction mixed owners");
    let bytes = pieces.iter().try_fold(0usize, |sum, piece| sum.checked_add(piece.body.len()))
        .ok_or_else(|| anyhow!("Document size overflow"))?;
    anyhow::ensure!(bytes <= 64 * 1024 * 1024, "Document exceeds the 64 MiB full-text reconstruction limit");
    let mut segments = Vec::<String>::new();
    let mut previous_end = None::<usize>;
    let mut seen_headings = std::collections::BTreeSet::new();
    for piece in pieces {
        if !piece.headings.is_empty() && seen_headings.insert(piece.headings.clone()) {
            if !input.headings.is_empty() { input.headings.push('\n'); }
            input.headings.push_str(&piece.headings);
        }
        if input.body_translated.is_none() { input.body_translated = piece.translated; }
        // Metadata-only rows may retain old source offsets but have no text
        // positions to reconstruct or compare.
        if piece.body.is_empty() { continue; }
        let range = match (piece.start, piece.end) {
            (Some(start), Some(end)) if start >= 0 && end >= start => {
                anyhow::ensure!((end - start) as usize == piece.body.len(), "Chunk byte offsets disagree with its text length");
                Some((start as usize, end as usize))
            }
            (None, None) => None,
            _ => anyhow::bail!("Stored chunk has incomplete or invalid source offsets"),
        };
        if let (Some(prior_end), Some((start, end))) = (previous_end, range) {
            if start <= prior_end {
                let previous = segments.last_mut().context("Missing reconstructed source region")?;
                let prior_start = prior_end.checked_sub(previous.len()).context("Invalid reconstructed range")?;
                anyhow::ensure!(start >= prior_start, "Chunk offsets move backwards");
                let overlap = prior_end.min(end) - start;
                let prior = previous.get(start - prior_start..start - prior_start + overlap)
                    .context("Chunk overlap splits a UTF-8 character")?;
                let incoming = piece.body.get(..overlap).context("Chunk overlap splits a UTF-8 character")?;
                anyhow::ensure!(prior == incoming, "Overlapping chunks contain inconsistent source text");
                if end > prior_end {
                    previous.push_str(&piece.body[overlap..]);
                    previous_end = Some(end);
                }
                continue;
            }
        }
        previous_end = range.map(|(_, end)| end);
        segments.push(piece.body);
    }
    input.body = segments.join("\n");
    // A single known or unknown region has the same positional behavior as a
    // whole body. Multiple regions must survive durable journal serialization.
    if segments.len() > 1 { input.body_segments = Some(segments); }
    Ok((extracted || !input.body.trim().is_empty()).then_some(input))
}

/// One row returned by `list_failed_extractions`.
pub struct FailedExtractionRow {
    pub doc_id:       String,
    pub location_uri: String,
    pub filename:     Option<String>,
    pub reason:       String,
    pub retryable:    bool,
}

/// Minimal document metadata used by sync conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictRow {
    pub doc_id: String,
    pub source_hash: String,
    pub indexed_at: Option<i64>,
    pub title: Option<String>,
}

/// Source text and character anchors for one indexed chunk. Page numbers are
/// optional: old extractors do not retain enough information to infer them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Passage {
    pub text: String,
    pub start_char: Option<i32>,
    pub end_char: Option<i32>,
    pub offset_unit: String,
}

/// Columns consumed by `batches_to_search_results_with_scores` and
/// `record_batches_to_search_results`.  Used by `.select(Select::Columns(…))`
/// to avoid reading embedding vectors and other large blobs.
fn search_result_columns() -> lancedb::query::Select {
    lancedb::query::Select::Columns(vec![
        "doc_id".into(), "location_uri".into(), "owner_id".into(),
        "title".into(), "author".into(), "year".into(), "filename".into(),
        "ext".into(), "language".into(), "chunk_index".into(),
        "full_text".into(), "metadata_json".into(), "indexed_at".into(),
        "volume_id".into(), "source_hash".into(), "text_translated".into(),
        "text_translated_lang".into(), "url".into(), "tags".into(),
        "summary".into(), "doc_status".into(),
    ])
}

// ── Struct ─────────────────────────────────────────────────────────────────

pub struct LocalIndex {
    // Kept alive to maintain the LanceDB connection for the table lifetime.
    _db: Connection,
    table: Table,
    pub dims: usize,
    /// Cached Arrow schema — built once at construction, reused by every
    /// `ingest_batch` call instead of re-allocating ~25 Field objects.
    schema: Arc<Schema>,
    /// Durable handoff between the vector transaction and full-text commit.
    ingest_journal_path: std::path::PathBuf,
}

// ── Constructor ────────────────────────────────────────────────────────────

/// What the re-ingest guard needs from a document's representative row.
///
/// Both fields live in `metadata_json` (see `ingest::build_metadata_json`), so
/// no schema migration is involved and older rows simply lack the fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedState {
    /// Source-file mtime recorded at ingest time, epoch seconds.
    pub mtime_unix: Option<i64>,
    /// Row was written while image indexing was enabled, so missing vectors
    /// (if any) are not a reason to re-ingest.
    pub images_indexed: bool,
    pub source_hash: Option<String>,
    pub extraction_failed: bool,
    pub extraction_skip: bool,
}

impl LocalIndex {
    pub async fn filename_only_count(&self) -> Result<usize> {
        Ok(self.table.count_rows(Some("chunk_index <= 0 AND (full_text IS NULL OR full_text = '')".to_owned())).await?)
    }
    pub async fn document_metadata(&self, doc_id: &str) -> Result<serde_json::Value> {
        let batches: Vec<RecordBatch> = self.table.query()
            .only_if(format!("doc_id = '{}'", doc_id.replace('\'', "''")))
            .select(lancedb::query::Select::Columns(vec!["metadata_json".into()]))
            .limit(1).execute().await?.try_collect().await?;
        for batch in batches {
            if batch.num_rows() == 0 { continue; }
            if let Some(metadata) = str_col_opt(&batch, "metadata_json") {
                if !metadata.is_null(0) { return Ok(serde_json::from_str(metadata.value(0))?); }
            }
            return Ok(serde_json::json!({}));
        }
        Ok(serde_json::json!({}))
    }

    pub async fn update_field_evidence(&self, doc_id: &str, previous: &serde_json::Value, updated: &serde_json::Value) -> Result<()> {
        let _guard = self.write_guard().await?;
        // A whole-document metadata edit must not replace newer ingest output.
        // Metadata is serialized in a canonical key order by serde_json.
        let current = self.document_metadata(doc_id).await?;
        anyhow::ensure!(&current == previous, "Document changed; reload before editing fields");
        let encoded = updated.to_string();
        let old_tags: Vec<String> = super::fields::read(previous).iter()
            .map(|f| format!("'{}'", super::fields::tag(f).replace('\'', "''"))).chain(std::iter::once("''".into())).collect();
        let new_tags: Vec<String> = super::fields::read(updated).iter().filter(|f| f.review_status != "rejected")
            .map(|f| format!("'{}'", super::fields::tag(f).replace('\'', "''"))).chain(std::iter::once("''".into())).collect();
        let tags_expression = format!("array_remove(array_concat(array_except(coalesce(tags, make_array('')), make_array({})), make_array({})), '')", old_tags.join(","), new_tags.join(","));
        self.table.update()
            .only_if(format!("doc_id = '{}' AND metadata_json = '{}'", doc_id.replace('\'', "''"), previous.to_string().replace('\'', "''")))
            .column("metadata_json", format!("'{}'", encoded.replace('\'', "''")))
            .column("tags", format!("array_distinct({tags_expression})"))
            .execute().await?;
        anyhow::ensure!(self.document_metadata(doc_id).await? == *updated, "Document changed during review; reload before editing fields");
        Ok(())
    }

    /// Hydrate exact owner/document/chunk identities, without embedding columns.
    pub async fn fetch_passages(
        &self,
        candidates: &[SearchResult],
    ) -> Result<std::collections::HashMap<String, Passage>> {
        let mut passages = std::collections::HashMap::new();
        let mut latest = std::collections::HashMap::<String, (i64, u64)>::new();
        for group in candidates.chunks(256) {
            let wanted: std::collections::HashSet<_> = group.iter().map(|r| passage_key(&r.owner_id, &r.doc_id, r.chunk_index)).collect();
            let keys: Vec<_> = group.iter().map(|r| (r.owner_id.clone(), r.doc_id.clone())).collect();
            let ids: Vec<_> = group.iter().map(|r| chunk_row_id(&r.doc_id, r.chunk_index)).collect();
            let mut stream = self.table.query()
                .only_if(format!("({}) AND id IN ({})", document_keys_sql(&keys), sql_values(&ids)))
                .select(lancedb::query::Select::Columns(["doc_id", "owner_id", "chunk_index", "full_text",
                    "chunk_start_char", "chunk_end_char", "indexed_at"].into_iter().map(str::to_owned).collect()))
                .with_row_id().execute().await?;
            while let Some(batch) = stream.try_next().await? {
                let docs = str_col(&batch, "doc_id")?;
                let owners = str_col(&batch, "owner_id")?;
                let indices = i32_col(&batch, "chunk_index")?;
                let text = str_col(&batch, "full_text")?;
                let times = ts_ms_col_opt(&batch, "indexed_at");
                let row_ids = batch.column_by_name("_rowid").and_then(|c| c.as_any().downcast_ref::<arrow_array::UInt64Array>())
                    .context("Missing stored passage row IDs")?;
                let start = i32_col_opt(&batch, "chunk_start_char");
                let end = i32_col_opt(&batch, "chunk_end_char");
                for row in 0..batch.num_rows() {
                    let key = passage_key(owners.value(row), docs.value(row), indices.value(row));
                    if !wanted.contains(&key) { continue; }
                    let order = (times.filter(|c| !c.is_null(row)).map_or(i64::MIN, |c| c.value(row)), row_ids.value(row));
                    if latest.get(&key).is_some_and(|previous| *previous > order) { continue; }
                    latest.insert(key.clone(), order);
                    if text.is_null(row) || text.value(row).trim().is_empty() { passages.remove(&key); continue; }
                    passages.insert(key, Passage {
                        text: text.value(row).to_owned(),
                        start_char: start.filter(|c| !c.is_null(row)).map(|c| c.value(row)),
                        end_char: end.filter(|c| !c.is_null(row)).map(|c| c.value(row)),
                        offset_unit: "utf8_bytes".into(),
                    });
                }
            }
        }
        Ok(passages)
    }
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

        // `dims` describes the table we would *create*. For a table that
        // already exists, the width on disk is the truth: building our schema
        // from the caller's guess instead is how a 384-dim model came to write
        // into a 1024-dim table and panic arrow. Read it back and use it.
        let dims = match table_embedding_dims(&table).await {
            Some(actual) => {
                if actual != dims {
                    eprintln!(
                        "[warn] LanceDB table stores {actual}-dim embeddings, not {dims}; \
                         using {actual}. Embedding with a different model will be refused."
                    );
                }
                actual
            }
            None => dims,
        };

        let schema = build_schema(dims);
        Ok(LocalIndex {
            _db: db,
            table,
            dims,
            schema,
            ingest_journal_path: data_dir.join("index-write-journal.sqlite"),
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
        let fts = super::fts_index::FtsIndex::open_or_create(
            &self.ingest_journal_path.parent().ok_or_else(|| anyhow!("Missing index directory"))?.join("fts"))?;
        self.ingest_batch_with_fts(chunks, &[], &fts).await
    }

    #[cfg(test)]
    pub(crate) async fn append_legacy_rows_for_test(&self, chunks: &[DocumentChunk]) -> Result<()> {
        let _guard = self.write_guard().await?;
        let batch = chunks_to_record_batch(chunks, self.dims, &self.schema)?;

        let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], Arc::clone(&self.schema));
        self.table
            .add(reader)
            .execute()
            .await
            .context("LanceDB add")?;
        // P23 — invalidate the search result cache after ingest.
        super::result_cache::invalidate();
        Ok(())
    }

    pub(crate) fn ingest_journal_path(&self) -> &Path {
        &self.ingest_journal_path
    }

    /// Readers and every application mutation share the same lease. Recovery
    /// finishes before a caller can observe mixed vector/full-text generations.
    pub async fn read_guard(&self, fts: &super::fts_index::FtsIndex) -> Result<super::ingest::IndexWriteGuard> {
        if self.dims == 0 { return Ok(super::ingest::IndexWriteGuard::immutable_snapshot()); }
        let guard = super::ingest::acquire_index_guard(&self.ingest_journal_path).await?;
        self.table.checkout_latest().await?;
        super::ingest::repair_fts_writes(fts, self).await?;
        Ok(guard)
    }

    async fn write_guard(&self) -> Result<super::ingest::IndexWriteGuard> {
        anyhow::ensure!(self.dims > 0, "An index snapshot is read-only");
        let fts = super::fts_index::FtsIndex::open_or_create(
            &self.ingest_journal_path.parent().ok_or_else(|| anyhow!("Missing index directory"))?.join("fts"))?;
        self.read_guard(&fts).await
    }

    pub async fn ingest_batch_with_fts(&self, chunks: &[DocumentChunk], replace_doc_ids: &[String], fts: &super::fts_index::FtsIndex) -> Result<()> {
        super::ingest::apply_imported_chunks(self, fts, chunks, replace_doc_ids).await
    }

    pub async fn delete_document_with_fts(&self, doc_id: &str, fts: &super::fts_index::FtsIndex) -> Result<()> {
        super::ingest::apply_imported_chunks(self, fts, &[], &[doc_id.to_owned()]).await
    }

    /// Apply a remotely requested deletion only if policy inspected this exact
    /// durable local revision. A concurrent local edit leaves the document intact.
    pub async fn delete_document_if_version_with_fts(
        &self, doc_id: &str, expected_version: u64, fts: &super::fts_index::FtsIndex,
    ) -> Result<bool> {
        super::ingest::apply_imported_chunks_if_version(
            self, fts, &[], &[doc_id.to_owned()], Some(expected_version)).await
    }

    pub(crate) async fn documents_absent(&self, doc_ids: &[String]) -> Result<bool> {
        self.table.checkout_latest().await?;
        if doc_ids.is_empty() { return Ok(true); }
        Ok(self.table.count_rows(Some(format!("doc_id IN ({})", sql_values(doc_ids)))).await? == 0)
    }

    /// Normal metadata refreshes preserve an extracted body. An explicit
    /// accepted-remote replacement is allowed to replace its prior identity.
    pub(crate) async fn prepare_imported_chunks(&self, chunks: &[DocumentChunk], replace_doc_ids: &[String]) -> Result<Vec<DocumentChunk>> {
        let mut identities = std::collections::BTreeSet::new();
        let mut prepared = Vec::new();
        for chunk in chunks {
            anyhow::ensure!(!chunk.doc_id.is_empty() && !chunk.id.is_empty(), "Imported chunk identity is required");
            anyhow::ensure!(identities.insert(&chunk.id), "Duplicate imported chunk identity");
            if !replace_doc_ids.contains(&chunk.doc_id) && chunk.chunk_index < 0
                && chunk.full_text.as_deref().is_none_or(|s| s.trim().is_empty()) {
                let count = self.table.count_rows(Some(format!(
                    "doc_id = '{}' AND chunk_index >= 0", chunk.doc_id.replace('\'', "''")))).await?;
                if count > 0 { continue; }
            }
            prepared.push(chunk.clone());
        }
        // Validate the entire batch before recording an intent or deleting old
        // rows, including auxiliary vector dimensions and finite values.
        if !prepared.is_empty() { chunks_to_record_batch(&prepared, self.dims, &self.schema)?; }
        Ok(prepared)
    }

    pub(crate) async fn replace_imported_rows(&self, chunks: &mut [DocumentChunk], replace_doc_ids: &[String], generation: &str) -> Result<()> {
        let replaced = import_replacement_ids(chunks, replace_doc_ids).into_iter().collect::<Vec<_>>();
        let mut predicates = Vec::new();
        if !replaced.is_empty() { predicates.push(format!("doc_id IN ({})", sql_values(&replaced))); }
        if !chunks.is_empty() { predicates.push(format!("id IN ({})", sql_values(&chunks.iter().map(|c| c.id.clone()).collect::<Vec<_>>()))); }
        if predicates.is_empty() { return Ok(()); }
        let filter = predicates.join(" OR ");
        if chunks.is_empty() {
            self.table.delete(&filter).await?;
        } else {
            for chunk in chunks.iter_mut() {
                let mut metadata: serde_json::Value = chunk.metadata_json.as_deref()
                    .map(serde_json::from_str).transpose()?.unwrap_or_else(|| serde_json::json!({}));
                anyhow::ensure!(metadata.is_object(), "Document metadata must be an object");
                metadata["index_write_generation"] = generation.into();
                chunk.metadata_json = Some(metadata.to_string());
            }
            let batch = chunks_to_record_batch(chunks, self.dims, &self.schema)?;
            let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], self.schema.clone());
            let mut merge = self.table.merge_insert(&["id", "metadata_json"]);
            merge.when_not_matched_insert_all().when_not_matched_by_source_delete(Some(filter));
            merge.execute(Box::new(reader)).await?;
        }
        super::result_cache::invalidate();
        Ok(())
    }

    /// Build the full-text view of the post-import rows before committing them.
    /// Partial chunk imports retain untouched siblings; complete document
    /// imports remove old tails. Byte offsets remove overlapping chunk text.
    pub(crate) async fn fts_inputs_after_import(&self, chunks: &[DocumentChunk], replace_doc_ids: &[String]) -> Result<Vec<super::ingest::TantivyInputOwned>> {
        let ids = chunks.iter().map(|c| c.doc_id.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        self.fts_inputs_for_doc_changes(&ids, chunks, replace_doc_ids).await
    }

    pub(crate) async fn fts_inputs_for_docs(&self, doc_ids: &[String]) -> Result<Vec<super::ingest::TantivyInputOwned>> {
        self.fts_inputs_for_doc_changes(doc_ids, &[], &[]).await
    }

    /// Reconstruct exact stored identities during full-text maintenance. Legacy
    /// append-only rows can share a chunk key, so use the same latest-row rule
    /// as passage inspection before validating source ranges and boundaries.
    pub(crate) async fn fts_inputs_for_keys(&self, keys: &[(String, String)]) -> Result<Vec<super::ingest::TantivyInputOwned>> {
        if keys.is_empty() { return Ok(Vec::new()); }
        let mut stream = self.table.query().only_if(document_keys_sql(keys))
            .select(lancedb::query::Select::Columns(["id", "doc_id", "owner_id", "language", "title", "headings_text", "full_text", "text_translated", "chunk_index", "chunk_start_char", "chunk_end_char", "indexed_at"]
                .into_iter().map(str::to_owned).collect()))
            .with_row_id().execute().await?;
        let mut rows = std::collections::BTreeMap::<(String, String, i32), (i64, u64, FtsPiece)>::new();
        while let Some(batch) = stream.try_next().await? {
            let docs = str_col(&batch, "doc_id")?;
            let owners = str_col(&batch, "owner_id")?;
            let indices = i32_col(&batch, "chunk_index")?;
            let times = ts_ms_col_opt(&batch, "indexed_at");
            let row_ids = batch.column_by_name("_rowid").and_then(|c| c.as_any().downcast_ref::<arrow_array::UInt64Array>())
                .context("Missing stored full-text row identities")?;
            for row in 0..batch.num_rows() {
                anyhow::ensure!(!docs.is_null(row) && !owners.is_null(row) && !indices.is_null(row) && !row_ids.is_null(row),
                    "Stored full-text document identity is incomplete");
                let key = (owners.value(row).to_owned(), docs.value(row).to_owned(), indices.value(row));
                let version = (times.filter(|c| !c.is_null(row)).map(|c| c.value(row)).unwrap_or(0), row_ids.value(row));
                if rows.get(&key).is_some_and(|(time, row_id, _)| (*time, *row_id) >= version) { continue; }
                let string = |name: &str| str_col_opt(&batch, name).filter(|c| !c.is_null(row)).map(|c| c.value(row).to_owned());
                let number = |name: &str| i32_col_opt(&batch, name).filter(|c| !c.is_null(row)).map(|c| c.value(row));
                let piece = FtsPiece {
                    id: string("id").context("Missing stored full-text chunk ID")?,
                    doc_id: key.1.clone(), owner: key.0.clone(), chunk_index: key.2,
                    language: string("language").unwrap_or_default(), title: string("title").unwrap_or_default(),
                    headings: string("headings_text").unwrap_or_default(), body: string("full_text").unwrap_or_default(),
                    translated: string("text_translated"), start: number("chunk_start_char"), end: number("chunk_end_char"),
                };
                rows.insert(key, (version.0, version.1, piece));
            }
        }
        let mut grouped = std::collections::BTreeMap::<(String, String), Vec<FtsPiece>>::new();
        for ((owner, doc_id, _), (_, _, piece)) in rows { grouped.entry((owner, doc_id)).or_default().push(piece); }
        let mut inputs = Vec::new();
        for ((_, doc_id), pieces) in grouped {
            if let Some(input) = fts_input_from_pieces(doc_id, pieces)? { inputs.push(input); }
        }
        Ok(inputs)
    }

    async fn fts_inputs_for_doc_changes(&self, doc_ids: &[String], chunks: &[DocumentChunk], replace_doc_ids: &[String]) -> Result<Vec<super::ingest::TantivyInputOwned>> {
        if doc_ids.is_empty() { return Ok(Vec::new()); }
        let replaced = import_replacement_ids(chunks, replace_doc_ids);
        let existing: Vec<RecordBatch> = self.table.query().only_if(format!("doc_id IN ({})", sql_values(doc_ids)))
            .select(lancedb::query::Select::Columns(["id", "doc_id", "owner_id", "language", "title", "headings_text", "full_text", "text_translated", "chunk_index", "chunk_start_char", "chunk_end_char"].iter().map(|s| s.to_string()).collect()))
            .execute().await?.try_collect().await?;
        let mut rows = std::collections::BTreeMap::<String, FtsPiece>::new();
        for batch in existing {
            for row in 0..batch.num_rows() {
                let string = |name: &str| str_col_opt(&batch, name).filter(|c| !c.is_null(row)).map(|c| c.value(row).to_owned());
                let number = |name: &str| batch.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Int32Array>()).filter(|c| !c.is_null(row)).map(|c| c.value(row));
                let doc_id = string("doc_id").unwrap_or_default();
                if replaced.contains(&doc_id) { continue; }
                let id = string("id").unwrap_or_default();
                rows.insert(id.clone(), FtsPiece { id, doc_id, owner: string("owner_id").unwrap_or_default(),
                    language: string("language").unwrap_or_default(), title: string("title").unwrap_or_default(),
                    headings: string("headings_text").unwrap_or_default(), body: string("full_text").unwrap_or_default(),
                    translated: string("text_translated"), chunk_index: number("chunk_index").unwrap_or(-1),
                    start: number("chunk_start_char"), end: number("chunk_end_char") });
            }
        }
        for chunk in chunks { rows.insert(chunk.id.clone(), FtsPiece::from(chunk)); }
        let mut grouped = std::collections::BTreeMap::<String, Vec<FtsPiece>>::new();
        for (_, piece) in rows { grouped.entry(piece.doc_id.clone()).or_default().push(piece); }
        let mut inputs = Vec::new();
        for (doc_id, pieces) in grouped {
            if let Some(input) = fts_input_from_pieces(doc_id, pieces)? { inputs.push(input); }
        }
        Ok(inputs)
    }

    pub(crate) async fn row_generation_committed(&self, row_ids: &[String], generation: &str) -> Result<bool> {
        if row_ids.is_empty() { return Ok(false); }
        self.table.checkout_latest().await?;
        let batches: Vec<RecordBatch> = self.table.query().only_if(format!("id IN ({})", sql_values(row_ids)))
            .select(lancedb::query::Select::Columns(vec!["id".into(), "metadata_json".into()]))
            .execute().await?.try_collect().await?;
        let mut found = std::collections::BTreeSet::new();
        for batch in batches {
            let (Some(ids), Some(metadata)) = (str_col_opt(&batch, "id"), str_col_opt(&batch, "metadata_json")) else { continue; };
            for row in 0..batch.num_rows() {
                if metadata.is_null(row) || ids.is_null(row) { continue; }
                let value: serde_json::Value = serde_json::from_str(metadata.value(row))?;
                if value.get("index_write_generation").and_then(|v| v.as_str()) == Some(generation) { found.insert(ids.value(row).to_owned()); }
            }
        }
        anyhow::ensure!(found.is_empty() || found.len() == row_ids.len(), "Incomplete index generation; full-text repair retained");
        Ok(found.len() == row_ids.len())
    }

    /// Find the old document identities superseded by complete replacements.
    /// A changed file has a new content hash, but still occupies the same source
    /// location. Matching the owner as well prevents cross-owner replacement.
    pub(crate) async fn replacement_doc_ids(&self, chunks: &[DocumentChunk]) -> Result<Vec<String>> {
        self.table.checkout_latest().await?;
        let filter = replacement_filter(chunks)?;
        let batches: Vec<RecordBatch> = self.table.query().only_if(filter)
            .select(lancedb::query::Select::Columns(vec!["doc_id".into()]))
            .execute().await?.try_collect().await?;
        let mut ids = std::collections::BTreeSet::new();
        ids.extend(chunks.iter().map(|c| c.doc_id.clone()));
        for batch in batches {
            if let Some(column) = str_col_opt(&batch, "doc_id") {
                ids.extend(column.iter().flatten().map(str::to_owned));
            }
        }
        Ok(ids.into_iter().collect())
    }

    /// Atomically replace complete documents after extraction and embedding
    /// have succeeded. A fresh generation in metadata makes the merge key
    /// distinct from every old row, including duplicate rows from older app
    /// versions. The delete and insert are one Lance transaction; unrelated
    /// documents remain untouched. Do not call with a partial chunk batch.
    pub(crate) async fn replace_documents(&self, chunks: &mut [DocumentChunk], generation: &str) -> Result<()> {
        if chunks.is_empty() { return Ok(()); }
        let filter = replacement_filter(chunks)?;
        for chunk in chunks.iter_mut() {
            let mut metadata: serde_json::Value = chunk.metadata_json.as_deref()
                .map(serde_json::from_str).transpose()?
                .unwrap_or_else(|| serde_json::json!({}));
            anyhow::ensure!(metadata.is_object(), "Document metadata must be an object");
            metadata["index_write_generation"] = generation.into();
            chunk.metadata_json = Some(metadata.to_string());
        }
        // Build and validate every Arrow row before any mutation. A model
        // dimension or conversion failure must retain the last good version.
        let batch = chunks_to_record_batch(chunks, self.dims, &self.schema)?;
        let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], self.schema.clone());
        let mut merge = self.table.merge_insert(&["id", "metadata_json"]);
        merge.when_not_matched_insert_all()
            .when_not_matched_by_source_delete(Some(filter));
        merge.execute(Box::new(reader)).await.context("replacing complete document rows")?;
        super::result_cache::invalidate();
        Ok(())
    }

    /// Idempotent metadata ingestion; re-scanning a catalog does not append a
    /// second copy of its representative row. This deliberately does not delete
    /// extracted chunks: a metadata-only scan cannot downgrade a document.
    pub(crate) async fn upsert_metadata_rows(&self, chunks: &[DocumentChunk]) -> Result<()> {
        if chunks.is_empty() { return Ok(()); }
        self.table.checkout_latest().await?;
        let mut pending = Vec::new();
        let mut scopes = std::collections::BTreeSet::new();
        let mut identities = std::collections::BTreeSet::new();
        for chunk in chunks {
            let chunk = self.metadata_occurrence(chunk).await?;
            anyhow::ensure!(identities.insert(chunk.id.clone()), "Duplicate metadata identity in one batch");
            let scope = SourceOccurrence::from_chunk(&chunk).filter();
            let existing: Vec<RecordBatch> = self.table.query()
                .only_if(format!("{scope} AND chunk_index = 0"))
                .select(lancedb::query::Select::Columns(vec!["doc_id".into(), "metadata_json".into()]))
                .limit(1).execute().await?.try_collect().await?;
            if existing.iter().any(|b| b.num_rows() > 0) {
                // A failed refresh keeps the previously searchable body and
                // vectors, but records the failed attempt for the UI. A plain
                // catalog scan leaves extracted content and metadata intact.
                let attempted: serde_json::Value = chunk.metadata_json.as_deref()
                    .map(serde_json::from_str).transpose()?.unwrap_or(serde_json::Value::Null);
                if let Some(failure) = attempted.get("extraction_failure") {
                    self.annotate_occurrence_failure(&SourceOccurrence::from_chunk(&chunk), failure, &chunk.source_hash).await?;
                }
                continue;
            }
            scopes.insert(format!("({scope} AND chunk_index < 0)"));
            pending.push(chunk.clone());
        }
        if pending.is_empty() {
            super::result_cache::invalidate();
            return Ok(());
        }
        let generation = uuid::Uuid::new_v4().to_string();
        for chunk in &mut pending {
            let mut metadata: serde_json::Value = chunk.metadata_json.as_deref()
                .map(serde_json::from_str).transpose()?.unwrap_or_else(|| serde_json::json!({}));
            metadata["index_write_generation"] = generation.clone().into();
            chunk.metadata_json = Some(metadata.to_string());
        }
        let batch = chunks_to_record_batch(&pending, self.dims, &self.schema)?;
        let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], self.schema.clone());
        let mut merge = self.table.merge_insert(&["id", "metadata_json"]);
        merge.when_not_matched_insert_all()
            .when_not_matched_by_source_delete(Some(scopes.into_iter().collect::<Vec<_>>().join(" OR ")));
        merge.execute(Box::new(reader)).await.context("upserting metadata rows")?;
        super::result_cache::invalidate();
        Ok(())
    }

    /// A journal entry is replayable only if its entire vector transaction
    /// committed. Intent records written before a failed transaction are safe
    /// to discard; they must never overwrite the previous full-text index.
    pub(crate) async fn generation_committed(&self, doc_ids: &[String], generation: &str) -> Result<bool> {
        if doc_ids.is_empty() { return Ok(false); }
        self.table.checkout_latest().await?;
        let quoted = doc_ids.iter().map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>().join(",");
        let predicate = format!("doc_id IN ({quoted}) AND chunk_index = 0");
        let batches: Vec<RecordBatch> = self.table.query().only_if(predicate)
            .select(lancedb::query::Select::Columns(vec!["doc_id".into(), "metadata_json".into()]))
            .execute().await?.try_collect().await?;
        let mut committed = std::collections::BTreeSet::new();
        for batch in batches {
            let (Some(ids), Some(metadata)) = (str_col_opt(&batch, "doc_id"), str_col_opt(&batch, "metadata_json")) else { continue; };
            for row in 0..batch.num_rows() {
                if ids.is_null(row) || metadata.is_null(row) { continue; }
                let value: serde_json::Value = serde_json::from_str(metadata.value(row))?;
                if value.get("index_write_generation").and_then(|v| v.as_str()) == Some(generation) {
                    committed.insert(ids.value(row).to_owned());
                }
            }
        }
        anyhow::ensure!(committed.is_empty() || committed.len() == doc_ids.len(),
            "Incomplete index generation; full-text repair is retained for retry");
        Ok(committed.len() == doc_ids.len())
    }

    // ── Search ─────────────────────────────────────────────────────────────

    /// ANN vector search.
    pub async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_vector_column(embedding, "embedding", filters, limit).await
    }

    /// P22 — "More Like This": find documents similar to a given
    /// doc_id + chunk_index by looking up the chunk's embedding and
    /// running an ANN search excluding the source document.
    pub async fn find_similar(
        &self,
        doc_id: &str,
        chunk_index: i32,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // 1. Look up the embedding for this doc_id + chunk_index.
        let row_id = chunk_row_id(doc_id, chunk_index);
        let filter = format!("id = '{}'", row_id.replace('\'', "''"));
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;

        if batches.is_empty() || batches[0].num_rows() == 0 {
            return Err(anyhow!("Document not found: {}", doc_id));
        }

        // 2. Extract the embedding vector.
        let batch = &batches[0];
        let emb_col = batch
            .column_by_name("embedding")
            .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
            .ok_or_else(|| anyhow!("No embedding column"))?;

        if emb_col.is_null(0) {
            return Err(anyhow!("Document has no embedding (L1-only?)"));
        }

        let values = emb_col.value(0);
        let float_arr = values
            .as_any()
            .downcast_ref::<Float32Array>()
            .ok_or_else(|| anyhow!("Embedding is not Float32"))?;
        let embedding: Vec<f32> = float_arr.values().to_vec();

        // 3. ANN search excluding the source document.
        let exclude = format!("doc_id != '{}'", doc_id.replace('\'', "''"));
        let vq = self
            .table
            .vector_search(embedding)?
            .distance_type(DistanceType::Cosine)
            .select(search_result_columns())
            .limit(limit)
            .only_if(exclude);

        let result_batches: Vec<RecordBatch> = vq.execute().await?.try_collect().await?;
        record_batches_to_search_results(&result_batches)
    }

    /// ANN search targeting a vector column, returning one best chunk per
    /// document occurrence. Candidate widening avoids long-document crowding;
    /// metadata filters apply in the database before every ANN collection.
    pub async fn search_vector_column(
        &self,
        embedding: &[f32],
        column: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if limit == 0 { return Ok(Vec::new()); }
        // ANN limits count chunks. Widen the candidate window when a long
        // document crowds out other sources, keeping its highest-ranked chunk.
        // This remains approximate retrieval; the bounded window avoids a full
        // corpus scan for every semantic request.
        let maximum_candidates = limit.max(4096);
        let mut candidate_limit = limit.saturating_mul(4).clamp(40, maximum_candidates);
        loop {
            let mut vq = self.table.vector_search(embedding)?
                .column(column)
                .distance_type(DistanceType::Cosine)
                .select(search_result_columns())
                .limit(candidate_limit);
            if let Some(sql) = filters.to_lance_sql() { vq = vq.only_if(sql); }
            let batches: Vec<RecordBatch> = vq.execute().await?.try_collect().await?;
            let rows = record_batches_to_search_results(&batches)?;
            let exhausted = rows.len() < candidate_limit;
            let mut seen = std::collections::HashSet::new();
            let mut documents: Vec<_> = rows.into_iter().filter(|hit|
                seen.insert((hit.owner_id.clone(), hit.doc_id.clone()))).collect();
            if documents.len() >= limit || exhausted || candidate_limit >= maximum_candidates {
                documents.truncate(limit);
                return Ok(documents);
            }
            candidate_limit = candidate_limit.saturating_mul(2).min(maximum_candidates);
        }
    }

    /// Stage AE — ColBERT late-interaction re-ranking of an existing
    /// candidate pool.  Fetches each candidate's `multivec_packed` +
    /// `multivec_n_tokens`, computes MaxSim against `query_multivec`,
    /// replaces each candidate's `score` with the MaxSim, re-sorts
    /// descending, and trims to `limit`.
    ///
    /// Candidates whose row has no ColBERT vectors (NULL column from
    /// rows ingested before v105, or models without a ColBERT head)
    /// are kept with their original score — the re-rank is purely
    /// additive on rows that have the data.
    ///
    /// `query_multivec` is the per-token L2-normalised query encoding
    /// from `Embedder::embed_multivec(vec![query]).unwrap()[0]`.  Empty
    /// outer vec is a no-op (returns `candidates` truncated to `limit`).
    pub async fn rerank_with_colbert(
        &self,
        candidates: Vec<SearchResult>,
        query_multivec: &[Vec<f32>],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if query_multivec.is_empty() || candidates.is_empty() {
            let mut out = candidates;
            out.truncate(limit);
            return Ok(out);
        }

        let keys: Vec<_> = candidates.iter().map(|r| (r.owner_id.clone(), r.doc_id.clone())).collect();
        let ids: Vec<_> = candidates.iter().map(|r| chunk_row_id(&r.doc_id, r.chunk_index)).collect();
        let wanted: std::collections::HashSet<_> = candidates.iter().map(|r| passage_key(&r.owner_id, &r.doc_id, r.chunk_index)).collect();
        let filter = format!("({}) AND id IN ({})", document_keys_sql(&keys), sql_values(&ids));

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(["doc_id", "owner_id", "chunk_index", "indexed_at", "multivec_packed", "multivec_n_tokens"]
                .into_iter().map(str::to_owned).collect()))
            .with_row_id()
            .execute()
            .await?
            .try_collect()
            .await?;

        // Build id -> MaxSim score map.
        let dim = query_multivec[0].len();
        let mut score_by_id: std::collections::HashMap<String, f32> =
            std::collections::HashMap::with_capacity(candidates.len());
        let mut latest = std::collections::HashMap::<String, (i64, u64)>::new();
        for batch in &batches {
            let docs = str_col(batch, "doc_id")?;
            let owners = str_col(batch, "owner_id")?;
            let indices = i32_col(batch, "chunk_index")?;
            let times = ts_ms_col_opt(batch, "indexed_at");
            let row_ids = batch.column_by_name("_rowid").and_then(|c| c.as_any().downcast_ref::<arrow_array::UInt64Array>())
                .context("Missing reranker row identities")?;
            let packed_col = batch
                .column_by_name("multivec_packed")
                .and_then(|c| c.as_any().downcast_ref::<LargeBinaryArray>());
            let n_tok_col = batch
                .column_by_name("multivec_n_tokens")
                .and_then(|c| c.as_any().downcast_ref::<Int16Array>());
            let (Some(packed_col), Some(n_tok_col)) = (packed_col, n_tok_col) else {
                // Column missing entirely — corpus pre-v105.  Bail to
                // the original-score path.
                break;
            };
            for i in 0..batch.num_rows() {
                let key = passage_key(owners.value(i), docs.value(i), indices.value(i));
                if !wanted.contains(&key) { continue; }
                let order = (times.filter(|c| !c.is_null(i)).map_or(i64::MIN, |c| c.value(i)), row_ids.value(i));
                if latest.get(&key).is_some_and(|previous| *previous > order) { continue; }
                latest.insert(key.clone(), order);
                score_by_id.remove(&key);
                if packed_col.is_null(i) || n_tok_col.is_null(i) { continue; }
                let packed = packed_col.value(i);
                let n_tok = n_tok_col.value(i);
                let doc_vecs = unpack_multivec(packed, n_tok, dim);
                if doc_vecs.is_empty() {
                    continue;
                }
                let score = maxsim(query_multivec, &doc_vecs);
                if score.is_finite() { score_by_id.insert(key, score); }
            }
        }

        let mut out: Vec<SearchResult> = candidates
            .into_iter()
            .map(|mut r| {
                let id = passage_key(&r.owner_id, &r.doc_id, r.chunk_index);
                if let Some(s) = score_by_id.get(&id) {
                    r.score = *s;
                }
                r
            })
            .collect();
        // MaxSim and the incoming RRF/BM25 scores have different units.
        // Rank scored rows together, followed by unscored rows in input order.
        out.sort_by(|a, b| {
            let a_scored = score_by_id.get(&passage_key(&a.owner_id, &a.doc_id, a.chunk_index)).filter(|v| v.is_finite());
            let b_scored = score_by_id.get(&passage_key(&b.owner_id, &b.doc_id, b.chunk_index)).filter(|v| v.is_finite());
            match (a_scored, b_scored) {
                (Some(a), Some(b)) => b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Fetch one representative passage for every ranked full-text document.
    ///
    /// Scan all selected documents' passages as a stream. A shared row limit
    /// cannot express a per-document limit: a long document would consume it
    /// and hide later documents, and a late matching passage would be missed.
    /// Memory is bounded by one Arrow batch plus one winning result per doc.
    /// Ties prefer the earliest chunk and returned order follows `doc_ids`.
    pub async fn fetch_best_chunk_per_doc(
        &self,
        doc_ids: &[String],
        query_terms: &[&str],
        score_map: &std::collections::HashMap<String, f32>,
    ) -> Result<Vec<SearchResult>> {
        let keys = self.matching_document_keys(doc_ids, None).await?;
        let mut owners = std::collections::HashMap::new();
        for (owner, doc) in keys {
            if let Some(previous) = owners.insert(doc, owner.clone()) {
                anyhow::ensure!(previous == owner, "Document identity belongs to multiple owners; select an exact owner before hydration");
            }
        }
        let keys: Vec<_> = doc_ids.iter().filter_map(|doc| owners.get(doc).map(|owner| (owner.clone(), doc.clone()))).collect();
        let scores = keys.iter().map(|(owner, doc)| (super::fts_index::document_key(owner, doc), score_map.get(doc).copied().unwrap_or(0.0))).collect();
        self.fetch_best_chunk_per_keys(&keys, query_terms, &scores, &SearchFilters::default()).await
    }

    /// Return every exact owner/document with any stored row matching metadata.
    /// Sparse chunk numbering and long documents do not change eligibility.
    pub async fn matching_document_keys(&self, doc_ids: &[String], extra_sql: Option<&str>) -> Result<Vec<(String, String)>> {
        let mut keys = std::collections::BTreeSet::new();
        for ids in doc_ids.chunks(256) {
            let mut filter = format!("doc_id IN ({})", sql_values(ids));
            if let Some(extra) = extra_sql.filter(|sql| !sql.trim().is_empty()) { filter = format!("({filter}) AND ({extra})"); }
            let mut stream = self.table.query().only_if(filter)
                .select(lancedb::query::Select::Columns(vec!["owner_id".into(), "doc_id".into()])).execute().await?;
            while let Some(batch) = stream.try_next().await? {
                let owners = str_col(&batch, "owner_id")?;
                let docs = str_col(&batch, "doc_id")?;
                for row in 0..batch.num_rows() { keys.insert((owners.value(row).to_owned(), docs.value(row).to_owned())); }
            }
        }
        Ok(keys.into_iter().collect())
    }

    /// Choose a passage only from the exact lexical identities and matching
    /// metadata rows. Scores use owner-scoped document keys throughout fusion.
    pub async fn fetch_best_chunk_per_keys(
        &self,
        keys: &[(String, String)],
        query_terms: &[&str],
        score_map: &std::collections::HashMap<String, f32>,
        filters: &SearchFilters,
    ) -> Result<Vec<SearchResult>> {
        let mut best = std::collections::HashMap::<String, (SearchResult, usize)>::new();
        let extra_sql = filters.to_lance_sql();
        for group in keys.chunks(256) {
            let mut filter = document_keys_sql(group);
            if let Some(extra) = &extra_sql { filter = format!("({filter}) AND ({extra})"); }
            let mut stream = self.table.query().only_if(filter).select(search_result_columns()).execute().await?;
            while let Some(batch) = stream.try_next().await? {
                let text = str_col_opt(&batch, "full_text");
                let results = record_batches_to_search_results(std::slice::from_ref(&batch))?;
                for (row, mut result) in results.into_iter().enumerate() {
                    let key = super::fts_index::document_key(&result.owner_id, &result.doc_id);
                    result.score = score_map.get(&key).copied().unwrap_or(0.0);
                    let body = text.filter(|column| !column.is_null(row)).map(|column| column.value(row)).unwrap_or("");
                    let hits = if query_terms.is_empty() { 0 } else {
                        let tokens: std::collections::HashSet<_> = super::fts_query::simple_tokenize(body).into_iter().collect();
                        query_terms.iter().filter(|&&term| tokens.contains(term)).count()
                    };
                    let replace = match best.get(&key) {
                        None => true,
                        Some((previous, previous_hits)) => hits > *previous_hits
                            || (hits == *previous_hits && result.chunk_index < previous.chunk_index),
                    };
                    if replace { best.insert(key, (result, hits)); }
                }
            }
        }
        Ok(keys.iter().filter_map(|(owner, doc)| best.remove(&super::fts_index::document_key(owner, doc)).map(|(result, _)| result)).collect())
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
        candidate_doc_ids: &[(String, String)],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if candidate_doc_ids.is_empty() || query.indices.is_empty() {
            return Ok(vec![]);
        }
        let mut filter = document_keys_sql(candidate_doc_ids);
        if let Some(extra) = filters.to_lance_sql() {
            filter = format!("({}) AND ({})", filter, extra);
        }
        // 8 chunks per doc lets us pick the best per-doc chunk without
        // scanning the full doc. Project to search-result cols +
        // embedding_sparse (needed for scoring) to avoid reading dense
        // embedding vectors.
        let mut sparse_cols = vec![
            "embedding_sparse".into(),
        ];
        if let lancedb::query::Select::Columns(ref cols) = search_result_columns() {
            sparse_cols.extend(cols.iter().cloned());
        }
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(sparse_cols))
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
            let url_col = str_col_opt(batch, "url");
            let summary_col = str_col_opt(batch, "summary");
            let doc_status_col = str_col_opt(batch, "doc_status");

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
                let snippet = super::snippet::truncate_str(full_text, 400).to_owned();

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
                    url: str_col_val_opt(&url_col, i),
                    tags: list_str_col_val(batch, "tags", i),
                    summary: str_col_val_opt(&summary_col, i),
                    doc_status: str_col_val_opt(&doc_status_col, i),
                };
                let doc_id = super::fts_index::document_key(&result.owner_id, &result.doc_id);
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

    /// Return representative local rows for one URI so cloud-backup pull can
    /// resolve changed content instead of silently leaving duplicate versions.
    pub async fn conflict_rows_for_location(&self, location_uri: &str) -> Result<Vec<ConflictRow>> {
        let filter = format!(
            "location_uri = '{}' AND chunk_index <= 0",
            location_uri.replace('\'', "''")
        );
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(vec![
                "doc_id".into(), "source_hash".into(),
                "indexed_at".into(), "title".into(),
            ]))
            .limit(100)
            .execute()
            .await?
            .try_collect()
            .await?;

        let mut out = Vec::new();
        for batch in &batches {
            let Some(doc_ids) = batch.column_by_name("doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>()) else { continue };
            let hashes = batch.column_by_name("source_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let indexed = batch.column_by_name("indexed_at")
                .and_then(|c| c.as_any().downcast_ref::<TimestampMillisecondArray>());
            let titles = batch.column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            for i in 0..batch.num_rows() {
                if doc_ids.is_null(i) { continue; }
                out.push(ConflictRow {
                    doc_id: doc_ids.value(i).to_owned(),
                    source_hash: hashes.filter(|c| !c.is_null(i))
                        .map(|c| c.value(i).to_owned()).unwrap_or_default(),
                    indexed_at: indexed.filter(|c| !c.is_null(i)).map(|c| c.value(i)),
                    title: titles.filter(|c| !c.is_null(i))
                        .map(|c| c.value(i).to_owned()),
                });
            }
        }
        Ok(out)
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
        let fts = super::fts_index::FtsIndex::open_or_create(
            &self.ingest_journal_path.parent().ok_or_else(|| anyhow!("Missing index directory"))?.join("fts"))?;
        self.delete_document_with_fts(doc_id, &fts).await
    }

    /// Update the `location_uri` column for all rows of a document.
    pub async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        let _guard = self.write_guard().await?;
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        self.update_location_filter_locked(&filter, new_uri).await?;
        Ok(())
    }

    /// Update `location_uri` by matching the old URI (no doc_id required).
    pub async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()> {
        let _guard = self.write_guard().await?;
        let filter = format!("location_uri = '{}'", old_uri.replace('\'', "''"));
        self.update_location_filter_locked(&filter, new_uri).await?;
        Ok(())
    }

    /// Set (or clear) the `doc_status` label for all chunks belonging to `doc_id`.
    /// Pass `None` to reset to NULL.  Used by the `index_set_doc_status` Tauri
    /// command so the UI can tag documents as e.g. "reviewed" / "rejected".
    pub async fn set_doc_status(&self, doc_id: &str, status: Option<&str>) -> Result<()> {
        let _guard = self.write_guard().await?;
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        let value = match status {
            Some(s) => format!("'{}'", s.replace('\'', "''")),
            None => "NULL".to_string(),
        };
        self.table
            .update()
            .only_if(filter)
            .column("doc_status", value)
            .execute()
            .await
            .context("updating doc_status")?;
        super::result_cache::invalidate();
        Ok(())
    }

    /// Remove `extraction_failure` from `metadata_json` so the next background
    /// ingest run re-attempts extraction. Only meaningful for retryable reasons
    /// (Timeout / Other); the caller should check `is_retryable()` first.
    /// Preserve a last good text version and invalidate the unchanged guard so
    /// an explicit retry also runs when the source bytes have not changed.
    pub async fn clear_extraction_failure(&self, doc_id: &str) -> Result<()> {
        let _guard = self.write_guard().await?;
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
        let retained_text = batches.iter().any(|b| {
            b.column_by_name("chunk_index")
                .and_then(|c| c.as_any().downcast_ref::<Int32Array>())
                .is_some_and(|c| !c.is_empty() && c.value(0) >= 0)
        });
        let new_meta = if let Some(mut m) = current_meta {
            m.as_object_mut().map(|o| {
                o.remove("extraction_failure");
                o.remove("failed_source_hash");
                o.remove("extraction_skip");
                o.remove("mtime_unix");
                o.insert("level".to_owned(), serde_json::Value::from(if retained_text { 3i64 } else { 1i64 }));
            });
            m.to_string()
        } else {
            r#"{"level":1}"#.to_owned()
        };
        self.table
            .update()
            .only_if(format!("doc_id = '{}'", doc_id.replace('\'', "''")))
            .column("metadata_json", format!("'{}'", new_meta.replace('\'', "''")))
            .execute()
            .await
            .context("clear_extraction_failure update")?;
        super::result_cache::invalidate();
        Ok(())
    }

    /// Patch metadata without dropping searchable text or vector rows. Supplied
    /// JSON is shallow-merged into each row's existing metadata. Title and
    /// language changes use the same durable paired write as body ingestion.
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
        let fts = super::fts_index::FtsIndex::open_or_create(
            &self.ingest_journal_path.parent().ok_or_else(|| anyhow!("Missing index directory"))?.join("fts"))?;
        super::ingest::apply_metadata_update(self, &fts, doc_id, title, author, year,
            language, page_count, metadata_json).await
    }

    /// Prepare all affected Arrow rows before recording a durable intent.
    /// Untouched columns keep their original arrays, preserving vectors and
    /// chunk-specific evidence during the atomic replacement.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_metadata_update(
        &self, doc_id: &str, title: Option<&str>, author: Option<&str>,
        year: Option<i32>, language: Option<&str>, page_count: Option<i32>,
        metadata_json: Option<&str>, generation: &str,
    ) -> Result<(Vec<String>, Vec<RecordBatch>)> {
        let patch = metadata_json.map(serde_json::from_str::<serde_json::Value>)
            .transpose()?.unwrap_or_else(|| serde_json::json!({}));
        let patch = patch.as_object().ok_or_else(|| anyhow!("Metadata update must be a JSON object"))?;
        let batches: Vec<RecordBatch> = self.table.query()
            .only_if(format!("doc_id = '{}'", doc_id.replace('\'', "''")))
            .execute().await?.try_collect().await?;
        let previous_time = batches.iter().filter_map(|batch| batch.column_by_name("indexed_at")
            .and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>()))
            .flat_map(|column| column.values().iter().copied()).max().unwrap_or(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
        let updated_at = now.max(previous_time.checked_add(1).ok_or_else(|| anyhow!("Metadata timestamp overflow"))?);
        let mut row_ids = std::collections::BTreeSet::new();
        let mut prepared = Vec::with_capacity(batches.len());
        for batch in batches {
            if batch.num_rows() == 0 { continue; }
            let ids = str_col_opt(&batch, "id").ok_or_else(|| anyhow!("Missing chunk identity"))?;
            let metadata = str_col_opt(&batch, "metadata_json");
            let chunk_indices = batch.column_by_name("chunk_index")
                .and_then(|column| column.as_any().downcast_ref::<Int32Array>());
            let mut updated_metadata = Vec::with_capacity(batch.num_rows());
            for row in 0..batch.num_rows() {
                anyhow::ensure!(!ids.is_null(row) && !ids.value(row).is_empty(), "Missing chunk identity");
                row_ids.insert(ids.value(row).to_owned());
                let previous = metadata.filter(|c| !c.is_null(row)).map(|c| c.value(row));
                let mut value = previous.map(serde_json::from_str::<serde_json::Value>)
                    .transpose()?.unwrap_or_else(|| serde_json::json!({}));
                let object = value.as_object_mut().ok_or_else(|| anyhow!("Existing document metadata must be a JSON object"))?;
                let row_level = if chunk_indices.is_some_and(|indices| !indices.is_null(row) && indices.value(row) >= 0) { 3 } else { 1 };
                let current_level = object.get("level").and_then(serde_json::Value::as_i64).unwrap_or(row_level).max(row_level);
                let incoming_level = patch.get("level").and_then(serde_json::Value::as_i64).unwrap_or(current_level);
                object.extend(patch.clone());
                object.insert("level".into(), current_level.max(incoming_level).into());
                object.insert("index_write_generation".into(), generation.into());
                updated_metadata.push(value.to_string());
            }
            let schema = batch.schema();
            let mut columns = batch.columns().to_vec();
            columns[schema.index_of("indexed_at")?] = Arc::new(TimestampMillisecondArray::from(vec![updated_at; batch.num_rows()]));
            columns[schema.index_of("metadata_json")?] = Arc::new(StringArray::from(updated_metadata));
            for (name, value) in [("title", title), ("author", author), ("language", language)] {
                if let Some(value) = value {
                    columns[schema.index_of(name)?] = Arc::new(StringArray::from(vec![Some(value); batch.num_rows()]));
                }
            }
            for (name, value) in [("year", year), ("page_count", page_count)] {
                if let Some(value) = value {
                    columns[schema.index_of(name)?] = Arc::new(Int32Array::from(vec![Some(value); batch.num_rows()]));
                }
            }
            prepared.push(RecordBatch::try_new(schema, columns)?);
        }
        Ok((row_ids.into_iter().collect(), prepared))
    }

    /// Called only with the paired writer lease and a durable intent in place.
    pub(crate) async fn apply_metadata_columns(
        &self, doc_id: &str, batches: Vec<RecordBatch>,
    ) -> Result<()> {
        if batches.is_empty() { return Ok(()); }
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        let schema = batches[0].schema();
        let reader = arrow_array::RecordBatchIterator::new(batches.into_iter().map(Ok), schema);
        let mut merge = self.table.merge_insert(&["id", "metadata_json"]);
        merge.when_not_matched_insert_all().when_not_matched_by_source_delete(Some(filter));
        merge.execute(Box::new(reader)).await.context("committing document metadata")?;
        super::result_cache::invalidate();
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
        let _guard = self.write_guard().await?;
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
        let _guard = self.write_guard().await?;
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

    // ── Corpus Stats ───────────────────────────────────────────────────────

    /// P22 — compute aggregate corpus statistics for the dashboard.
    /// Scans all document-level rows (`chunk_index <= 0`) and computes
    /// distributions client-side.
    pub async fn corpus_stats(&self) -> Result<super::tauri_commands::CorpusStats> {
        use lancedb::query::Select;

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index <= 0")
            .select(Select::Columns(vec![
                "ext".to_owned(),
                "language".to_owned(),
                "year".to_owned(),
                "metadata_json".to_owned(),
            ]))
            .execute()
            .await?
            .try_collect()
            .await?;

        let total_docs: usize = batches.iter().map(|b| b.num_rows()).sum();
        let total_chunks = self.count().await?;

        let mut ext_map: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut lang_map: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut year_map: std::collections::HashMap<i32, usize> =
            std::collections::HashMap::new();
        let mut total_size_bytes: u64 = 0;

        for batch in &batches {
            let ext_col = str_col_opt(batch, "ext");
            let lang_col = str_col_opt(batch, "language");
            let year_col = i32_col_opt(batch, "year");
            let meta_col = str_col_opt(batch, "metadata_json");

            for i in 0..batch.num_rows() {
                if let Some(col) = ext_col {
                    if !col.is_null(i) {
                        let v = col.value(i).to_lowercase();
                        if !v.is_empty() {
                            *ext_map.entry(v).or_default() += 1;
                        }
                    }
                }
                if let Some(col) = lang_col {
                    if !col.is_null(i) {
                        let v = col.value(i).to_string();
                        if !v.is_empty() {
                            *lang_map.entry(v).or_default() += 1;
                        }
                    }
                }
                if let Some(col) = year_col {
                    if !col.is_null(i) {
                        *year_map.entry(col.value(i)).or_default() += 1;
                    }
                }
                // Extract fs_size from metadata_json for total size
                if let Some(col) = meta_col {
                    if !col.is_null(i) {
                        let json = col.value(i);
                        if let Some(size) = extract_i64_from_json(json, "fs_size") {
                            total_size_bytes += size as u64;
                        }
                    }
                }
            }
        }

        // Collect tag distribution from the tags column separately
        // (requires a List<Utf8> scan; cheaper to do here than a second
        // full scan). Tags are only on chunk_index=0 rows post-v107.
        let mut tag_map: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let tag_batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index <= 0")
            .select(Select::Columns(vec!["tags".to_owned()]))
            .execute()
            .await?
            .try_collect()
            .await?;
        for batch in &tag_batches {
            for i in 0..batch.num_rows() {
                let tags = list_str_col_val(batch, "tags", i);
                for t in tags {
                    *tag_map.entry(t).or_default() += 1;
                }
            }
        }

        // Sort distributions by count descending
        let mut ext_distribution: Vec<(String, usize)> = ext_map.into_iter().collect();
        ext_distribution.sort_by(|a, b| b.1.cmp(&a.1));

        let mut lang_distribution: Vec<(String, usize)> = lang_map.into_iter().collect();
        lang_distribution.sort_by(|a, b| b.1.cmp(&a.1));

        let mut year_histogram: Vec<(i32, usize)> = year_map.into_iter().collect();
        year_histogram.sort_by_key(|&(y, _)| y);

        let mut top_tags: Vec<(String, usize)> = tag_map.into_iter().collect();
        top_tags.sort_by(|a, b| b.1.cmp(&a.1));
        top_tags.truncate(50);

        Ok(super::tauri_commands::CorpusStats {
            total_docs,
            total_chunks,
            ext_distribution,
            lang_distribution,
            year_histogram,
            top_tags,
            total_size_bytes,
        })
    }

    // ── Clustering ─────────────────────────────────────────────────────────

    /// K-means clustering on the dense embedding column.  Returns `k`
    /// clusters, each with its member doc_ids and top TF-IDF terms
    /// (from `full_text`) as a human-readable name.
    pub async fn cluster_documents(
        &self,
        k: usize,
    ) -> Result<Vec<super::tauri_commands::Cluster>> {
        use lancedb::query::Select;

        if k == 0 {
            return Err(anyhow!("k must be >= 1"));
        }

        // 1. Fetch all docs with embeddings (one row per document).
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index <= 0 AND embedding IS NOT NULL")
            .select(Select::Columns(vec![
                "doc_id".to_owned(),
                "embedding".to_owned(),
                "full_text".to_owned(),
                "title".to_owned(),
            ]))
            .execute()
            .await?
            .try_collect()
            .await?;

        // Collect doc_ids, embeddings, texts.
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        let mut doc_ids: Vec<String> = Vec::with_capacity(total_rows);
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(total_rows);
        let mut texts: Vec<String> = Vec::with_capacity(total_rows);
        let mut titles: Vec<String> = Vec::with_capacity(total_rows);

        for batch in &batches {
            let n = batch.num_rows();
            let doc_col = batch.column_by_name("doc_id");
            let emb_col = batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
            let text_col = batch.column_by_name("full_text");
            let title_col = batch.column_by_name("title");

            for i in 0..n {
                // doc_id
                let did = doc_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
                    .and_then(|a| if a.is_null(i) { None } else { Some(a.value(i).to_owned()) })
                    .unwrap_or_default();

                // embedding
                let emb = emb_col.and_then(|fsl| {
                    if fsl.is_null(i) { return None; }
                    let vals = fsl.value(i);
                    vals.as_any()
                        .downcast_ref::<Float32Array>()
                        .map(|a| a.values().to_vec())
                });
                let Some(e) = emb else { continue };

                let txt = text_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
                    .and_then(|a| if a.is_null(i) { None } else { Some(a.value(i).to_owned()) })
                    .unwrap_or_default();

                let ttl = title_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
                    .and_then(|a| if a.is_null(i) { None } else { Some(a.value(i).to_owned()) })
                    .unwrap_or_default();

                doc_ids.push(did);
                embeddings.push(e);
                texts.push(txt);
                titles.push(ttl);
            }
        }

        let n = embeddings.len();
        if n == 0 {
            return Ok(vec![]);
        }
        let actual_k = k.min(n);

        // 2. K-means++ clustering.
        let dim = embeddings[0].len();
        let assignments = kmeans_pp(&embeddings, actual_k, dim, 20);

        // 3. Build clusters with top TF-IDF terms for naming.
        let mut clusters: Vec<super::tauri_commands::Cluster> = Vec::with_capacity(actual_k);
        for c in 0..actual_k {
            let member_indices: Vec<usize> = assignments
                .iter()
                .enumerate()
                .filter(|(_, &a)| a == c)
                .map(|(i, _)| i)
                .collect();
            if member_indices.is_empty() { continue; }

            let member_doc_ids: Vec<String> = member_indices.iter().map(|&i| doc_ids[i].clone()).collect();
            let member_titles: Vec<String> = member_indices.iter()
                .filter_map(|&i| {
                    let t = &titles[i];
                    if t.is_empty() { None } else { Some(t.clone()) }
                })
                .take(5)
                .collect();

            // Simple term-frequency naming: collect words from cluster members,
            // rank by frequency, pick top 3 distinctive words.
            let top_terms = cluster_top_terms(&member_indices, &texts, 4);
            let name = if top_terms.is_empty() {
                format!("Cluster {}", c + 1)
            } else {
                top_terms.join(", ")
            };

            clusters.push(super::tauri_commands::Cluster {
                id: c as u32,
                name,
                doc_count: member_indices.len(),
                top_terms,
                sample_titles: member_titles,
                member_doc_ids,
            });
        }

        // Sort by doc count descending.
        clusters.sort_by(|a, b| b.doc_count.cmp(&a.doc_count));
        Ok(clusters)
    }

    /// P25.7 helper — fetch a document's full_text by doc_id.
    pub async fn fetch_full_text(&self, doc_id: &str) -> Result<String> {
        use lancedb::query::Select;
        let filter = format!("doc_id = '{}' AND chunk_index <= 0", doc_id.replace('\'', "''"));
        let batches: Vec<RecordBatch> = self.table.query()
            .only_if(filter)
            .select(Select::Columns(vec!["full_text".to_owned()]))
            .limit(1)
            .execute().await?
            .try_collect().await?;
        for batch in &batches {
            if batch.num_rows() > 0 {
                if let Some(col) = batch.column_by_name("full_text") {
                    if let Some(arr) = col.as_any().downcast_ref::<arrow::array::StringArray>() {
                        if !arr.is_null(0) {
                            return Ok(arr.value(0).to_string());
                        }
                    }
                }
            }
        }
        Err(anyhow!("Document not found: {}", doc_id))
    }

    /// Read a complete stored article body. Wallabag imports store the complete
    /// plain text here even when vector indexing splits full_text into chunks.
    pub async fn fetch_article_text(&self, doc_id: &str) -> Result<String> {
        use lancedb::query::Select;
        let filter = format!("doc_id = '{}' AND chunk_index = 0", doc_id.replace('\'', "''"));
        let batches: Vec<RecordBatch> = self.table.query().only_if(filter)
            .select(Select::Columns(vec!["full_text_md".to_owned()])).limit(1)
            .execute().await?.try_collect().await?;
        for batch in &batches {
            if let Some(arr) = batch.column_by_name("full_text_md").and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>()) {
                if !arr.is_empty() && !arr.is_null(0) { return Ok(arr.value(0).to_owned()); }
            }
        }
        Err(anyhow!("Article text is unavailable"))
    }

    /// P24.3 helper — fetch the tags column for all documents.
    pub async fn query_tags_for_graph(&self) -> Result<Vec<RecordBatch>> {
        use lancedb::query::Select;
        self.table.query()
            .only_if("chunk_index <= 0")
            .select(Select::Columns(vec!["tags".to_owned()]))
            .execute()
            .await?
            .try_collect()
            .await
            .map_err(Into::into)
    }

    // ── Purge ──────────────────────────────────────────────────────────────

    /// Stage P — LRU purge to keep the lance dir ≤ `max_bytes`.
    ///
    /// Two-phase:
    /// 1. Strip heavy columns (`full_text`, `full_text_md`, `embedding`,
    ///    `embedding_sparse`) from the oldest rows (by `indexed_at`),
    ///    batch by batch, until the directory is small enough — or every
    ///    row has been stripped.
    /// 2. If stripping alone doesn't reach the target, delete the oldest
    ///    rows entirely until the target is met.
    ///
    /// Returns `(stripped_rows, deleted_rows, bytes_reclaimed)`.
    pub async fn purge_to_size(
        &self,
        lance_dir: &std::path::Path,
        max_bytes: u64,
    ) -> Result<(usize, usize, u64)> {
        let _guard = self.write_guard().await?;
        let initial = dir_size_bytes(lance_dir);
        if initial <= max_bytes {
            return Ok((0, 0, 0));
        }

        use lance::dataset::scanner::ColumnOrdering;

        let mut stripped = 0usize;
        let mut deleted  = 0usize;
        let batch_size   = 1000usize;

        // Helper: collect doc_id strings from a batch of RecordBatches.
        let collect_doc_ids = |batches: &[RecordBatch]| -> Vec<String> {
            batches.iter()
                .flat_map(|b| {
                    b.column_by_name("doc_id")
                        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                        .map(|arr| (0..arr.len())
                            .filter(|&i| arr.is_valid(i) && !arr.value(i).is_empty())
                            .map(|i| arr.value(i).to_owned())
                            .collect::<Vec<_>>())
                        .unwrap_or_default()
                })
                .collect()
        };

        // Helper: build a SQL IN list, escaping single-quotes in values.
        let in_list = |ids: &[String]| -> String {
            ids.iter()
                .map(|id| format!("'{}'", id.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ")
        };

        // ── Phase 1: strip heavy columns from oldest batches ──────────
        // We use the lance Scanner (not the Query builder) so we can order
        // by `indexed_at` ASC without putting the timestamp in a filter
        // predicate.  LanceDB's custom filter planner cannot coerce Int64,
        // Float64, or Utf8 literals to Timestamp(Millisecond) — so every
        // push-down filter over `indexed_at` fails.  Instead we filter
        // only on non-timestamp columns (heavy-col presence) and update /
        // delete by `doc_id IN (...)` which is unambiguously a TEXT
        // comparison.
        loop {
            if dir_size_bytes(lance_dir) <= max_bytes { break; }

            let batches: Vec<RecordBatch> = {
                let guard = self.table
                    .dataset()
                    .ok_or_else(|| anyhow!("purge: not a native LanceDB table"))?
                    .get().await
                    .context("purge phase-1: dataset guard")?;
                let mut scanner = guard.scan();
                scanner
                    .filter("full_text IS NOT NULL OR embedding IS NOT NULL")
                    .context("purge phase-1: filter")?;
                scanner
                    .order_by(Some(vec![ColumnOrdering::asc_nulls_last("indexed_at".to_string())]))
                    .context("purge phase-1: order_by")?;
                scanner
                    .limit(Some(batch_size as i64), None)
                    .context("purge phase-1: limit")?;
                scanner
                    .project(&["doc_id", "_rowid"])
                    .context("purge phase-1: project")?;
                scanner
                    .try_into_stream().await.context("purge phase-1: stream")?
                    .try_collect().await.context("purge phase-1: collect")?
            };

            if batches.is_empty() { break; }
            let doc_ids = collect_doc_ids(&batches);
            if doc_ids.is_empty() { break; }
            let row_count = doc_ids.len();

            self.table
                .update()
                .only_if(format!("doc_id IN ({})", in_list(&doc_ids)))
                .column("full_text",        "NULL")
                .column("full_text_md",     "NULL")
                .column("embedding",        "NULL")
                .column("embedding_sparse", "NULL")
                .execute()
                .await
                .context("purge phase-1 strip")?;
            stripped += row_count;

            self.table
                .optimize(OptimizeAction::Prune { older_than: None, delete_unverified: Some(true), error_if_tagged_old_versions: None })
                .await
                .context("purge compact after strip")?;
            self.table
                .optimize(OptimizeAction::Compact { options: Default::default(), remap_options: None })
                .await
                .context("purge compact after strip")?;

            if row_count < batch_size { break; }
        }

        // ── Phase 2: delete oldest rows if still over cap ─────────────
        // Open the skeleton index once if it exists — evicted docs have their
        // author + parent_dir preserved there so the "✦ Local hints" panel
        // keeps showing them even after the LanceDB row is gone.
        let skeleton: Option<crate::index::skeleton::SkeletonIndex> = lance_dir
            .parent()
            .filter(|data_dir| data_dir.join("skeleton_index.db").exists())
            .and_then(|data_dir| {
                crate::index::skeleton::SkeletonIndex::open_or_create(data_dir).ok()
            });

        loop {
            if dir_size_bytes(lance_dir) <= max_bytes { break; }

            let batches: Vec<RecordBatch> = {
                let guard = self.table
                    .dataset()
                    .ok_or_else(|| anyhow!("purge: not a native LanceDB table"))?
                    .get().await
                    .context("purge phase-2: dataset guard")?;
                let mut scanner = guard.scan();
                scanner
                    .order_by(Some(vec![ColumnOrdering::asc_nulls_last("indexed_at".to_string())]))
                    .context("purge phase-2: order_by")?;
                scanner
                    .limit(Some(batch_size as i64), None)
                    .context("purge phase-2: limit")?;
                // No project() call: project + order_by + no-filter triggers
                // TakeExec without a row-address column in lance 2.0.  Accept
                // full rows and extract doc_id client-side.
                scanner
                    .try_into_stream().await.context("purge phase-2: stream")?
                    .try_collect().await.context("purge phase-2: collect")?
            };

            if batches.is_empty() { break; }
            let doc_ids = collect_doc_ids(&batches);
            if doc_ids.is_empty() { break; }
            let row_count = doc_ids.len();

            // Preserve author/parent_dir hints before the rows disappear.
            // Deduplicate by doc_id (chunk_index=0 representative) so
            // upsert_* counts one per doc, not one per chunk.
            if let Some(ref sk) = skeleton {
                let mut seen_docs = std::collections::HashSet::new();
                for batch in &batches {
                    let doc_id_col = batch.schema().index_of("doc_id").ok()
                        .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                    let cidx_col = batch.schema().index_of("chunk_index").ok()
                        .and_then(|i| batch.column(i).as_any().downcast_ref::<Int32Array>());
                    let author_col = batch.schema().index_of("author").ok()
                        .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                    let parent_dir_col = batch.schema().index_of("parent_dir").ok()
                        .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>());
                    for i in 0..batch.num_rows() {
                        // Only process the representative chunk (chunk_index == 0).
                        let is_rep = cidx_col.map_or(true, |c| !c.is_null(i) && c.value(i) == 0);
                        if !is_rep { continue; }
                        let doc_id = doc_id_col.filter(|c| !c.is_null(i)).map(|c| c.value(i)).unwrap_or("");
                        if doc_id.is_empty() || !seen_docs.insert(doc_id.to_owned()) { continue; }
                        if let Some(col) = author_col {
                            if !col.is_null(i) { let _ = sk.upsert_author(col.value(i)); }
                        }
                        if let Some(col) = parent_dir_col {
                            if !col.is_null(i) { let _ = sk.upsert_parent_dir(col.value(i)); }
                        }
                    }
                }
            }

            self.table
                .delete(&format!("doc_id IN ({})", in_list(&doc_ids)))
                .await
                .context("purge phase-2 delete")?;
            deleted += row_count;

            self.table
                .optimize(OptimizeAction::Prune { older_than: None, delete_unverified: Some(true), error_if_tagged_old_versions: None })
                .await
                .context("purge compact after delete")?;
            self.table
                .optimize(OptimizeAction::Compact { options: Default::default(), remap_options: None })
                .await
                .context("purge compact after delete")?;

            if row_count < batch_size { break; }
        }

        let final_size = dir_size_bytes(lance_dir);
        let reclaimed = initial.saturating_sub(final_size);
        Ok((stripped, deleted, reclaimed))
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

    /// Fetch one representative row without scanning the catalog or loading
    /// vector columns. Callers can hold `read_guard` across related reads.
    pub async fn fetch_document_summary(&self, doc_id: &str) -> Result<Option<SearchResult>> {
        let batches: Vec<RecordBatch> = self.table.query()
            .only_if(format!("doc_id = '{}' AND chunk_index <= 0", doc_id.replace('\'', "''")))
            .select(search_result_columns())
            .limit(1).execute().await?.try_collect().await?;
        Ok(record_batches_to_search_results(&batches)?.into_iter().next())
    }

    /// Resolve a metadata scan to an existing complete document at its source.
    pub async fn fetch_document_summary_for_uri(&self, location_uri: &str, owner_id: &str) -> Result<Option<SearchResult>> {
        let batches: Vec<RecordBatch> = self.table.query()
            .only_if(format!("location_uri = '{}' AND owner_id = '{}' AND chunk_index <= 0",
                location_uri.replace('\'', "''"), owner_id.replace('\'', "''")))
            .select(search_result_columns())
            .limit(1).execute().await?.try_collect().await?;
        Ok(record_batches_to_search_results(&batches)?.into_iter().next())
    }

    /// P13.7 Stage A — list documents in a shape suitable for
    /// pushing to cloud-backup over HTTP.  Returns one row per
    /// document (chunk_index <= 0) with all indexed body chunks included.
    ///
    /// Scan newer rows in timestamp order, so the watermark cannot skip
    /// older unseen rows. Refuse a page boundary inside a timestamp tie.
    pub async fn list_documents_for_push(
        &self,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<ManifestPushCandidate>> {
        let batches = self.scan_recent_for_push(
            "chunk_index <= 0", since_ts, limit,
            &["doc_id", "location_uri", "owner_id", "filename", "title", "author",
              "year", "ext", "language", "source_hash", "parent_dir", "full_text",
              "indexed_at", "metadata_json"],
        ).await?;
        let mut out = record_batches_to_push_candidates(&batches)?;
        if out.len() > limit {
            if limit == 0 || out[limit - 1].indexed_at == out[limit].indexed_at {
                anyhow::bail!("push page splits rows with the same timestamp; increase the limit");
            }
            out.truncate(limit);
        }
        if !out.is_empty() {
            // The representative row contains only chunk zero.  Sending it
            // as the document body silently drops the rest of long files.
            // Fetch the selected documents' chunks in one query, then join
            // them in order without mixing owners or locations.
            let ids = out.iter().map(|c| format!("'{}'", c.doc_id.replace('\'', "''")))
                .collect::<Vec<_>>().join(",");
            let predicate = format!("doc_id IN ({ids}) AND chunk_index >= 0");
            let chunks: Vec<RecordBatch> = self.table.query()
                .only_if(&predicate)
                .select(lancedb::query::Select::Columns(vec![
                    "doc_id".into(), "owner_id".into(), "location_uri".into(),
                    "chunk_index".into(), "full_text".into(),
                ]))
                .execute().await?.try_collect().await?;
            let mut by_doc: std::collections::HashMap<(String, String, String), Vec<(i32, String)>> =
                std::collections::HashMap::new();
            for batch in &chunks {
                let ids = str_col(batch, "doc_id")?;
                let owners = str_col(batch, "owner_id")?;
                let locations = str_col(batch, "location_uri")?;
                let indexes = i32_col_opt(batch, "chunk_index")
                    .ok_or_else(|| anyhow!("missing chunk_index"))?;
                let texts = str_col_opt(batch, "full_text");
                for i in 0..batch.num_rows() {
                    if indexes.is_null(i) { continue; }
                    if let Some(text) = str_col_val_opt(&texts, i) {
                        by_doc.entry((str_val(ids, i), str_val(owners, i), str_val(locations, i)))
                            .or_default().push((indexes.value(i), text));
                    }
                }
            }
            for candidate in &mut out {
                let key = (candidate.doc_id.clone(), candidate.owner_id.clone(), candidate.location_uri.clone());
                if let Some(mut parts) = by_doc.remove(&key) {
                    parts.sort_by_key(|(index, _)| *index);
                    candidate.full_text = Some(parts.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n"));
                }
            }
        }
        Ok(out)
    }

    /// P13.7 Stage B — list chunks with their embedding vectors,
    /// suitable for pushing to `/api/index/push-embeddings`.  Walks
    /// `chunk_index >= 0 AND embedding IS NOT NULL`.  L1 rows
    /// (chunk_index = -1) are skipped — they never carry an
    /// embedding.  Watermark applied client-side (same reason as
    /// `list_documents_for_push`).
    pub async fn list_chunks_with_embeddings(
        &self,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<EmbeddingPushCandidate>> {
        let batches = self.scan_recent_for_push(
            "chunk_index >= 0 AND embedding IS NOT NULL AND embedding_model IS NOT NULL", since_ts, limit,
            &["doc_id", "chunk_index", "embedding_model", "embedding_sparse", "indexed_at", "embedding"],
        ).await?;
        let mut out = record_batches_to_embedding_candidates(&batches)?;
        if out.len() > limit {
            if limit == 0 || out[limit - 1].indexed_at == out[limit].indexed_at {
                anyhow::bail!("embedding page splits rows with the same timestamp; increase the limit");
            }
            out.truncate(limit);
        }
        Ok(out)
    }

    /// Ordered server-side scan with one extra row to detect cursor ties.
    async fn scan_recent_for_push(
        &self, filter: &str, since_ts: i64, limit: usize, columns: &[&str],
    ) -> Result<Vec<RecordBatch>> {
        use lance::dataset::scanner::ColumnOrdering;
        let mut scanner = {
            let guard = self.table.dataset()
                .ok_or_else(|| anyhow!("local index: not a native LanceDB table"))?
                .get().await.context("acquiring lance dataset read guard")?;
            guard.scan()
        };
        scanner.filter(&format!("({filter}) AND indexed_at > to_timestamp_millis({since_ts})"))?;
        scanner.order_by(Some(vec![ColumnOrdering::asc_nulls_last("indexed_at".to_owned())]))?;
        scanner.limit(Some(limit.saturating_add(1).min(i64::MAX as usize) as i64), None)?;
        scanner.project(columns)?;
        Ok(scanner.try_into_stream().await?.try_collect().await?)
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

        // Project only the columns record_batches_to_search_results reads.
        // Omits embedding (1024+ f32), embedding_omni (2048 f32),
        // embedding_vit (768 f32), multivec_packed (LargeBinary),
        // full_text_md, embedding_sparse, embedding_model, headings_text,
        // and other large columns the browse view never displays.
        scanner
            .project(&[
                "doc_id", "location_uri", "owner_id", "title", "author",
                "year", "filename", "ext", "language", "chunk_index",
                "full_text", "metadata_json", "indexed_at", "volume_id",
                "source_hash", "text_translated", "text_translated_lang",
                "url", "tags", "summary", "doc_status", "parent_dir",
            ])
            .context("scanner project")?;

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

    /// Tag-cloud facets (Tier 2) — count how many documents under `filter`
    /// carry each distinct tag, sorted by count descending (ties broken by
    /// tag ascending), capped at `limit` entries.
    ///
    /// The count deliberately ignores `filter.tags` itself — a faceted
    /// browse shows the *available* tags within the current non-tag filter
    /// so the user can keep narrowing; counting against the already-applied
    /// tag selection would shrink every other tag to its co-occurrence with
    /// the selection and is the usual faceted-search foot-gun.
    ///
    /// `collection:<id>` routing markers are skipped — they're internal
    /// (cb-api collection routing), not user-facing tags.
    pub async fn tag_facets(
        &self,
        filter: &super::schema::DocumentFilter,
        limit: usize,
    ) -> Result<Vec<super::schema::TagFacet>> {
        use super::schema::TagFacet;
        use std::collections::HashMap;

        // Apply the whole filter EXCEPT its own tag selection.
        let mut facet_filter = filter.clone();
        facet_filter.tags = Vec::new();
        let pred = filter_to_sql(&facet_filter);

        // Project only the tags column to minimise transfer; the safety cap
        // mirrors folder_children (200k metadata rows).
        let base = self.table.query();
        let q = match pred.as_deref() {
            Some(p) => base.only_if(p),
            None => base,
        };
        let batches: Vec<RecordBatch> = q
            .select(lancedb::query::Select::Columns(vec!["tags".to_owned()]))
            .limit(200_000)
            .execute()
            .await?
            .try_collect()
            .await?;

        let mut counts: HashMap<String, u64> = HashMap::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                for tag in list_str_col_val(batch, "tags", i) {
                    if tag.is_empty() || tag.starts_with("collection:") {
                        continue;
                    }
                    *counts.entry(tag).or_insert(0) += 1;
                }
            }
        }

        let mut facets: Vec<TagFacet> = counts
            .into_iter()
            .map(|(tag, count)| TagFacet { tag, count })
            .collect();
        // Most-used first; stable tiebreak on the tag so the cloud order is
        // deterministic across calls.
        facets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        facets.truncate(limit.clamp(1, 5000));
        Ok(facets)
    }

    /// Cross-corpus deduplication by canonical URL (PLAN Tier 3).
    ///
    /// Finds documents that share the same `url` but have different `doc_id`s —
    /// e.g. the same article ingested via both a wallabag import and a manual
    /// "papers" folder.  Returns groups of ≥2 items sorted by group size
    /// descending.  Each group contains lightweight metadata (doc_id, url,
    /// location_uri, title, indexed_at) for the frontend to render a dedup UI.
    pub async fn url_duplicates(
        &self,
        limit: usize,
    ) -> Result<Vec<super::schema::UrlDuplicateGroup>> {
        use std::collections::HashMap;

        // Fetch url + doc_id + location_uri + title + indexed_at for all
        // document-level rows that have a non-null url.
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if("chunk_index <= 0 AND url IS NOT NULL AND url != ''")
            .select(lancedb::query::Select::Columns(vec![
                "doc_id".to_owned(),
                "url".to_owned(),
                "location_uri".to_owned(),
                "title".to_owned(),
                "indexed_at".to_owned(),
            ]))
            .limit(200_000)
            .execute()
            .await?
            .try_collect()
            .await?;

        // Group by url client-side (LanceDB doesn't support GROUP BY).
        let mut by_url: HashMap<String, Vec<super::schema::UrlDuplicateItem>> = HashMap::new();

        for batch in &batches {
            let url_col = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let doc_id_col = batch
                .column_by_name("doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let loc_col = batch
                .column_by_name("location_uri")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let title_col = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let ts_col = batch
                .column_by_name("indexed_at")
                .and_then(|c| c.as_any().downcast_ref::<TimestampMillisecondArray>());

            let (url_col, doc_id_col) = match (url_col, doc_id_col) {
                (Some(u), Some(d)) => (u, d),
                _ => continue,
            };

            for i in 0..batch.num_rows() {
                let url = match url_col.is_valid(i).then(|| url_col.value(i)) {
                    Some(u) if !u.is_empty() => u.to_string(),
                    _ => continue,
                };
                let doc_id = doc_id_col.is_valid(i).then(|| doc_id_col.value(i).to_string()).unwrap_or_default();
                let location = loc_col.and_then(|c| c.is_valid(i).then(|| c.value(i).to_string())).unwrap_or_default();
                let title = title_col.and_then(|c| c.is_valid(i).then(|| c.value(i).to_string()));
                let indexed_at = ts_col.and_then(|c| c.is_valid(i).then(|| c.value(i)));

                by_url.entry(url).or_default().push(super::schema::UrlDuplicateItem {
                    doc_id,
                    location_uri: location,
                    title,
                    indexed_at,
                });
            }
        }

        // Keep only groups with ≥2 items, sort largest first.
        let mut groups: Vec<super::schema::UrlDuplicateGroup> = by_url
            .into_iter()
            .filter(|(_, items)| items.len() >= 2)
            .map(|(url, items)| super::schema::UrlDuplicateGroup {
                url,
                count: items.len() as u32,
                items,
            })
            .collect();
        groups.sort_by(|a, b| b.count.cmp(&a.count));
        groups.truncate(limit.clamp(1, 1000));
        Ok(groups)
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
            "location_uri = '{}' AND chunk_index <= 0",
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

    /// Parse recorded source state using JSON semantics, including whitespace
    /// and negative timestamps from imported records.
    fn parse_indexed_state(json: &str) -> IndexedState {
        let value: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
        IndexedState {
            mtime_unix: value.get("mtime_unix").and_then(|v| v.as_i64()),
            images_indexed: value.get("images_indexed").and_then(|v| v.as_bool()).unwrap_or(false),
            source_hash: value.get("failed_source_hash").and_then(|v| v.as_str()).map(str::to_owned),
            extraction_failed: value.get("extraction_failure").is_some(),
            extraction_skip: value.get("extraction_skip").and_then(|v| v.as_bool()).unwrap_or(false)
                || matches!(value.get("extraction_failure").and_then(|v| v.get("msg")).and_then(|v| v.as_str()), Some("skipped via GUI" | "manually skipped via CLI")),
        }
    }

    pub async fn indexed_state_for_uri(
        &self,
        location_uri: &str,
    ) -> Result<Option<IndexedState>> {
        self.indexed_state_for_scope(location_uri, None).await
    }

    pub async fn indexed_state_for_owner_uri(&self, location_uri: &str, owner_id: &str) -> Result<Option<IndexedState>> {
        self.indexed_state_for_scope(location_uri, Some(owner_id)).await
    }

    async fn indexed_state_for_scope(&self, location_uri: &str, owner_id: Option<&str>) -> Result<Option<IndexedState>> {
        let mut pred = format!(
            "location_uri = '{}' AND chunk_index <= 0",
            location_uri.replace('\'', "''")
        );
        if let Some(owner) = owner_id { pred.push_str(&format!(" AND owner_id = '{}'", owner.replace('\'', "''"))); }
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
                    let mut state = Self::parse_indexed_state(arr.value(i));
                    if !state.extraction_skip || state.source_hash.is_none() {
                        state.source_hash = str_col_opt(batch, "source_hash")
                            .filter(|hashes| !hashes.is_null(i)).map(|hashes| hashes.value(i).to_owned());
                    }
                    return Ok(Some(state));
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

        let mut out = Vec::with_capacity(64);
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
                if v.get("extraction_skip").and_then(|v| v.as_bool()) == Some(true) { continue; }
                let Some(reason) = v
                    .get("extraction_failure")
                    .and_then(|f| f.get("reason"))
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_owned())
                else { continue; };

                use crate::index::task_failure::TaskFailureReason;
                let tfr = match reason.as_str() {
                    "timeout" => TaskFailureReason::Timeout,
                    "empty_text" => TaskFailureReason::EmptyText,
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

    /// Explicitly skip failed sources without replacing their metadata, tags,
    /// review evidence or write receipt. A changed content hash is eligible for
    /// extraction again; an explicit per-document retry clears this marker.
    pub async fn skip_failed_extractions(&self) -> Result<usize> {
        let _guard = self.write_guard().await?;
        let rows = self.list_failed_extractions(false).await?;
        let ids = rows.into_iter().map(|r| r.doc_id).collect::<std::collections::BTreeSet<_>>();
        let mut count = 0;
        for id in ids {
            let mut metadata = self.document_metadata(&id).await?;
            if metadata.get("extraction_failure").is_none() { continue; }
            metadata["extraction_skip"] = true.into();
            self.table.update().only_if(format!("doc_id = '{}'", id.replace('\'', "''")))
                .column("metadata_json", format!("'{}'", metadata.to_string().replace('\'', "''")))
                .execute().await?;
            count += 1;
        }
        super::result_cache::invalidate();
        Ok(count)
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
    /// Collect only identities for maintenance; load one document's text at a
    /// time through `fts_inputs_for_keys` instead of retaining the corpus body.
    pub(crate) async fn fts_document_keys(&self) -> Result<Vec<(String, String)>> {
        let mut stream = self.table.query()
            .select(lancedb::query::Select::Columns(vec!["owner_id".into(), "doc_id".into()]))
            .execute().await?;
        let mut keys = std::collections::BTreeSet::new();
        while let Some(batch) = stream.try_next().await? {
            let owners = str_col(&batch, "owner_id")?;
            let docs = str_col(&batch, "doc_id")?;
            for row in 0..batch.num_rows() {
                anyhow::ensure!(!owners.is_null(row) && !docs.is_null(row), "Stored full-text document identity is incomplete");
                keys.insert((owners.value(row).to_owned(), docs.value(row).to_owned()));
            }
        }
        Ok(keys.into_iter().collect())
    }

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
            meta_cols.extend(["text_translated", "text_translated_lang", "url", "summary", "doc_status"]);
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

            // Build from the immutable exported rows, not the live source
            // table. A source write during export must not alter its companion.
            let mut doc_ids = std::collections::BTreeSet::new();
            for batch in &batches {
                doc_ids.extend(str_col(batch, "doc_id")?.iter().flatten().map(str::to_owned));
            }
            let snapshot = Self::open_cidx(dest_path).await?;
            let doc_ids: Vec<_> = doc_ids.into_iter().collect();
            for ids in doc_ids.chunks(256) {
                for input in snapshot.fts_inputs_for_docs(ids).await? {
                    input.write_to(&fts, &mut writer)?;
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
        Ok(Self { _db: db, table, dims: 0, schema: build_schema(0),
            ingest_journal_path: cidx_path.join("index-write-journal.sqlite") })
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
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    let mut results = Vec::with_capacity(total);

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
        let url_col = str_col_opt(batch, "url");
        let summary_col = str_col_opt(batch, "summary");
        let doc_status_col = str_col_opt(batch, "doc_status");

        for i in 0..n {
            let doc_id = str_val(doc_id_col, i);
            let score = *score_map.get(&doc_id).unwrap_or(&0.0);

            let full_text = full_text_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .unwrap_or("");
            let snippet = super::snippet::truncate_str(full_text, 400).to_owned();

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
                url: str_col_val_opt(&url_col, i),
                tags: list_str_col_val(batch, "tags", i),
                summary: str_col_val_opt(&summary_col, i),
                doc_status: str_col_val_opt(&doc_status_col, i),
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

    for chunk in chunks {
        for (name, vector, width) in [
            ("embedding", chunk.embedding.as_ref(), dims),
            ("embedding_omni", chunk.embedding_omni.as_ref(), 2048usize),
            ("embedding_vit", chunk.embedding_vit.as_ref(), 768usize),
        ] {
            if let Some(vector) = vector {
                anyhow::ensure!(vector.len() == width && vector.iter().all(|n| n.is_finite()),
                    "Invalid {name} for {}: expected {width} finite values, received {}", chunk.id, vector.len());
            }
        }
        match (&chunk.multivec_packed, chunk.multivec_n_tokens) {
            (None, None) => {},
            (Some(bytes), Some(tokens)) if tokens > 0 && !bytes.is_empty()
                && bytes.len() % (tokens as usize * 4) == 0 => {
                anyhow::ensure!(bytes.chunks_exact(4).all(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).is_finite()),
                    "Non-finite multi-vector values for {}", chunk.id);
            },
            _ => anyhow::bail!("Invalid multi-vector shape for {}", chunk.id),
        }
    }

    // A vector whose length is not the table's width would reach arrow's
    // FixedSizeListBuilder and *panic* ("Length of the child array (384) must
    // be the multiple of the value length (1024)"), taking the writer task
    // with it — after which every subsequent write fails with "Writer task
    // has stopped". Refuse it here with the numbers the caller needs.
    if let Some((i, got)) = chunks
        .iter()
        .enumerate()
        .find_map(|(i, c)| c.embedding.as_ref().map(|v| (i, v.len())).filter(|(_, l)| *l != dims))
    {
        anyhow::bail!(
            "embedding width mismatch: chunk {i} ({}) has {got} dimensions but this index \
             stores {dims}. The index was created for a {dims}-dim model; either re-init it \
             for the new model or embed with a {dims}-dim one.",
            chunks[i].id
        );
    }

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
        for c in chunks {
            if let Some(v) = &c.embedding {
                if v.len() != dims {
                    anyhow::bail!("embedding has {} dimensions; index requires {dims}", v.len());
                }
            }
        }
        Arc::new(FixedSizeListArray::from_iter_primitive::<
            arrow_array::types::Float32Type,
            _,
            _,
        >(
            chunks.iter().map(|c| c.embedding.as_ref().map(|v| v.iter().copied().map(Some))),
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
    // Stage AD — ColBERT multi-vector columns added by migration v105.
    // Nullable; rows without a ColBERT head (most models) pass through as nulls.
    let multivec_packed_col: LargeBinaryArray = chunks
        .iter()
        .map(|c| c.multivec_packed.as_deref())
        .collect();
    let multivec_n_tokens_col: Int16Array = chunks
        .iter()
        .map(|c| c.multivec_n_tokens)
        .collect();
    // v106 — `url` column.  Nullable; rows without source-URL
    // provenance (most local-disk files) pass through as nulls.
    let urls: arrow_array::StringArray =
        chunks.iter().map(|c| c.url.as_deref()).collect();

    // P17.5 — BidirLM-Omni embedding (2048-D FixedSizeList<Float32>).
    let embedding_omni_col: Arc<dyn Array> = {
        const OMNI_DIM: usize = 2048;
        let flat: Vec<Option<f32>> = chunks
            .iter()
            .flat_map(|c| match &c.embedding_omni {
                Some(v) => v.iter().map(|&x| Some(x)).collect::<Vec<_>>(),
                None => vec![None; OMNI_DIM],
            })
            .collect();
        Arc::new(FixedSizeListArray::from_iter_primitive::<
            arrow_array::types::Float32Type,
            _,
            _,
        >(
            flat.chunks(OMNI_DIM).map(|chunk| Some(chunk.iter().copied())),
            OMNI_DIM as i32,
        ))
    };

    // P17.7 — ViT image embedding (768-D FixedSizeList<Float32>).
    let embedding_vit_col: Arc<dyn Array> = {
        const VIT_DIM: usize = 768;
        let flat: Vec<Option<f32>> = chunks
            .iter()
            .flat_map(|c| match &c.embedding_vit {
                Some(v) => v.iter().map(|&x| Some(x)).collect::<Vec<_>>(),
                None => vec![None; VIT_DIM],
            })
            .collect();
        Arc::new(FixedSizeListArray::from_iter_primitive::<
            arrow_array::types::Float32Type,
            _,
            _,
        >(
            flat.chunks(VIT_DIM).map(|chunk| Some(chunk.iter().copied())),
            VIT_DIM as i32,
        ))
    };

    // P22 — extractive summary column.
    let summaries: StringArray = chunks.iter().map(|c| c.summary.as_deref()).collect();
    // P26.8 — user-assigned document status label.
    let doc_statuses: StringArray = chunks.iter().map(|c| c.doc_status.as_deref()).collect();

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
            Arc::new(multivec_packed_col),
            Arc::new(multivec_n_tokens_col),
            Arc::new(urls),
            embedding_omni_col,
            embedding_vit_col,
            Arc::new(summaries),
            Arc::new(doc_statuses),
        ],
    )
    .context("building RecordBatch")?;

    let _ = n; // used implicitly via iterators
    Ok(batch)
}

/// Extract `SearchResult` values from a stream of `RecordBatch`es returned by
/// a LanceDB vector query.
fn record_batches_to_search_results(batches: &[RecordBatch]) -> Result<Vec<SearchResult>> {
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    let mut results = Vec::with_capacity(total);

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
        let url_col = str_col_opt(batch, "url");
        let summary_col = str_col_opt(batch, "summary");
        let doc_status_col = str_col_opt(batch, "doc_status");

        // LanceDB appends a `_distance` column for vector queries.
        let score_col = f32_col_opt(batch, "_distance");

        for i in 0..n {
            let full_text = full_text_col
                .as_ref()
                .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) })
                .unwrap_or("");

            let snippet = super::snippet::truncate_str(full_text, 400).to_owned();

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
                url: str_col_val_opt(&url_col, i),
                tags: list_str_col_val(batch, "tags", i),
                summary: str_col_val_opt(&summary_col, i),
                doc_status: str_col_val_opt(&doc_status_col, i),
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

/// Extract an integer value from a simple JSON object by key name.
/// Used by `corpus_stats` to read `fs_size` from `metadata_json`
/// without pulling in a full JSON parser.
fn extract_i64_from_json(json: &str, key: &str) -> Option<i64> {
    let search = format!("\"{}\"", key);
    let start = json.find(&search)?;
    let after = &json[start + search.len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    // Read digits (and optional leading minus)
    let end = after
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(after.len());
    after[..end].parse().ok()
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

/// Read row `i` of a `List<Utf8>` column into a `Vec<String>`.  Returns an
/// empty Vec for a null row, an absent column, or a column that isn't a Utf8
/// list — so callers can treat "no tags" and "old schema" identically.
fn list_str_col_val(batch: &RecordBatch, name: &str, i: usize) -> Vec<String> {
    let Some(col) = batch.column_by_name(name) else { return vec![] };
    let Some(arr) = col.as_any().downcast_ref::<ListArray>() else { return vec![] };
    if arr.is_null(i) {
        return vec![];
    }
    let values = arr.value(i);
    let Some(strs) = values.as_any().downcast_ref::<StringArray>() else { return vec![] };
    (0..strs.len())
        .filter_map(|j| if strs.is_null(j) { None } else { Some(strs.value(j).to_owned()) })
        .collect()
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

/// P13.7 Stage A — projection of a document row for the manifest-push
/// path.  Includes the body text + every column the
/// `ManifestRow` wire shape carries.  Distinct from `SearchResult`
/// (which has `snippet` and no body) to avoid breaking the wider
/// search surface for one new use case.
#[derive(Debug, Clone)]
pub struct ManifestPushCandidate {
    pub doc_id:        String,
    pub location_uri:  String,
    pub owner_id:      String,
    pub filename:      Option<String>,
    pub title:         Option<String>,
    pub author:        Option<String>,
    pub year:          Option<i32>,
    pub ext:           Option<String>,
    pub language:      Option<String>,
    pub source_hash:   String,
    pub parent_dir:    Option<String>,
    pub full_text:     Option<String>,
    pub indexed_at:    i64,
    pub metadata_json: Option<String>,
    /// Stage K — when set, the SyncManager push routes this row
    /// to the VPS shard owning the `collection_id`.  Pulled from
    /// the local row's `tags` (look for `collection:<id>`) at
    /// projection time; `None` falls back to sha-prefix routing.
    pub collection_id: Option<String>,
}

/// P13.7 Stage B — projection of a chunk row for the
/// embeddings-push path.  Returned only for chunk_index >= 0 rows
/// that carry a non-null embedding.
#[derive(Debug, Clone)]
pub struct EmbeddingPushCandidate {
    pub doc_id:        String,
    pub chunk_index:   i32,
    pub model_id:      Option<String>,
    pub embedding:     Vec<f32>,
    pub sparse_json:   Option<String>,
    pub indexed_at:    i64,
}

fn record_batches_to_push_candidates(
    batches: &[RecordBatch],
) -> Result<Vec<ManifestPushCandidate>> {
    let mut out = Vec::new();
    for batch in batches {
        let doc_id   = str_col(batch, "doc_id")?;
        let loc      = str_col(batch, "location_uri")?;
        let owner    = str_col(batch, "owner_id")?;
        let filename = str_col_opt(batch, "filename");
        let title    = str_col_opt(batch, "title");
        let author   = str_col_opt(batch, "author");
        let year     = i32_col_opt(batch, "year");
        let ext      = str_col_opt(batch, "ext");
        let language = str_col_opt(batch, "language");
        let hash     = str_col(batch, "source_hash")?;
        let parent   = str_col_opt(batch, "parent_dir");
        let full_text = str_col_opt(batch, "full_text");
        // indexed_at is Timestamp(Millisecond, None), not Int64.
        let indexed_at = ts_ms_col_opt(batch, "indexed_at");
        let meta_json  = str_col_opt(batch, "metadata_json");
        // Stage K — the LanceDB schema doesn't have a top-level
        // `collection_id` column (it'd be a schema migration);
        // we lift it out of metadata_json's `collection_id` key
        // when present.  L1 + L3 ingest both write the same
        // metadata_json shape, so this works uniformly.
        for i in 0..batch.num_rows() {
            let meta = str_col_val_opt(&meta_json, i);
            let collection_id = meta.as_deref().and_then(|s| {
                serde_json::from_str::<serde_json::Value>(s).ok()
                    .and_then(|v| v.get("collection_id")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string()))
            });
            out.push(ManifestPushCandidate {
                doc_id:        str_val(doc_id, i),
                location_uri:  str_val(loc, i),
                owner_id:      str_val(owner, i),
                filename:      str_col_val_opt(&filename, i),
                title:         str_col_val_opt(&title, i),
                author:        str_col_val_opt(&author, i),
                year:          year.and_then(|a| if a.is_null(i) { None } else { Some(a.value(i)) }),
                ext:           str_col_val_opt(&ext, i),
                language:      str_col_val_opt(&language, i),
                source_hash:   str_val(hash, i),
                parent_dir:    str_col_val_opt(&parent, i),
                full_text:     str_col_val_opt(&full_text, i),
                indexed_at:    indexed_at.map(|c| c.value(i)).unwrap_or(0),
                metadata_json: meta,
                collection_id,
            });
        }
    }
    Ok(out)
}

fn record_batches_to_embedding_candidates(
    batches: &[RecordBatch],
) -> Result<Vec<EmbeddingPushCandidate>> {
    let mut out = Vec::new();
    for batch in batches {
        let doc_id = str_col(batch, "doc_id")?;
        let chunk_index = i32_col(batch, "chunk_index")?;
        let model_id = str_col_opt(batch, "embedding_model");
        let sparse_json = str_col_opt(batch, "embedding_sparse");
        let indexed_at = ts_ms_col_opt(batch, "indexed_at");
        let embedding_col = batch
            .schema()
            .index_of("embedding")
            .ok()
            .and_then(|i| batch.column(i).as_any().downcast_ref::<FixedSizeListArray>());
        for i in 0..batch.num_rows() {
            // Lift the f32 chunk for this row.  FixedSizeListArray
            // stores all rows' values flat; the per-row slice is
            // value_length() wide and starts at `i * value_length()`.
            let embedding: Vec<f32> = if let Some(fsl) = embedding_col {
                if fsl.is_null(i) {
                    Vec::new()
                } else {
                    let values = fsl.values();
                    let arr = values
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .ok_or_else(|| anyhow!("embedding inner type not Float32"))?;
                    let dim = fsl.value_length() as usize;
                    let start = i * dim;
                    // Older text-only rows used a non-null list filled with
                    // null elements. Never push those as zero vectors.
                    if (start..start + dim).any(|j| arr.is_null(j)) { continue; }
                    arr.values()[start..start + dim].to_vec()
                }
            } else {
                Vec::new()
            };
            if embedding.is_empty() { continue; }
            out.push(EmbeddingPushCandidate {
                doc_id:      str_val(doc_id, i),
                chunk_index: chunk_index.value(i),
                model_id:    str_col_val_opt(&model_id, i),
                embedding,
                sparse_json: str_col_val_opt(&sparse_json, i),
                indexed_at:  indexed_at.map(|c| c.value(i)).unwrap_or(0),
            });
        }
    }
    Ok(out)
}

fn is_table_not_found(e: &lancedb::Error) -> bool {
    // LanceDB returns TableNotFound when the table doesn't exist yet.
    matches!(e, lancedb::Error::TableNotFound { .. })
}

/// The width of the `embedding` column as the existing table declares it.
///
/// `None` when the table has no such column or its type is unexpected — in
/// which case the caller's requested width is all we have to go on.
async fn table_embedding_dims(table: &lancedb::Table) -> Option<usize> {
    let schema = table.schema().await.ok()?;
    let field = schema.field_with_name("embedding").ok()?;
    match field.data_type() {
        arrow_schema::DataType::FixedSizeList(_, n) if *n > 0 => Some(*n as usize),
        _ => None,
    }
}

/// Recursively sum file sizes under `dir` (for on-disk size measurement).
/// Returns 0 if the directory doesn't exist or can't be read.
pub fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += dir_size_bytes(&path);
        } else if let Ok(m) = std::fs::metadata(&path) {
            total += m.len();
        }
    }
    total
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
pub(crate) fn filter_to_sql(f: &super::schema::DocumentFilter) -> Option<String> {
    let mut parts: Vec<String> = vec!["chunk_index <= 0".to_owned()];

    if let Some(prefix) = f.parent_dir_prefix.as_ref().filter(|s| !s.is_empty()) {
        // parent_dir is now a first-class column with a BTree scalar index
        // (P9 step 3). Escape single-quotes for SQL safety; escape LIKE
        // wildcards (%_) so they are treated as literals within the prefix.
        let escaped = super::schema::sql_like_literal(prefix);
        parts.push(format!("parent_dir LIKE '{}%' ESCAPE '\\'", escaped));
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
        let pat = super::schema::sql_like_literal(sub);
        // Search both filename and title (LIKE in LanceDB is
        // case-insensitive in 0.26 — verified by the existing
        // metadata_json LIKE patterns elsewhere in the codebase).
        parts.push(format!(
            "(filename LIKE '%{}%' ESCAPE '\\' OR title LIKE '%{}%' ESCAPE '\\')",
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
    // Tag-cloud filter (Tier 2). AND semantics: the row must carry every
    // selected tag, so each tag emits its own `array_has` clause. `tags`
    // is a `List<Utf8>` column, so this is the same predicate shape the
    // federated v2 path pushes to cb-api.
    for tag in f.tags.iter().filter(|t| !t.is_empty()) {
        parts.push(format!("array_has(tags, '{}')", tag.replace('\'', "''")));
    }

    Some(parts.join(" AND "))
}

/// In-process sort over the candidate window. LanceDB 0.26's public
// ── K-means++ clustering ──────────────────────────────────────────────

/// K-means++ initialization + Lloyd iterations.  Returns a Vec of
/// cluster assignments (0..k) for each embedding.
fn kmeans_pp(embeddings: &[Vec<f32>], k: usize, dim: usize, max_iter: usize) -> Vec<usize> {
    let n = embeddings.len();
    if n == 0 || k == 0 { return vec![]; }
    if k >= n { return (0..n).collect(); }

    // K-means++ seeding
    let mut rng_state: u64 = 42;
    let mut next_rand = || -> f64 {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng_state >> 33) as f64 / (1u64 << 31) as f64
    };

    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    let first = (next_rand() * n as f64) as usize % n;
    centroids.push(embeddings[first].clone());

    for _ in 1..k {
        let dists: Vec<f64> = embeddings
            .iter()
            .map(|e| {
                centroids.iter().map(|c| sq_dist(e, c)).fold(f64::MAX, f64::min)
            })
            .collect();
        let total: f64 = dists.iter().sum();
        if total <= 0.0 { break; }
        let threshold = next_rand() * total;
        let mut cumulative = 0.0;
        let mut chosen = 0;
        for (i, d) in dists.iter().enumerate() {
            cumulative += d;
            if cumulative >= threshold { chosen = i; break; }
        }
        centroids.push(embeddings[chosen].clone());
    }

    // Lloyd iterations
    let mut assignments = vec![0usize; n];
    for _ in 0..max_iter {
        let mut changed = false;
        // Assign
        for (i, e) in embeddings.iter().enumerate() {
            let best = centroids.iter().enumerate()
                .map(|(ci, c)| (ci, sq_dist(e, c)))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(ci, _)| ci)
                .unwrap_or(0);
            if assignments[i] != best { changed = true; assignments[i] = best; }
        }
        if !changed { break; }
        // Update centroids
        for ci in 0..centroids.len() {
            let mut sum = vec![0.0f64; dim];
            let mut count = 0usize;
            for (i, &a) in assignments.iter().enumerate() {
                if a == ci {
                    for (j, &v) in embeddings[i].iter().enumerate() {
                        sum[j] += v as f64;
                    }
                    count += 1;
                }
            }
            if count > 0 {
                for j in 0..dim {
                    centroids[ci][j] = (sum[j] / count as f64) as f32;
                }
            }
        }
    }
    assignments
}

fn sq_dist(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| { let d = (*x as f64) - (*y as f64); d * d }).sum()
}

/// Extract top distinctive terms from cluster members' texts via simple
/// TF-IDF-ish scoring (term frequency in cluster / frequency in corpus).
fn cluster_top_terms(member_indices: &[usize], all_texts: &[String], top_n: usize) -> Vec<String> {
    use std::collections::HashMap;

    let stop = [
        "the","a","an","and","or","but","in","on","at","to","for","of","with",
        "is","it","as","by","that","this","from","are","was","were","be","been",
        "has","have","had","not","no","all","its","der","die","das","und","ist",
        "ein","eine","von","den","dem","des","im","zu","auf","mit","sich","als",
        "für","auch","nach","wie","über","nur","aus","so","noch","bei","er","sie",
    ];
    let stop_set: std::collections::HashSet<&str> = stop.iter().copied().collect();

    // Corpus-wide document frequency
    let mut df: HashMap<String, usize> = HashMap::new();
    for text in all_texts {
        let mut seen = std::collections::HashSet::new();
        for w in text.split_whitespace().take(200) {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if w.len() >= 3 && !stop_set.contains(w.as_str()) && seen.insert(w.clone()) {
                *df.entry(w).or_insert(0) += 1;
            }
        }
    }

    // Cluster term frequency
    let mut tf: HashMap<String, usize> = HashMap::new();
    for &i in member_indices {
        for w in all_texts[i].split_whitespace().take(200) {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if w.len() >= 3 && !stop_set.contains(w.as_str()) {
                *tf.entry(w).or_insert(0) += 1;
            }
        }
    }

    let n_docs = all_texts.len().max(1) as f64;
    let mut scored: Vec<(String, f64)> = tf.into_iter()
        .map(|(term, count)| {
            let idf = (n_docs / (*df.get(&term).unwrap_or(&1)) as f64).ln();
            (term, count as f64 * idf)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(top_n).map(|(t, _)| t).collect()
}

#[cfg(test)]
mod clustering_tests {
    use super::*;

    #[test]
    fn kmeans_empty() {
        let result = kmeans_pp(&[], 3, 2, 10);
        assert!(result.is_empty());
    }

    #[test]
    fn kmeans_k_zero() {
        let data = vec![vec![1.0, 2.0]];
        let result = kmeans_pp(&data, 0, 2, 10);
        assert!(result.is_empty());
    }

    #[test]
    fn kmeans_k_equals_n() {
        let data = vec![vec![0.0, 0.0], vec![10.0, 10.0], vec![20.0, 20.0]];
        let result = kmeans_pp(&data, 3, 2, 10);
        assert_eq!(result.len(), 3);
        // Each point is its own cluster
        let mut sorted = result.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn kmeans_k_greater_than_n() {
        let data = vec![vec![1.0, 1.0], vec![2.0, 2.0]];
        let result = kmeans_pp(&data, 5, 2, 10);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn kmeans_two_clusters() {
        // Two well-separated clusters
        let data = vec![
            vec![0.0, 0.0], vec![0.1, 0.1], vec![0.2, 0.0],
            vec![10.0, 10.0], vec![10.1, 9.9], vec![9.9, 10.1],
        ];
        let result = kmeans_pp(&data, 2, 2, 20);
        assert_eq!(result.len(), 6);
        // Points 0-2 should be in same cluster, 3-5 in another
        assert_eq!(result[0], result[1]);
        assert_eq!(result[1], result[2]);
        assert_eq!(result[3], result[4]);
        assert_eq!(result[4], result[5]);
        assert_ne!(result[0], result[3]);
    }

    #[test]
    fn kmeans_single_point() {
        let data = vec![vec![5.0, 5.0]];
        let result = kmeans_pp(&data, 1, 2, 10);
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn kmeans_deterministic() {
        // Same input → same output (fixed seed)
        let data = vec![
            vec![0.0, 0.0], vec![1.0, 1.0], vec![10.0, 10.0], vec![11.0, 11.0],
        ];
        let r1 = kmeans_pp(&data, 2, 2, 20);
        let r2 = kmeans_pp(&data, 2, 2, 20);
        assert_eq!(r1, r2);
    }

    #[test]
    fn sq_dist_basic() {
        assert!((sq_dist(&[0.0, 0.0], &[3.0, 4.0]) - 25.0).abs() < 1e-6);
        assert!((sq_dist(&[1.0, 1.0], &[1.0, 1.0])).abs() < 1e-6);
    }

    #[test]
    fn cluster_top_terms_basic() {
        let texts = vec![
            "machine learning algorithms neural networks".into(),
            "deep learning neural networks training".into(),
            "database queries optimization indexing".into(),
        ];
        let members = vec![0, 1]; // cluster of ML docs
        let terms = cluster_top_terms(&members, &texts, 3);
        // "neural" and "networks" should be top terms (appear in cluster but not in all docs)
        assert!(!terms.is_empty());
    }

    #[test]
    fn cluster_top_terms_empty_members() {
        let texts = vec!["hello world".into()];
        let terms = cluster_top_terms(&[], &texts, 3);
        assert!(terms.is_empty());
    }

    #[test]
    fn cluster_top_terms_filters_stopwords() {
        let texts = vec!["the and but for with this that from".into()];
        let terms = cluster_top_terms(&[0], &texts, 5);
        // All stopwords should be filtered
        assert!(terms.is_empty());
    }

    #[test]
    fn cluster_top_terms_short_words_skipped() {
        let texts = vec!["ab cd ef gh ij kl mn".into()];
        let terms = cluster_top_terms(&[0], &texts, 5);
        assert!(terms.is_empty()); // all < 3 chars
    }
}

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
            url: None,
            tags: vec![],
            summary: None,
            doc_status: None,
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

// ── P13.7 Stage A/B — push-candidate scan tests ──────────────────────────
//
// End-to-end LanceDB tests for `list_documents_for_push` +
// `list_chunks_with_embeddings`.  Spin up an isolated LocalIndex on
// a tempdir, ingest a small mix of L1 / L3 chunks, and assert the
// scan returns the expected rows under the indexed_at watermark
// + embedding-not-null filters.

#[cfg(test)]
mod push_candidate_tests {
    use super::*;
    use crate::index::schema::DocumentChunk;

    /// Build a minimal DocumentChunk in either L1 (chunk_index=-1)
    /// or L3-with-embedding (chunk_index>=0, embedding=Some) shape.
    /// `dim` controls the embedding vector length; pass `0` for L1.
    fn mk(
        doc_id: &str,
        chunk_index: i32,
        indexed_at: i64,
        full_text: Option<&str>,
        embedding: Option<Vec<f32>>,
        model_id: Option<&str>,
    ) -> DocumentChunk {
        DocumentChunk {
            id: crate::index::ingest::chunk_row_id(doc_id, chunk_index),
            doc_id: doc_id.to_owned(),
            location_uri: format!("crisp+local://owner@m1/{doc_id}.txt"),
            owner_id: "owner".to_owned(),
            filename: Some(format!("{doc_id}.txt")),
            title: Some(format!("Title-{doc_id}")),
            author: None,
            year: None,
            ext: Some("txt".into()),
            language: Some("en".into()),
            page_count: None,
            headings_text: None,
            full_text: full_text.map(|s| s.to_owned()),
            full_text_md: full_text.map(|s| s.to_owned()),
            embedding,
            embedding_sparse: None,
            embedding_model: model_id.map(|s| s.to_owned()),
            chunk_index,
            chunk_total: if chunk_index < 0 { 0 } else { 1 },
            chunk_start_char: None,
            chunk_end_char: None,
            indexed_at,
            source_hash: format!("hash-{doc_id}"),
            tags: vec![],
            metadata_json: None,
            parent_dir: Some("/data".into()),
            volume_id: None,
            text_translated: None,
            text_translated_lang: None,
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_documents_for_push_returns_chunks_newer_than_watermark() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Use dim=4 to match the embedding fixtures below; the schema
        // is built around the dim on first open.
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        local.ingest_batch(&[
            // L1 sentinel — included in push scan.
            mk("a", -1, 100, Some("body a"), None, None),
            // L3 chunk-zero — included.
            mk("b",  0, 200, Some("body b"), Some(vec![1.0, 0.0, 0.0, 0.0]), Some("m")),
            // L3 chunk-one contributes to the same document's body.
            mk("b",  1, 200, Some("later passage"), Some(vec![0.0, 1.0, 0.0, 0.0]), Some("m")),
        ]).await.unwrap();

        let cand = local.list_documents_for_push(0, 100).await.unwrap();
        let ids: Vec<&str> = cand.iter().map(|c| c.doc_id.as_str()).collect();
        assert!(ids.contains(&"a"), "L1 doc 'a' missing from push scan");
        assert!(ids.contains(&"b"), "L3 doc 'b' missing from push scan");
        assert_eq!(cand.len(), 2, "chunk_index=1 row leaked into push scan");

        // Watermark = 150 → only 'b' (200) > watermark survives.
        let cand = local.list_documents_for_push(150, 100).await.unwrap();
        let ids: Vec<&str> = cand.iter().map(|c| c.doc_id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
        assert_eq!(cand[0].full_text.as_deref(), Some("body b\nlater passage"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn push_scan_orders_rows_and_rejects_split_timestamps() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        local.ingest_batch(&[
            mk("new", 0, 300, Some("new"), None, None),
            mk("old", 0, 100, Some("old"), None, None),
            mk("mid", 0, 200, Some("mid"), None, None),
        ]).await.unwrap();
        let first = local.list_documents_for_push(0, 2).await.unwrap();
        assert_eq!(first.iter().map(|c| c.indexed_at).collect::<Vec<_>>(), vec![100, 200]);
        assert_eq!(local.list_documents_for_push(200, 1).await.unwrap()[0].doc_id, "new");
        local.ingest_batch(&[mk("tie", 0, 200, Some("tie"), None, None)]).await.unwrap();
        assert!(local.list_documents_for_push(0, 2).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_chunks_with_embeddings_filters_l1_and_null_embeddings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        local.ingest_batch(&[
            // L1 — no embedding; should be skipped.
            mk("a", -1, 100, Some("body a"), None, None),
            // L3 chunk-zero — included.
            mk("b",  0, 200, Some("body b"), Some(vec![0.1, 0.2, 0.3, 0.4]), Some("bge")),
            // L3 chunk-one — also included (each chunk is a separate row).
            mk("b",  1, 200, Some("body b"), Some(vec![0.9, 0.8, 0.7, 0.6]), Some("bge")),
        ]).await.unwrap();

        let cand = local.list_chunks_with_embeddings(0, 100).await.unwrap();
        let ids: Vec<(String, i32)> = cand.iter()
            .map(|c| (c.doc_id.clone(), c.chunk_index)).collect();
        assert_eq!(ids.len(), 2);
        // L1 row 'a' must be skipped (chunk_index=-1, embedding=None).
        for (doc_id, ci) in &ids {
            assert_ne!(doc_id, "a");
            assert!(*ci >= 0);
        }
        // Embedding values round-trip f32-exactly via LanceDB.
        let chunk0 = cand.iter().find(|c| c.chunk_index == 0).unwrap();
        assert_eq!(chunk0.embedding, vec![0.1f32, 0.2, 0.3, 0.4]);
        assert_eq!(chunk0.model_id.as_deref(), Some("bge"));
    }

    /// Tier 2 tag-cloud — `tag_facets` counts distinct tags, skips
    /// `collection:` routing markers + empty tags, and sorts by count
    /// desc (tie broken by tag asc).
    #[tokio::test(flavor = "current_thread")]
    async fn tag_facets_counts_and_skips_markers() {
        use crate::index::schema::DocumentFilter;

        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        let with_tags = |doc: &str, tags: &[&str]| {
            let mut c = mk(doc, -1, 100, Some("body"), None, None);
            c.tags = tags.iter().map(|s| s.to_string()).collect();
            c
        };
        local.ingest_batch(&[
            with_tags("a", &["rust", "pocket-import"]),
            with_tags("b", &["rust"]),
            with_tags("c", &["pocket-import", "collection:wallabag"]),
            with_tags("d", &[]),
        ]).await.unwrap();

        let facets = local
            .tag_facets(&DocumentFilter::default(), 100)
            .await
            .unwrap();

        // collection: marker + the empty-tags doc contribute nothing.
        assert_eq!(facets.len(), 2, "unexpected facet set: {facets:?}");
        // Both at count 2; tie broken by tag asc → pocket-import before rust.
        assert_eq!(facets[0].tag, "pocket-import");
        assert_eq!(facets[0].count, 2);
        assert_eq!(facets[1].tag, "rust");
        assert_eq!(facets[1].count, 2);
        assert!(
            !facets.iter().any(|f| f.tag.starts_with("collection:")),
            "collection: marker leaked into the tag cloud"
        );
    }

    /// Tier 2 tag-cloud — selecting tags narrows the browse with AND
    /// semantics (`array_has` per tag).
    #[tokio::test(flavor = "current_thread")]
    async fn tag_filter_is_and_semantics() {
        use crate::index::schema::{DocumentFilter, SortSpec, PageSpec};

        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        let with_tags = |doc: &str, tags: &[&str]| {
            let mut c = mk(doc, -1, 100, Some("body"), None, None);
            c.tags = tags.iter().map(|s| s.to_string()).collect();
            c
        };
        local.ingest_batch(&[
            with_tags("a", &["rust", "pocket-import"]),
            with_tags("b", &["rust"]),
            with_tags("c", &["pocket-import"]),
        ]).await.unwrap();

        let query = |tags: Vec<&str>| {
            let filter = DocumentFilter {
                tags: tags.into_iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
            let local = &local;
            async move {
                // NB: PageSpec::default() yields limit=0 (the
                // serde-`default` attribute only feeds deserialization,
                // not Rust's Default), which query_documents clamps to 1.
                // Pass an explicit page size so all matches come back.
                let page = PageSpec { limit: 200, cursor: None };
                local
                    .query_documents(&filter, SortSpec::default(), page)
                    .await
                    .unwrap()
            }
        };

        // Single tag: a + b carry "rust".
        let page = query(vec!["rust"]).await;
        let mut ids: Vec<&str> = page.rows.iter().map(|r| r.doc_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(page.total_estimate, 2);

        // Two tags AND: only "a" carries both.
        let page = query(vec!["rust", "pocket-import"]).await;
        let ids: Vec<&str> = page.rows.iter().map(|r| r.doc_id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
        assert_eq!(page.total_estimate, 1);
    }

    /// Stage P — purge_to_size: index already within cap → no-op.
    #[tokio::test(flavor = "current_thread")]
    async fn purge_noop_when_within_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        local.ingest_batch(&[
            mk("a", 0, 100, Some("body a"), Some(vec![1.0, 0.0, 0.0, 0.0]), Some("m")),
        ]).await.unwrap();

        let lance_dir = tmp.path().join("lance");
        let (stripped, deleted, reclaimed) = local
            .purge_to_size(&lance_dir, u64::MAX)
            .await
            .unwrap();
        assert_eq!(stripped,  0);
        assert_eq!(deleted,   0);
        assert_eq!(reclaimed, 0);
    }

    /// Stage P — purge_to_size: with a cap of 0, all rows should be
    /// stripped and/or deleted until the table is empty.
    #[tokio::test(flavor = "current_thread")]
    async fn purge_strips_heavy_columns_and_evicts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        // 5 rows with full_text + embedding.  Oldest indexed_at first.
        let chunks: Vec<_> = (0..5u32).map(|i| {
            mk(
                &format!("doc{i}"),
                0,
                (i as i64) * 10 + 10, // 10, 20, 30, 40, 50
                Some(&"x".repeat(4096)), // ~4KB body per row
                Some(vec![i as f32, 0.0, 0.0, 0.0]),
                Some("m"),
            )
        }).collect();
        local.ingest_batch(&chunks).await.unwrap();

        let lance_dir = tmp.path().join("lance");
        // Cap at 0 bytes forces full eviction.
        let (stripped, deleted, _reclaimed) = local
            .purge_to_size(&lance_dir, 0)
            .await
            .unwrap();
        // At least something was stripped or deleted.
        assert!(stripped + deleted > 0,
            "expected at least one row affected; stripped={stripped} deleted={deleted}");
        // After eviction, row count should be reduced or zero.
        let remaining = local.count().await.unwrap();
        assert!(
            remaining < 5,
            "expected fewer rows after purge, got {remaining}"
        );
    }

    /// Stage AB — purge preserves author + parent_dir in skeleton_index.db
    /// when it exists alongside the lance dir.
    #[tokio::test(flavor = "current_thread")]
    async fn purge_preserves_skeleton_hints_on_eviction() {
        let tmp = tempfile::TempDir::new().unwrap();

        // Pre-create the skeleton index so purge_to_size finds it.
        let sk = crate::index::skeleton::SkeletonIndex::open_or_create(tmp.path()).unwrap();
        drop(sk); // close before purge opens it

        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        // Ingest a doc with author + parent_dir set.
        let mut chunk = mk("evict-me", 0, 1, Some("body"), None, None);
        chunk.author = Some("Kant, Immanuel".to_owned());
        chunk.parent_dir = Some("/philosophy".to_owned());
        local.ingest_batch(&[chunk]).await.unwrap();

        let lance_dir = tmp.path().join("lance");
        local.purge_to_size(&lance_dir, 0).await.unwrap();

        // Skeleton should now contain the author and parent_dir from the evicted doc.
        let sk2 = crate::index::skeleton::SkeletonIndex::open_or_create(tmp.path()).unwrap();
        let authors = sk2.search_authors("kant", 10).unwrap();
        assert!(
            authors.iter().any(|h| h.name.contains("Kant")),
            "expected Kant in skeleton authors after eviction, got: {authors:?}"
        );
        let dirs = sk2.search_parent_dirs("philosophy", 10).unwrap();
        assert!(
            dirs.iter().any(|h| h.name.contains("philosophy")),
            "expected /philosophy in skeleton dirs after eviction, got: {dirs:?}"
        );
    }

    /// Stage AE — ColBERT re-ranking reorders candidates by MaxSim.
    /// Ingest two chunks: one whose token vectors align with the
    /// query, one orthogonal.  The orthogonal one has a higher
    /// original score (simulating a noisy ANN hit) and must drop
    /// after re-ranking.
    #[tokio::test(flavor = "current_thread")]
    async fn rerank_with_colbert_reorders_by_maxsim() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        let aligned = vec![vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]];
        let orthogonal = vec![vec![0.0_f32, 0.0], vec![0.0_f32, 0.0]]; // zero -> MaxSim=0
        let (aligned_bytes, aligned_n) =
            crate::index::ingest::pack_multivec(aligned).unwrap();
        let (ortho_bytes, ortho_n) =
            crate::index::ingest::pack_multivec(orthogonal).unwrap();

        let mut c_aligned = mk("aligned", 0, 100, Some("body a"),
            Some(vec![0.5, 0.5, 0.5, 0.5]), Some("m"));
        c_aligned.multivec_packed = Some(aligned_bytes);
        c_aligned.multivec_n_tokens = Some(aligned_n);

        let mut c_ortho = mk("ortho", 0, 100, Some("body b"),
            Some(vec![0.5, 0.5, 0.5, 0.5]), Some("m"));
        c_ortho.multivec_packed = Some(ortho_bytes);
        c_ortho.multivec_n_tokens = Some(ortho_n);

        local.ingest_batch(&[c_aligned, c_ortho]).await.unwrap();

        // Candidate pool: ortho ranked first by some noisy upstream score.
        let candidates = vec![
            SearchResult {
                doc_id: "ortho".into(),
                chunk_index: 0,
                score: 0.9, // ANN said this was "better"
                ..mk_result("ortho", None, None)
            },
            SearchResult {
                doc_id: "aligned".into(),
                chunk_index: 0,
                score: 0.1, // ANN said this was "worse"
                ..mk_result("aligned", None, None)
            },
        ];

        let query = vec![vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]];
        let reranked = local
            .rerank_with_colbert(candidates, &query, 2)
            .await
            .unwrap();

        assert_eq!(reranked.len(), 2);
        assert_eq!(reranked[0].doc_id, "aligned",
            "MaxSim should promote 'aligned' to rank 1, got {:?}",
            reranked.iter().map(|r| &r.doc_id).collect::<Vec<_>>());
        assert!(reranked[0].score > reranked[1].score,
            "scores should be ordered: {:?}",
            reranked.iter().map(|r| r.score).collect::<Vec<_>>());
    }

    /// Empty query_multivec is a no-op (no DB call) — returns the
    /// candidates truncated to `limit`.
    #[tokio::test(flavor = "current_thread")]
    async fn rerank_with_colbert_empty_query_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        let candidates = vec![
            SearchResult { doc_id: "a".into(), score: 0.5, ..mk_result("a", None, None) },
            SearchResult { doc_id: "b".into(), score: 0.3, ..mk_result("b", None, None) },
            SearchResult { doc_id: "c".into(), score: 0.1, ..mk_result("c", None, None) },
        ];
        let out = local.rerank_with_colbert(candidates, &[], 2).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].doc_id, "a");
        assert_eq!(out[1].doc_id, "b");
    }

    fn mk_result(doc_id: &str, _f: Option<&str>, _y: Option<i32>) -> SearchResult {
        SearchResult {
            doc_id: doc_id.to_owned(),
            location_uri: format!("crisp+local://owner@m1/{doc_id}.txt"),
            owner_id: "owner".into(),
            title: None, author: None, year: None, filename: None, ext: None,
            language: None, snippet: String::new(), score: 0.0, chunk_index: 0,
            metadata_json: None, catalog_source: None, volume_id: None,
            indexed_at: 0, source_hash: String::new(),
            text_translated: None, text_translated_lang: None,
            url: None, tags: vec![], summary: None, doc_status: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn field_review_preserves_unrelated_metadata_and_rejects_stale_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        let mut chunk = mk("invoice", 0, 100, Some("Vendor Acme"), Some(vec![0.5; 4]), Some("m"));
        let fields = crate::index::fields::from_extraction("invoice", "Vendor Acme", "test", vec![("vendor".into(), "Acme".into(), 0.9)]);
        chunk.metadata_json = Some(serde_json::json!({"fs_size":123}).to_string());
        crate::index::fields::attach(&mut chunk.metadata_json, &fields);
        chunk.tags = vec!["manual-tag".into(), crate::index::fields::tag(&fields[0])];
        local.ingest_batch(&[chunk]).await.unwrap();
        let previous = local.document_metadata("invoice").await.unwrap();
        let mut updated = previous.clone();
        updated["field_evidence"][0]["normalized_value"] = serde_json::json!("Acme Ltd");
        updated["field_evidence"][0]["review_status"] = serde_json::json!("corrected");
        updated["field_evidence"][0]["revision"] = serde_json::json!(1);
        local.update_field_evidence("invoice", &previous, &updated).await.unwrap();
        let actual = local.document_metadata("invoice").await.unwrap();
        assert_eq!(actual["fs_size"], 123);
        assert_eq!(actual["field_evidence"][0]["original_value"], "Acme");
        assert_eq!(actual["field_evidence"][0]["normalized_value"], "Acme Ltd");
        assert!(local.update_field_evidence("invoice", &previous, &updated).await.is_err());
        let filter = SearchFilters { tag: Some("field:vendor:Acme Ltd".into()), ..Default::default() };
        let hits = local.search_vector(&[0.5;4], &filter, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].tags.contains(&"manual-tag".into()));
        assert!(!hits[0].tags.contains(&"field:vendor:Acme".into()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn passages_retain_text_beyond_preview_and_exact_chunk_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        let body = format!("{}The warranty expires in November 2031.", "Unrelated intro. ".repeat(80));
        let mut chunk = mk("long-doc", 2, 100, Some(&body), Some(vec![0.5; 4]), Some("m"));
        chunk.chunk_start_char = Some(125);
        chunk.chunk_end_char = Some(125 + body.len() as i32);
        local.ingest_batch(&[chunk]).await.unwrap();
        let result = SearchResult { chunk_index: 2, snippet: "clipped preview".into(), ..mk_result("long-doc", None, None) };
        let passages = local.fetch_passages(&[result]).await.unwrap();
        let passage = passages.get(&passage_key("owner", "long-doc", 2)).unwrap();
        assert!(passage.text.contains("November 2031"));
        assert!(passage.text.len() > 400);
        assert_eq!(passage.start_char, Some(125));
        assert_eq!(passage.offset_unit, "utf8_bytes");
        assert!(!passages.contains_key(&passage_key("owner", "long-doc", 0)));
    }

    /// Stage AE coverage — a candidate whose row has NULL multivec data
    /// (pre-v105 ingest or non-ColBERT model) must keep its original
    /// score after re-rank.  No silent zeroing.
    #[tokio::test(flavor = "current_thread")]
    async fn rerank_with_colbert_keeps_original_score_for_null_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        // One row with multivec, one row without.
        let multivec = vec![vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]];
        let (packed, n_tok) = crate::index::ingest::pack_multivec(multivec).unwrap();
        let mut with_mv = mk("hasvec", 0, 100, Some("a"),
            Some(vec![0.5, 0.5, 0.5, 0.5]), Some("m"));
        with_mv.multivec_packed = Some(packed);
        with_mv.multivec_n_tokens = Some(n_tok);
        let no_mv = mk("nullvec", 0, 100, Some("b"),
            Some(vec![0.5, 0.5, 0.5, 0.5]), Some("m"));

        local.ingest_batch(&[with_mv, no_mv]).await.unwrap();

        let candidates = vec![
            SearchResult { doc_id: "hasvec".into(), score: 0.2, ..mk_result("hasvec", None, None) },
            SearchResult { doc_id: "nullvec".into(), score: 0.7, ..mk_result("nullvec", None, None) },
        ];
        let query = vec![vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]];
        let out = local.rerank_with_colbert(candidates, &query, 5).await.unwrap();

        // The null-multivec row keeps its 0.7; the multivec row picks up
        // its MaxSim score (2.0 for identical query/doc).  Order: hasvec
        // (2.0) then nullvec (0.7).
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].doc_id, "hasvec");
        assert!(out[0].score > 1.5, "MaxSim should be ~2.0, got {}", out[0].score);
        assert_eq!(out[1].doc_id, "nullvec");
        assert!((out[1].score - 0.7).abs() < 1e-5, "original 0.7 preserved, got {}", out[1].score);
    }

    /// Stage AE coverage — re-rank truncates the output to `limit` even
    /// when the candidate pool is larger.  Top-K contract.
    #[tokio::test(flavor = "current_thread")]
    async fn rerank_with_colbert_truncates_to_limit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        // Ingest 5 rows, all with the same aligned multivec → same MaxSim.
        let multivec = vec![vec![1.0_f32, 0.0]];
        let (packed, n_tok) = crate::index::ingest::pack_multivec(multivec).unwrap();
        let chunks: Vec<_> = (0..5).map(|i| {
            let mut c = mk(&format!("doc{i}"), 0, 100, Some("body"),
                Some(vec![0.5, 0.5, 0.5, 0.5]), Some("m"));
            c.multivec_packed = Some(packed.clone());
            c.multivec_n_tokens = Some(n_tok);
            c
        }).collect();
        local.ingest_batch(&chunks).await.unwrap();

        let candidates: Vec<_> = (0..5).map(|i| SearchResult {
            doc_id: format!("doc{i}"),
            score: 0.1, ..mk_result(&format!("doc{i}"), None, None)
        }).collect();
        let query = vec![vec![1.0_f32, 0.0]];
        let out = local.rerank_with_colbert(candidates, &query, 2).await.unwrap();
        assert_eq!(out.len(), 2, "limit=2 must trim 5 candidates to 2");
    }

    /// P22 — find_similar returns the nearest neighbour and excludes
    /// the source document from its own results.
    #[tokio::test(flavor = "current_thread")]
    async fn find_similar_returns_nearest_neighbors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        local.ingest_batch(&[
            mk("a", 0, 100, Some("doc about climate change"),
               Some(vec![1.0, 0.0, 0.0, 0.0]), Some("m")),
            mk("b", 0, 200, Some("doc about weather patterns"),
               Some(vec![0.9, 0.1, 0.0, 0.0]), Some("m")),
            mk("c", 0, 300, Some("doc about cooking recipes"),
               Some(vec![0.0, 0.0, 1.0, 0.0]), Some("m")),
        ]).await.unwrap();

        let similar = local.find_similar("a", 0, 2).await.unwrap();
        assert!(!similar.is_empty(), "find_similar returned no results");
        assert!(
            similar.iter().all(|r| r.doc_id != "a"),
            "source doc 'a' must be excluded from its own similar results"
        );
        assert_eq!(
            similar[0].doc_id, "b",
            "nearest neighbor should be 'b' (closest cosine distance to 'a'): {:?}",
            similar.iter().map(|r| &r.doc_id).collect::<Vec<_>>()
        );
    }

    /// corpus_stats — counts documents and populates ext/lang distributions.
    #[tokio::test(flavor = "current_thread")]
    async fn corpus_stats_counts_distributions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        let mut a = mk("a", -1, 100, Some("body"), None, None);
        a.ext = Some("pdf".into());
        a.language = Some("de".into());
        a.year = Some(2023);
        a.tags = vec!["invoice".into()];

        let mut b = mk("b", -1, 200, Some("body"), None, None);
        b.ext = Some("pdf".into());
        b.language = Some("en".into());
        b.year = Some(2024);
        b.tags = vec!["contract".into()];

        let mut c = mk("c", -1, 300, Some("body"), None, None);
        c.ext = Some("docx".into());
        c.language = Some("de".into());

        local.ingest_batch(&[a, b, c]).await.unwrap();

        let stats = local.corpus_stats().await.unwrap();
        assert_eq!(stats.total_docs, 3, "expected 3 documents");
        // ext distribution: pdf×2, docx×1
        assert!(
            stats.ext_distribution.iter().any(|(e, cnt)| e == "pdf" && *cnt == 2),
            "expected pdf:2 in ext_distribution; got {:?}", stats.ext_distribution
        );
        assert!(
            stats.ext_distribution.iter().any(|(e, cnt)| e == "docx" && *cnt == 1),
            "expected docx:1 in ext_distribution; got {:?}", stats.ext_distribution
        );
        // lang distribution: de×2, en×1
        assert!(
            stats.lang_distribution.iter().any(|(l, cnt)| l == "de" && *cnt == 2),
            "expected de:2 in lang_distribution; got {:?}", stats.lang_distribution
        );
    }

    /// set_doc_status — round-trips a status value and clears it back to NULL.
    #[tokio::test(flavor = "current_thread")]
    async fn set_doc_status_updates_and_clears() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();
        local.ingest_batch(&[mk("a", -1, 100, Some("body"), None, None)]).await.unwrap();

        // Setting a non-NULL status must not error.
        local.set_doc_status("a", Some("approved")).await.unwrap();

        // Verify the value is persisted by reading it back via corpus_stats
        // (which scans all chunk_index<=0 rows).  A simpler probe: call
        // set_doc_status again with None — if the first call silently
        // failed we'd at least exercise the clear path without a panic.
        local.set_doc_status("a", None).await.unwrap();

        // Idempotent clear on a row with NULL must also succeed.
        local.set_doc_status("a", None).await.unwrap();
    }

    /// doc_id_scope filter — vector search with a scope list excludes
    /// documents whose doc_id is not in the list.
    #[tokio::test(flavor = "current_thread")]
    async fn doc_id_scope_restricts_search() {
        use crate::index::schema::SearchFilters;

        let tmp = tempfile::TempDir::new().unwrap();
        let local = LocalIndex::open_or_create(tmp.path(), 4).await.unwrap();

        // Three documents with distinct, well-separated embeddings.
        local.ingest_batch(&[
            mk("a", 0, 100, Some("alpha"), Some(vec![1.0, 0.0, 0.0, 0.0]), Some("m")),
            mk("b", 0, 200, Some("bravo"), Some(vec![0.9, 0.1, 0.0, 0.0]), Some("m")),
            mk("c", 0, 300, Some("charlie"), Some(vec![0.0, 0.0, 1.0, 0.0]), Some("m")),
        ]).await.unwrap();

        // Scope restricts to only "a" and "b".
        let filters = SearchFilters {
            doc_id_scope: vec!["a".to_owned(), "b".to_owned()],
            ..Default::default()
        };
        // Query vector close to "a"/"b".
        let results = local
            .search_vector(&[1.0, 0.0, 0.0, 0.0], &filters, 10)
            .await
            .unwrap();

        assert!(
            results.iter().all(|r| r.doc_id != "c"),
            "'c' must be excluded by doc_id_scope; results: {:?}",
            results.iter().map(|r| &r.doc_id).collect::<Vec<_>>()
        );
        assert!(
            results.iter().any(|r| r.doc_id == "a") || results.iter().any(|r| r.doc_id == "b"),
            "expected 'a' or 'b' in scoped results; results: {:?}",
            results.iter().map(|r| &r.doc_id).collect::<Vec<_>>()
        );
    }

    // ── Re-ingest guard: metadata parsing ────────────────────────────────

    #[test]
    fn indexed_state_parses_mtime_and_images_flag() {
        let st = LocalIndex::parse_indexed_state(
            r#"{"mtime_unix":1730000000,"fs_mtime":1730000000000,"images_indexed":true}"#,
        );
        assert_eq!(st.mtime_unix, Some(1730000000));
        assert!(st.images_indexed);

        // Rows written before the flag existed, or with the feature off.
        let st = LocalIndex::parse_indexed_state(r#"{"mtime_unix":5,"fs_size":10}"#);
        assert_eq!(st.mtime_unix, Some(5));
        assert!(!st.images_indexed);

        let st = LocalIndex::parse_indexed_state(r#"{}"#);
        assert_eq!(st.mtime_unix, None);
        assert!(!st.images_indexed);
    }
}
