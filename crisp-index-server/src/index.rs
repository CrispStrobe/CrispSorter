/// Server-side index: LanceDB vector store + Tantivy FTS.
///
/// The client (CrispSorter) is responsible for chunking and embedding.
/// This server receives pre-computed dense vectors alongside the plain text and
/// stores both.  Full-text search (BM25) runs on the stored text; ANN search
/// uses the stored vectors.  Hybrid search merges both with RRF (k = 60).
///
/// Schema columns mirror the CrispSorter client exactly so that Lance files
/// can be moved between local and remote configurations without conversion.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

pub use crate::fts::FtsStore;
use crate::fts::{DocumentKey, FtsDocument, FtsHit, PreparedSearch, StoredChunk};
use anyhow::{anyhow, ensure, Context, Result};
use arrow_array::{
    builder::{ListBuilder, StringBuilder},
    Array, FixedSizeListArray, Float32Array, Int32Array, RecordBatch, StringArray,
    TimestampMillisecondArray, UInt32Array, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use crisp_index_protocol::{IngestBatch, IngestChunk, SearchFilters, SearchHit};
use futures_util::TryStreamExt;
use lancedb::{
    connect,
    index::{vector::IvfPqIndexBuilder, Index},
    query::{ExecutableQuery, QueryBase, Select},
    Connection, DistanceType, Table,
};

const MAX_AFFECTED_CHUNKS: usize = 65_536;

#[derive(Debug)]
pub struct AmbiguousDocument;
impl std::fmt::Display for AmbiguousDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("This selection represents multiple owners, document identities, or source locations; resolve the exact occurrence before changing it")
    }
}
impl std::error::Error for AmbiguousDocument {}

struct SourceRemoval {
    key: DocumentKey,
    location_uri: String,
}

struct PreparedWrite {
    keys: Vec<DocumentKey>,
    batches: Vec<RecordBatch>,
    documents: Vec<(DocumentKey, Option<FtsDocument>)>,
}

// ── Schema ────────────────────────────────────────────────────────────────────

const TABLE_NAME: &str = "documents";
const EMB_FIELD: &str = "embedding";
// Default dims — overridden by EMBED_DIMS env var at startup.
#[allow(dead_code)]
const DIMS_DEFAULT: usize = 1024;

pub fn build_lance_schema(dims: usize) -> Arc<Schema> {
    let emb_field = Field::new(
        EMB_FIELD,
        DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float32, true)),
            dims as i32,
        ),
        true,
    );

    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("doc_id", DataType::Utf8, false),
        Field::new("location_uri", DataType::Utf8, false),
        Field::new("owner_id", DataType::Utf8, false),
        Field::new("filename", DataType::Utf8, true),
        Field::new("title", DataType::Utf8, true),
        Field::new("author", DataType::Utf8, true),
        Field::new("year", DataType::Int32, true),
        Field::new("ext", DataType::Utf8, true),
        Field::new("language", DataType::Utf8, true),
        Field::new("page_count", DataType::Int32, true),
        Field::new("headings_text", DataType::Utf8, true),
        Field::new("full_text", DataType::Utf8, true),
        Field::new("full_text_md", DataType::Utf8, true),
        emb_field,
        Field::new("embedding_sparse", DataType::Utf8, true),
        Field::new("embedding_model", DataType::Utf8, true),
        Field::new("chunk_index", DataType::Int32, false),
        Field::new("chunk_total", DataType::Int32, false),
        Field::new("chunk_start_char", DataType::Int32, true),
        Field::new("chunk_end_char", DataType::Int32, true),
        Field::new(
            "indexed_at",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("source_hash", DataType::Utf8, false),
        Field::new(
            "tags",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new("metadata_json", DataType::Utf8, true),
    ]))
}

// Wire-format types (`IngestChunk`, `SearchHit`, `SearchFilters`) live in
// the `crisp-index-protocol` crate and are imported above. Internally we
// produce `SearchHit`s directly — they're a strict subset of CrispSorter's
// richer client-side `SearchResult` (which carries optional metadata fields
// the server doesn't populate yet). See
// `../../crisp-index-protocol/src/lib.rs` for per-field documentation.

// ── VectorStore ───────────────────────────────────────────────────────────────

pub struct VectorStore {
    _db: Connection,
    table: Table,
    pub dims: usize,
}

impl VectorStore {
    pub async fn open_or_create(data_dir: &Path, dims: usize) -> Result<Self> {
        ensure!(
            dims > 0 && dims <= i32::MAX as usize,
            "Invalid server embedding dimension"
        );
        let lance_dir = data_dir.join("lance");
        std::fs::create_dir_all(&lance_dir)?;
        let uri = lance_dir.to_string_lossy().into_owned();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("LanceDB connect {uri}"))?;

        let table = match db.open_table(TABLE_NAME).execute().await {
            Ok(t) => t,
            Err(e) if is_table_not_found(&e) => {
                let schema = build_lance_schema(dims);
                db.create_empty_table(TABLE_NAME, schema)
                    .execute()
                    .await
                    .context("create LanceDB table")?
            }
            Err(e) => return Err(e).context("open LanceDB table"),
        };

        ensure!(table.schema().await?.as_ref() == build_lance_schema(dims).as_ref(),
            "Stored vector schema or embedding dimension differs from this server; preserve the existing data and use a compatible configuration");
        Ok(VectorStore {
            _db: db,
            table,
            dims,
        })
    }

    pub(crate) fn table(&self) -> &Table {
        &self.table
    }

    /// Resolve only explicitly marked ordinary uploads. Unmarked external IDs
    /// remain authoritative. Source lookup and row publication share the gate.
    async fn resolve_source_identities(
        &self,
        chunks: &[IngestChunk],
    ) -> Result<(Vec<IngestChunk>, Vec<SourceRemoval>)> {
        let mut normalized = chunks.to_vec();
        let mut groups = BTreeMap::<(String, String, String), Vec<usize>>::new();
        for (position, chunk) in chunks.iter().enumerate() {
            if let Some(version) = chunk.source_identity_version {
                ensure!(version == 1, "Unsupported source identity version");
                validate_chunk(chunk, self.dims)?;
                let canonical = crate::source_identity::occurrence_id(
                    &chunk.owner_id,
                    &chunk.location_uri,
                    &chunk.source_hash,
                );
                ensure!(
                    chunk.doc_id == canonical,
                    "Generated source identity does not match its owner and source URI"
                );
                groups
                    .entry((
                        chunk.owner_id.clone(),
                        chunk.location_uri.clone(),
                        canonical,
                    ))
                    .or_default()
                    .push(position);
            }
        }
        let mut removals = Vec::new();
        for ((owner, uri, canonical), positions) in groups {
            ensure!(
                !uri.is_empty(),
                "Generated per-file uploads require a concrete source URI"
            );
            let first = &chunks[positions[0]];
            let total = first.chunk_total.ok_or_else(|| {
                anyhow!("Generated source uploads require a complete declared chunk sequence")
            })?;
            let indices: HashSet<_> = positions
                .iter()
                .map(|position| chunks[*position].chunk_index)
                .collect();
            ensure!(
                positions.len() == total as usize
                    && indices.len() == positions.len()
                    && positions
                        .iter()
                        .all(|position| chunks[*position].chunk_total == Some(total)
                            && chunks[*position].source_hash == first.source_hash),
                "Generated source uploads require all chunks from one complete source version"
            );
            let mut selected = canonical.clone();
            if !uri.is_empty() {
                let filter = format!(
                    "owner_id='{}' AND location_uri='{}'",
                    sql_escape(&owner),
                    sql_escape(&uri)
                );
                let batches: Vec<RecordBatch> = self
                    .table
                    .query()
                    .only_if(filter)
                    .select(Select::columns(&["doc_id"]))
                    .limit(MAX_AFFECTED_CHUNKS + 1)
                    .execute()
                    .await?
                    .try_collect()
                    .await?;
                ensure!(
                    batches.iter().map(RecordBatch::num_rows).sum::<usize>() <= MAX_AFFECTED_CHUNKS,
                    "Source has too many stored chunks to resolve safely"
                );
                let mut ids = BTreeSet::new();
                for batch in &batches {
                    let ids_column = string_column(batch, "doc_id")?;
                    for row in 0..batch.num_rows() {
                        ids.insert(ids_column.value(row).to_owned());
                    }
                }
                let canonical_at_source = ids.contains(&canonical);
                if canonical_at_source {
                    selected = canonical.clone();
                } else if ids.len() == 1 {
                    let legacy = DocumentKey {
                        owner_id: owner.clone(),
                        doc_id: ids.into_iter().next().expect("one ID"),
                    };
                    let elsewhere = format!(
                        "({}) AND location_uri!='{}'",
                        keys_sql(std::slice::from_ref(&legacy)),
                        sql_escape(&uri)
                    );
                    if self.table.count_rows(Some(elsewhere)).await? == 0 {
                        selected = legacy.doc_id;
                    } else {
                        removals.push(SourceRemoval {
                            key: legacy,
                            location_uri: uri.clone(),
                        });
                    }
                } else {
                    ensure!(ids.is_empty(),"Several legacy document IDs claim this owner and source URI; resolve the occurrence before replacing it");
                }
                // A moved occurrence can retain a URI-derived ID. If that old
                // address is reused later, allocate a separate stable receipt ID.
                if selected == canonical {
                    let canonical_key = DocumentKey {
                        owner_id: owner.clone(),
                        doc_id: canonical.clone(),
                    };
                    let elsewhere = format!(
                        "({}) AND location_uri!='{}'",
                        keys_sql(std::slice::from_ref(&canonical_key)),
                        sql_escape(&uri)
                    );
                    if self.table.count_rows(Some(elsewhere)).await? > 0 {
                        if canonical_at_source {
                            removals.push(SourceRemoval {
                                key: canonical_key,
                                location_uri: uri.clone(),
                            });
                        }
                        use sha2::Digest;
                        selected = format!(
                            "{:x}",
                            sha2::Sha256::digest(uuid::Uuid::new_v4().as_bytes())
                        );
                    }
                }
            }
            for position in positions {
                normalized[position].doc_id = selected.clone();
            }
        }
        Ok((normalized, removals))
    }

    /// Validate and stage complete affected documents before any persistent write.
    /// Existing duplicate rows are collapsed by latest timestamp. A declared,
    /// complete chunk sequence replaces old tails; partial chunk updates retain
    /// untouched siblings. The eventual merge publishes all changes atomically.
    async fn prepare_upsert(&self, chunks: &[IngestChunk]) -> Result<PreparedWrite> {
        self.prepare_upsert_scoped(chunks, &[]).await
    }

    async fn prepare_upsert_scoped(
        &self,
        chunks: &[IngestChunk],
        removals: &[SourceRemoval],
    ) -> Result<PreparedWrite> {
        ensure!(
            !chunks.is_empty() && chunks.len() <= 4096,
            "An ingest batch must contain between 1 and 4096 chunks"
        );
        let mut incoming = HashSet::new();
        let mut groups = BTreeMap::<DocumentKey, Vec<&IngestChunk>>::new();
        for chunk in chunks {
            validate_chunk(chunk, self.dims)?;
            let key = DocumentKey {
                owner_id: chunk.owner_id.clone(),
                doc_id: chunk.doc_id.clone(),
            };
            ensure!(
                incoming.insert((key.clone(), chunk.chunk_index)),
                "An ingest batch contains duplicate chunk identities"
            );
            groups.entry(key).or_default().push(chunk);
        }
        let keys: Vec<_> = groups
            .keys()
            .cloned()
            .chain(removals.iter().map(|removal| removal.key.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut complete = HashSet::new();
        for (key, group) in &groups {
            ensure!(
                group
                    .iter()
                    .all(|chunk| same_document_metadata(group[0], chunk)),
                "Chunks disagree about document metadata"
            );
            let totals: HashSet<_> = group.iter().filter_map(|chunk| chunk.chunk_total).collect();
            ensure!(
                totals.len() <= 1,
                "Chunks disagree about the document's total chunk count"
            );
            if let Some(&total) = totals.iter().next() {
                if group.len() == total as usize
                    && group.iter().all(|chunk| chunk.chunk_total == Some(total))
                {
                    complete.insert(key.clone());
                }
            }
        }
        let existing = self.fetch_by_keys(&keys).await?;
        let mut latest = HashMap::<(DocumentKey, i32), (usize, usize, i64, u64)>::new();
        for (batch_index, batch) in existing.iter().enumerate() {
            for (row, chunk) in stored_chunks(batch)?.into_iter().enumerate() {
                let identity = (chunk.key.clone(), chunk.chunk_index);
                let location = string_column(batch, "location_uri")?.value(row);
                if removals
                    .iter()
                    .any(|removal| removal.key == chunk.key && removal.location_uri == location)
                {
                    continue;
                }
                if complete.contains(&chunk.key) || incoming.contains(&identity) {
                    continue;
                }
                if latest.get(&identity).is_none_or(|(_, _, time, row_order)| {
                    (*time, *row_order) <= (chunk.indexed_at, chunk.row_order)
                }) {
                    latest.insert(
                        identity,
                        (batch_index, row, chunk.indexed_at, chunk.row_order),
                    );
                }
            }
        }
        let generation = uuid::Uuid::new_v4().to_string();
        let mut batches = Vec::new();
        for (batch_index, batch) in existing.iter().enumerate() {
            let mut rows: Vec<_> = latest
                .values()
                .filter(|(index, _, _, _)| *index == batch_index)
                .map(|(_, row, _, _)| *row as u32)
                .collect();
            rows.sort_unstable();
            if rows.is_empty() {
                continue;
            }
            let selection = UInt32Array::from(rows);
            let columns = batch
                .columns()
                .iter()
                .map(|column| arrow_select::take::take(column, &selection, None))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            batches.push(with_generation(
                RecordBatch::try_new(batch.schema(), columns)?,
                &generation,
            )?);
        }
        let schema = build_lance_schema(self.dims);
        for chunk in chunks {
            batches.push(with_generation(
                chunk_to_record_batch(chunk, self.dims, &schema)?,
                &generation,
            )?);
        }
        ensure!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>() <= MAX_AFFECTED_CHUNKS,
            "Affected documents exceed the 65536-chunk safety limit; split the request"
        );
        for batch in &mut batches {
            *batch = propagate_document_metadata(batch, &groups)?;
        }
        let documents = documents_from_batches(&batches, &keys)?;
        Ok(PreparedWrite {
            keys,
            batches,
            documents,
        })
    }

    async fn commit_upsert(&self, write: &PreparedWrite) -> Result<()> {
        let schema = build_lance_schema(self.dims);
        let reader = arrow_array::RecordBatchIterator::new(
            write
                .batches
                .clone()
                .into_iter()
                .map(Ok::<RecordBatch, arrow_schema::ArrowError>),
            schema,
        );
        // New generation metadata never matches old rows. Insert the replacement
        // and remove the old affected rows in one Lance transaction, including
        // legacy duplicate identities and stale trailing chunks.
        let mut merge = self
            .table
            .merge_insert(&["id", "owner_id", "metadata_json"]);
        merge
            .when_not_matched_insert_all()
            .when_not_matched_by_source_delete(Some(keys_sql(&write.keys)));
        merge
            .execute(Box::new(reader))
            .await
            .context("Publish server document replacement")?;
        Ok(())
    }

    /// ANN vector search.  Converts cosine distance → similarity score.
    pub async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut q = self
            .table
            .vector_search(embedding)?
            .distance_type(DistanceType::Cosine)
            .limit(limit);

        if let Some(sql) = filters.to_lance_sql() {
            q = q.only_if(sql);
        }

        let batches: Vec<RecordBatch> = q.execute().await?.try_collect().await?;
        batches_to_results(&batches, None, /* use distance */ true)
    }

    /// Fetch all rows for explicitly owner-scoped identities. Never cap this at
    /// a small multiplier of document count: long documents can have many chunks.
    async fn fetch_by_keys(&self, keys: &[DocumentKey]) -> Result<Vec<RecordBatch>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        ensure!(
            keys.len() <= 4096,
            "Too many document identities requested at once"
        );
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(keys_sql(keys))
            .with_row_id()
            .limit(MAX_AFFECTED_CHUNKS + 1)
            .execute()
            .await?
            .try_collect()
            .await?;
        ensure!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>() <= MAX_AFFECTED_CHUNKS,
            "Affected documents exceed the 65536-chunk safety limit; split the request"
        );
        Ok(batches)
    }

    /// Resolve scalar predicates against every chunk without loading bodies or vectors.
    async fn matching_keys_for_filters(&self, keys: &[DocumentKey], filters: &SearchFilters) -> Result<HashSet<DocumentKey>> {
        if keys.is_empty() { return Ok(HashSet::new()); }
        let mut predicate = keys_sql(keys);
        if let Some(extra) = filters.to_lance_sql() {
            predicate = format!("({predicate}) AND ({extra})");
        }
        let mut stream = self.table.query().only_if(predicate)
            .select(Select::columns(&["owner_id", "doc_id"]))
            .execute().await?;
        let mut allowed = HashSet::new();
        while let Some(batch) = stream.try_next().await? {
            let owners = string_column(&batch, "owner_id")?;
            let ids = string_column(&batch, "doc_id")?;
            for row in 0..batch.num_rows() {
                allowed.insert(DocumentKey { owner_id: owners.value(row).into(), doc_id: ids.value(row).into() });
            }
        }
        Ok(allowed)
    }

    async fn keys_for_doc(&self, doc_id: &str) -> Result<Vec<DocumentKey>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(format!("doc_id='{}'", sql_escape(doc_id)))
            .select(Select::columns(&["owner_id", "doc_id"]))
            .execute()
            .await?
            .try_collect()
            .await?;
        let mut keys = BTreeSet::new();
        for batch in &batches {
            let owners = string_column(batch, "owner_id")?;
            let ids = string_column(batch, "doc_id")?;
            for row in 0..batch.num_rows() {
                keys.insert(DocumentKey {
                    owner_id: owners.value(row).into(),
                    doc_id: ids.value(row).into(),
                });
            }
        }
        Ok(keys.into_iter().collect())
    }

    async fn keys_for_location(&self, uri: &str) -> Result<Vec<DocumentKey>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(format!("location_uri='{}'", sql_escape(uri)))
            .select(Select::columns(&["owner_id", "doc_id"]))
            .limit(MAX_AFFECTED_CHUNKS + 1)
            .execute()
            .await?
            .try_collect()
            .await?;
        ensure!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>() <= MAX_AFFECTED_CHUNKS,
            "Source location exceeds the 65536-chunk safety limit"
        );
        let mut keys = BTreeSet::new();
        for batch in &batches {
            let owners = string_column(batch, "owner_id")?;
            let ids = string_column(batch, "doc_id")?;
            for row in 0..batch.num_rows() {
                keys.insert(DocumentKey {
                    owner_id: owners.value(row).into(),
                    doc_id: ids.value(row).into(),
                });
            }
        }
        Ok(keys.into_iter().collect())
    }

    /// Stream a stable Lance snapshot to disk, prepare the complete FTS view,
    /// and remove legacy duplicate vector rows before publishing either view.
    async fn rebuild_fts(&self, fts: &FtsStore) -> Result<()> {
        let spool_path = fts.rebuild_spool_path();
        let mut spool = rusqlite::Connection::open(&spool_path)?;
        spool.execute_batch("PRAGMA synchronous=FULL;
            CREATE TABLE chunks(owner_id TEXT NOT NULL,doc_id TEXT NOT NULL,chunk_index INTEGER NOT NULL,indexed_at INTEGER NOT NULL,row_order INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(owner_id,doc_id,chunk_index));
            CREATE TABLE duplicates(owner_id TEXT NOT NULL,doc_id TEXT NOT NULL,PRIMARY KEY(owner_id,doc_id));")?;
        let mut stream = self
            .table
            .query()
            .select(Select::columns(&[
                "owner_id",
                "doc_id",
                "chunk_index",
                "indexed_at",
                "title",
                "filename",
                "language",
                "year",
                "headings_text",
                "full_text",
                "chunk_start_char",
                "chunk_end_char",
            ]))
            .with_row_id()
            .execute()
            .await?;
        while let Some(batch) = stream.try_next().await? {
            let transaction = spool.transaction()?;
            for chunk in stored_chunks(&batch)? {
                let row_order = i64::try_from(chunk.row_order)
                    .context("Stored row identifier exceeds supported range")?;
                transaction.execute("INSERT OR IGNORE INTO duplicates SELECT ?1,?2 WHERE EXISTS (SELECT 1 FROM chunks WHERE owner_id=?1 AND doc_id=?2 AND chunk_index=?3)",rusqlite::params![chunk.key.owner_id,chunk.key.doc_id,chunk.chunk_index])?;
                transaction.execute("INSERT INTO chunks VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(owner_id,doc_id,chunk_index) DO UPDATE SET indexed_at=excluded.indexed_at,row_order=excluded.row_order,payload=excluded.payload WHERE (excluded.indexed_at,excluded.row_order)>=(chunks.indexed_at,chunks.row_order)",
                    rusqlite::params![chunk.key.owner_id,chunk.key.doc_id,chunk.chunk_index,chunk.indexed_at,row_order,serde_json::to_string(&chunk)?])?;
            }
            transaction.commit()?;
        }
        let mut builder = fts.begin_rebuild()?;
        {
            let mut docs = spool
                .prepare("SELECT DISTINCT owner_id,doc_id FROM chunks ORDER BY owner_id,doc_id")?;
            let keys = docs.query_map([], |row| {
                Ok(DocumentKey {
                    owner_id: row.get(0)?,
                    doc_id: row.get(1)?,
                })
            })?;
            for key in keys {
                let key = key?;
                let mut select=spool.prepare("SELECT payload FROM chunks WHERE owner_id=?1 AND doc_id=?2 ORDER BY chunk_index LIMIT 65537")?;
                let serialized = select
                    .query_map(rusqlite::params![key.owner_id, key.doc_id], |row| {
                        row.get::<_, String>(0)
                    })?;
                let mut chunks = Vec::new();
                let mut bytes = 0usize;
                for payload in serialized {
                    let chunk: StoredChunk = serde_json::from_str(&payload?)?;
                    bytes = bytes.saturating_add(chunk.text.len());
                    ensure!(
                        bytes <= 64 * 1024 * 1024,
                        "Document exceeds the 64 MiB reconstruction limit"
                    );
                    chunks.push(chunk);
                }
                ensure!(
                    chunks.len() <= MAX_AFFECTED_CHUNKS,
                    "Document exceeds the 65536-chunk rebuild limit"
                );
                if let Some(document) = FtsDocument::from_chunks(chunks)? {
                    builder.add(&document)?;
                }
            }
        }
        // Finish and verify the new FTS files before touching legacy vector
        // duplicates. Lance retains its old data versions for crash recovery.
        let maximum: i64 = spool.query_row(
            "SELECT COALESCE(MAX(indexed_at),0) FROM chunks",
            [],
            |row| row.get(0),
        )?;
        fts.observe_timestamp(maximum)?;
        let sealed = fts.seal_rebuild(builder)?;
        loop {
            use rusqlite::OptionalExtension;
            let key = spool
                .query_row(
                    "SELECT owner_id,doc_id FROM duplicates ORDER BY owner_id,doc_id LIMIT 1",
                    [],
                    |row| {
                        Ok(DocumentKey {
                            owner_id: row.get(0)?,
                            doc_id: row.get(1)?,
                        })
                    },
                )
                .optional()?;
            let Some(key) = key else { break };
            let existing = self.fetch_by_keys(std::slice::from_ref(&key)).await?;
            let generation = uuid::Uuid::new_v4().to_string();
            let batches = latest_batches(&existing)?
                .into_iter()
                .map(|batch| with_generation(batch, &generation))
                .collect::<Result<Vec<_>>>()?;
            let write = PreparedWrite {
                documents: documents_from_batches(&batches, std::slice::from_ref(&key))?,
                keys: vec![key.clone()],
                batches,
            };
            fts.mark_pending(&write.keys)?;
            self.commit_upsert(&write).await?;
            spool.execute(
                "DELETE FROM duplicates WHERE owner_id=?1 AND doc_id=?2",
                rusqlite::params![key.owner_id, key.doc_id],
            )?;
        }
        fts.publish_generation(sealed)?;
        drop(spool);
        std::fs::remove_file(spool_path)?;
        Ok(())
    }

    /// Read one ordered metadata page from a pinned immutable snapshot.
    pub async fn rows_since(
        &self,
        fts: &FtsStore,
        since_ms: i64,
        cursor: Option<&crisp_index_protocol::SyncCursor>,
        limit: usize,
    ) -> Result<crisp_index_protocol::SyncPage> {
        self.read_sync_page(fts, since_ms, cursor, limit).await
    }

    async fn delete_keys(&self, keys: &[DocumentKey]) -> Result<()> {
        self.table
            .delete(&keys_sql(keys))
            .await
            .context("Delete scoped document")?;
        Ok(())
    }

    async fn update_keys_location(
        &self,
        keys: &[DocumentKey],
        new_uri: &str,
        indexed_at: i64,
    ) -> Result<()> {
        self.table
            .update()
            .only_if(keys_sql(keys))
            .column("location_uri", format!("'{}'", sql_escape(new_uri)))
            .column(
                "indexed_at",
                format!("timestamp '{}'", iso_from_ms(indexed_at)),
            )
            .execute()
            .await
            .context("Update scoped document location")?;
        Ok(())
    }

    /// Total row count.
    pub async fn count(&self) -> Result<usize> {
        Ok(self.table.count_rows(None).await?)
    }

    /// Approximate document count (distinct doc_ids — cheaper than COUNT DISTINCT).
    /// We count rows where chunk_index = 0.
    pub async fn doc_count(&self) -> Result<usize> {
        Ok(self
            .table
            .count_rows(Some("chunk_index = 0".to_owned()))
            .await?)
    }

    /// Build IVF-PQ vector index.  Call after bulk ingest (≥10 000 rows).
    pub async fn build_ivf_pq(&self) -> Result<()> {
        self.table
            .create_index(
                &[EMB_FIELD],
                Index::IvfPq(
                    IvfPqIndexBuilder::default()
                        .distance_type(DistanceType::Cosine)
                        .num_partitions(256)
                        .num_sub_vectors((self.dims / 8) as u32),
                ),
            )
            .execute()
            .await
            .context("build IVF-PQ index")?;
        Ok(())
    }
}

// ── SearchIndex — combined store ──────────────────────────────────────────────

/// One serialized publication boundary protects Lance rows, the FTS generation,
/// and result hydration. A durable repair record always precedes a vector write.
pub struct SearchIndex {
    pub vector: Arc<VectorStore>,
    pub fts: Arc<FtsStore>,
    gate: tokio::sync::Mutex<()>,
}

impl SearchIndex {
    pub fn new(vector: Arc<VectorStore>, fts: Arc<FtsStore>) -> Self {
        Self {
            vector,
            fts,
            gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Must finish before queue workers or HTTP listeners publish this server.
    pub async fn ensure_ready(&self) -> Result<()> {
        let _guard = self.gate.lock().await;
        self.repair_locked().await
    }

    pub(crate) async fn read_guard(&self) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let guard = self.gate.lock().await;
        self.repair_locked().await?;
        Ok(guard)
    }

    async fn repair_locked(&self) -> Result<()> {
        if self.fts.needs_rebuild()? {
            self.vector.rebuild_fts(&self.fts).await?;
        }
        loop {
            let pending = self.fts.pending()?;
            if pending.is_empty() {
                return Ok(());
            }
            let batches = self.vector.fetch_by_keys(&pending).await?;
            self.fts
                .finish_repairs(&documents_from_batches(&batches, &pending)?)?;
        }
    }

    pub async fn capabilities(&self) -> Result<crisp_index_protocol::SearchCapabilities> {
        let _guard = self.gate.lock().await;
        self.repair_locked().await?;
        self.fts.capabilities()
    }

    /// Validate all syntax and actual dictionary expansions before model work.
    pub async fn validate_query(&self, query: &str, filters: &SearchFilters) -> Result<()> {
        let _guard = self.gate.lock().await;
        self.repair_locked().await?;
        self.fts.prepare(query, filters)?;
        Ok(())
    }

    pub async fn ingest_batch(&self, batch: &IngestBatch) -> Result<()> {
        if batch.chunks.is_empty() {
            return Ok(());
        }
        let _guard = self.gate.lock().await;
        self.repair_locked().await?;
        let (chunks, removals) = self.vector.resolve_source_identities(&batch.chunks).await?;
        let mut write = self
            .vector
            .prepare_upsert_scoped(&chunks, &removals)
            .await?;
        let indexed_at = self.fts.next_write_timestamp()?;
        for batch in &mut write.batches {
            let position = batch.schema().index_of("indexed_at")?;
            let mut columns = batch.columns().to_vec();
            columns[position] = Arc::new(TimestampMillisecondArray::from(vec![
                indexed_at;
                batch.num_rows()
            ]));
            *batch = RecordBatch::try_new(batch.schema(), columns)?;
        }
        self.fts.mark_pending(&write.keys)?;
        let retired: Vec<_> = write
            .documents
            .iter()
            .filter(|(_, document)| document.is_none())
            .map(|(key, _)| key.clone())
            .collect();
        if !retired.is_empty() {
            self.fts.mark_pending_delete(&retired, indexed_at)?;
        }
        self.vector.commit_upsert(&write).await?;
        self.fts.finish_repairs(&write.documents)?;
        Ok(())
    }

    async fn mutation_keys(
        &self,
        doc_id: &str,
        owner_id: Option<&str>,
    ) -> Result<Vec<DocumentKey>> {
        let mut keys = self.vector.keys_for_doc(doc_id).await?;
        if let Some(owner) = owner_id {
            keys.retain(|key| key.owner_id == owner);
        } else if keys.len() > 1 {
            return Err(AmbiguousDocument.into());
        }
        if !keys.is_empty() {
            let batches = self.vector.fetch_by_keys(&keys).await?;
            let mut locations = HashSet::new();
            for batch in &batches {
                let column = string_column(batch, "location_uri")?;
                for row in 0..batch.num_rows() {
                    locations.insert(column.value(row).to_owned());
                }
            }
            if locations.len() > 1 {
                return Err(AmbiguousDocument.into());
            }
        }
        Ok(keys)
    }

    pub async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        self.delete_scoped_doc(doc_id, None).await
    }

    pub async fn delete_scoped_doc(&self, doc_id: &str, owner_id: Option<&str>) -> Result<()> {
        let _guard = self.read_guard().await?;
        let keys = self.mutation_keys(doc_id, owner_id).await?;
        if keys.is_empty() {
            return Ok(());
        }
        self.fts
            .mark_pending_delete(&keys, self.fts.next_write_timestamp()?)?;
        self.vector.delete_keys(&keys).await?;
        self.fts
            .finish_repairs(&keys.into_iter().map(|key| (key, None)).collect::<Vec<_>>())?;
        Ok(())
    }

    pub async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        self.update_scoped_location(doc_id, new_uri, None).await
    }

    pub async fn update_scoped_location(
        &self,
        doc_id: &str,
        new_uri: &str,
        owner_id: Option<&str>,
    ) -> Result<()> {
        let _guard = self.read_guard().await?;
        let keys = self.mutation_keys(doc_id, owner_id).await?;
        if keys.is_empty() {
            return Ok(());
        }
        self.vector
            .update_keys_location(&keys, new_uri, self.fts.next_write_timestamp()?)
            .await
    }

    /// Resolve an exact old URI when a filesystem move has no document ID.
    /// Ambiguous sources and destinations require an explicitly scoped action.
    pub async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()> {
        ensure!(
            !old_uri.trim().is_empty() && !new_uri.trim().is_empty(),
            "Source and destination locations are required"
        );
        let _guard = self.read_guard().await?;
        let keys = self.vector.keys_for_location(old_uri).await?;
        if keys.len() > 1 {
            return Err(AmbiguousDocument.into());
        }
        let Some(key) = keys.first() else {
            return Ok(());
        };
        // A legacy key can cover several locations even when this URI itself
        // selects only one key. Do not move its unselected source copies.
        self.mutation_keys(&key.doc_id, Some(&key.owner_id)).await?;
        if self
            .vector
            .keys_for_location(new_uri)
            .await?
            .iter()
            .any(|other| other != key)
        {
            return Err(AmbiguousDocument.into());
        }
        if old_uri == new_uri {
            return Ok(());
        }
        self.vector
            .update_keys_location(&keys, new_uri, self.fts.next_write_timestamp()?)
            .await
    }

    pub async fn stats(&self) -> Result<(usize, usize)> {
        let _guard = self.read_guard().await?;
        Ok((
            self.vector.count().await?,
            usize::try_from(self.fts.document_count()?)?,
        ))
    }

    /// Metadata predicates apply before result limits, including matches beyond
    /// the first lexical page. The pinned reader is shared across every page.
    async fn filtered_fts_hits(&self, prepared: &PreparedSearch, filters: &SearchFilters, wanted: usize) -> Result<Vec<FtsHit>> {
        if !filters.needs_metadata_filter_version() { return prepared.hits(wanted); }
        let mut hits = Vec::with_capacity(wanted);
        let mut offset = 0usize;
        let total = prepared.document_count();
        while offset < total && hits.len() < wanted {
            let page_size = offset.max(wanted.clamp(128, 512)).min(4096).min(total - offset);
            let page = prepared.hits_page(page_size, offset)?;
            let exhausted = page.len() < page_size;
            offset += page.len();
            for group in page.chunks(512) {
                let keys: Vec<_> = group.iter().map(|hit| hit.key.clone()).collect();
                let allowed = self.vector.matching_keys_for_filters(&keys, filters).await?;
                for hit in group {
                    if allowed.contains(&hit.key) {
                        hits.push(hit.clone());
                        if hits.len() == wanted { return Ok(hits); }
                    }
                }
            }
            if exhausted { break; }
        }
        Ok(hits)
    }

    pub async fn search_text(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        ensure!(limit <= 200, "Result limit exceeds 200");
        filters.validate().map_err(|error| anyhow!(error))?;
        let _guard = self.gate.lock().await;
        self.repair_locked().await?;
        let prepared = self.fts.prepare(query, filters)?;
        let hits = self.filtered_fts_hits(&prepared, filters, limit).await?;
        let scores = hits
            .iter()
            .map(|hit| (hit.key.clone(), hit.score))
            .collect();
        self.hydrate(&scores, &hits, limit).await
    }

    pub async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        ensure!(limit <= 200, "Result limit exceeds 200");
        filters.validate().map_err(|error| anyhow!(error))?;
        validate_embedding(embedding, self.vector.dims)?;
        let _guard = self.gate.lock().await;
        self.repair_locked().await?;
        let matches = self
            .vector_candidates(embedding, filters, limit, None)
            .await?;
        let scores = matches
            .iter()
            .map(|hit| (hit_key(hit), hit.score))
            .collect();
        let mut hydrated = self.hydrate(&scores, &[], limit).await?;
        let best: HashMap<_, _> = matches
            .into_iter()
            .map(|hit| (hit_key(&hit), hit))
            .collect();
        for hit in &mut hydrated {
            if let Some(best) = best.get(&hit_key(hit)) {
                hit.snippet = best.snippet.clone();
                hit.chunk_index = best.chunk_index;
            }
        }
        Ok(hydrated)
    }

    pub async fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        ensure!(limit <= 200, "Result limit exceeds 200");
        filters.validate().map_err(|error| anyhow!(error))?;
        let constrained = crisp_search_query::has_explicit_syntax(query)?;
        validate_embedding(embedding, self.vector.dims)?;
        let _guard = self.gate.lock().await;
        self.repair_locked().await?;
        let prepared = self.fts.prepare(query, filters)?;
        let fts_hits = self.filtered_fts_hits(&prepared, filters, (limit * 3).min(1000)).await?;
        let vec_hits = self
            .vector_candidates(
                embedding,
                filters,
                (limit * 3).min(600),
                constrained.then_some(&prepared),
            )
            .await?;
        let scores = rrf_merge(&fts_hits, &vec_hits, 60, limit);
        self.hydrate(&scores, &fts_hits, limit).await
    }

    async fn vector_candidates(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        wanted: usize,
        constraint: Option<&PreparedSearch>,
    ) -> Result<Vec<SearchHit>> {
        let metadata = self.fts.prepare_filters(filters)?;
        let constraint = constraint.unwrap_or(&metadata);
        let mut candidate_limit = (wanted * 4).clamp(40, 1000);
        loop {
            let hits = self
                .vector
                .search_vector(embedding, filters, candidate_limit)
                .await?;
            let exhausted = hits.len() < candidate_limit;
            let mut seen = HashSet::new();
            let mut unique: Vec<_> = hits
                .into_iter()
                .filter(|hit| seen.insert(hit_key(hit)))
                .collect();
            {
                let keys: Vec<_> = unique.iter().map(hit_key).collect();
                let mut allowed = HashSet::new();
                for group in keys.chunks(1000) {
                    allowed.extend(constraint.matching_keys(group)?);
                }
                unique.retain(|hit| allowed.contains(&hit_key(hit)));
            }
            if unique.len() >= wanted || exhausted || candidate_limit >= 4096 {
                unique.truncate(wanted);
                return Ok(unique);
            }
            candidate_limit = (candidate_limit * 2).min(4096);
        }
    }

    async fn hydrate(
        &self,
        scores: &HashMap<DocumentKey, f32>,
        fts_hits: &[FtsHit],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if scores.is_empty() {
            return Ok(Vec::new());
        }
        let mut keys: Vec<_> = scores.keys().cloned().collect();
        keys.sort();
        let batches = self.vector.fetch_by_keys(&keys).await?;
        let mut candidates = batches_to_results(&latest_batches(&batches)?, Some(scores), false)?;
        candidates.sort_by(|a, b| {
            hit_key(a)
                .cmp(&hit_key(b))
                .then(a.chunk_index.cmp(&b.chunk_index))
        });
        let snippets: HashMap<_, _> = fts_hits
            .iter()
            .map(|hit| (&hit.key, &hit.snippet))
            .collect();
        let mut seen = HashSet::new();
        let mut results: Vec<_> = candidates
            .into_iter()
            .filter(|hit| seen.insert(hit_key(hit)))
            .collect();
        for hit in &mut results {
            if let Some(snippet) = snippets
                .get(&hit_key(hit))
                .filter(|snippet| !snippet.is_empty())
            {
                hit.snippet = (*snippet).clone();
            }
        }
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| hit_key(a).cmp(&hit_key(b)))
        });
        results.truncate(limit);
        Ok(results)
    }
}

fn hit_key(hit: &SearchHit) -> DocumentKey {
    DocumentKey {
        owner_id: hit.owner_id.clone(),
        doc_id: hit.doc_id.clone(),
    }
}

fn rrf_merge(
    fts_hits: &[FtsHit],
    vec_hits: &[SearchHit],
    k: usize,
    limit: usize,
) -> HashMap<DocumentKey, f32> {
    let mut scores = HashMap::<DocumentKey, f32>::new();
    let mut seen = HashSet::new();
    for hit in fts_hits {
        if seen.insert(hit.key.clone()) {
            *scores.entry(hit.key.clone()).or_default() += 1.0 / (k + seen.len()) as f32;
        }
    }
    seen.clear();
    for hit in vec_hits {
        let key = hit_key(hit);
        if seen.insert(key.clone()) {
            *scores.entry(key).or_default() += 1.0 / (k + seen.len()) as f32;
        }
    }
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(limit);
    ranked.into_iter().collect()
}

fn same_document_metadata(left: &IngestChunk, right: &IngestChunk) -> bool {
    left.title == right.title
        && left.author == right.author
        && left.year == right.year
        && left.filename == right.filename
        && left.ext == right.ext
        && left.language == right.language
        && left.location_uri == right.location_uri
        && left.source_hash == right.source_hash
        && left.tags == right.tags
}

/// Partial chunk updates still publish one coherent document metadata view.
fn propagate_document_metadata(
    batch: &RecordBatch,
    groups: &BTreeMap<DocumentKey, Vec<&IngestChunk>>,
) -> Result<RecordBatch> {
    let owners = string_column(batch, "owner_id")?;
    let ids = string_column(batch, "doc_id")?;
    let metadata: Vec<_> = (0..batch.num_rows())
        .map(|row| {
            groups
                .get(&DocumentKey {
                    owner_id: owners.value(row).into(),
                    doc_id: ids.value(row).into(),
                })
                .and_then(|chunks| chunks.first().copied())
        })
        .collect();
    let mut columns = batch.columns().to_vec();
    for name in [
        "title",
        "author",
        "filename",
        "ext",
        "language",
        "location_uri",
        "source_hash",
    ] {
        let old = string_column(batch, name)?;
        let values: Vec<Option<&str>> = metadata
            .iter()
            .enumerate()
            .map(|(row, chunk)| match chunk {
                Some(chunk) => match name {
                    "title" => chunk.title.as_deref(),
                    "author" => chunk.author.as_deref(),
                    "filename" => Some(chunk.filename.as_str()),
                    "ext" => Some(chunk.ext.as_str()),
                    "language" => Some(chunk.language.as_str()),
                    "location_uri" => Some(chunk.location_uri.as_str()),
                    _ => Some(chunk.source_hash.as_str()),
                },
                None => {
                    if old.is_null(row) {
                        None
                    } else {
                        Some(old.value(row))
                    }
                }
            })
            .collect();
        columns[batch.schema().index_of(name)?] = Arc::new(StringArray::from(values));
    }
    let year = batch
        .column_by_name("year")
        .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
        .ok_or_else(|| anyhow!("Stored year column is missing"))?;
    columns[batch.schema().index_of("year")?] = Arc::new(Int32Array::from(
        metadata
            .iter()
            .enumerate()
            .map(|(row, chunk)| match chunk {
                Some(chunk) => chunk.year,
                None => {
                    if year.is_null(row) {
                        None
                    } else {
                        Some(year.value(row))
                    }
                }
            })
            .collect::<Vec<_>>(),
    ));
    let old_tags = batch
        .column_by_name("tags")
        .and_then(|column| column.as_any().downcast_ref::<arrow_array::ListArray>())
        .ok_or_else(|| anyhow!("Stored tags column is missing"))?;
    let mut tags = ListBuilder::new(StringBuilder::new());
    for (row, chunk) in metadata.into_iter().enumerate() {
        if let Some(chunk) = chunk {
            for tag in &chunk.tags {
                tags.values().append_value(tag);
            }
            tags.append(true);
        } else if old_tags.is_null(row) {
            tags.append(false);
        } else {
            let value = old_tags.value(row);
            let values = value
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| anyhow!("Stored tags are not strings"))?;
            for index in 0..values.len() {
                if values.is_null(index) {
                    tags.values().append_null();
                } else {
                    tags.values().append_value(values.value(index));
                }
            }
            tags.append(true);
        }
    }
    columns[batch.schema().index_of("tags")?] = Arc::new(tags.finish());
    Ok(RecordBatch::try_new(batch.schema(), columns)?)
}

fn sql_escape(text: &str) -> String {
    text.replace('\'', "''")
}

fn keys_sql(keys: &[DocumentKey]) -> String {
    if keys.is_empty() {
        return "false".into();
    }
    keys.iter()
        .map(|key| {
            format!(
                "(owner_id='{}' AND doc_id='{}')",
                sql_escape(&key.owner_id),
                sql_escape(&key.doc_id)
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub fn validate_embedding(embedding: &[f32], dims: usize) -> Result<()> {
    ensure!(
        embedding.len() == dims && dims > 0,
        "Embedding dimension {} does not match server dimension {dims}",
        embedding.len()
    );
    ensure!(
        embedding.iter().all(|value| value.is_finite()),
        "Embedding contains non-finite values"
    );
    ensure!(
        embedding.iter().any(|value| *value != 0.0),
        "Embedding must contain a non-zero vector for cosine search"
    );
    Ok(())
}

pub fn validate_chunk_shape(chunk: &IngestChunk) -> Result<()> {
    ensure!(
        !chunk.doc_id.is_empty() && chunk.doc_id.len() <= 512,
        "Document identifier must contain 1 to 512 bytes"
    );
    ensure!(
        chunk.owner_id.len() <= 512,
        "Owner identifier exceeds 512 bytes"
    );
    ensure!(chunk.chunk_index >= 0, "Chunk index cannot be negative");
    ensure!(
        chunk
            .source_identity_version
            .is_none_or(|version| version == 1),
        "Unsupported source identity version"
    );
    if let Some(total) = chunk.chunk_total {
        ensure!(
            total > 0 && total <= MAX_AFFECTED_CHUNKS as i32 && chunk.chunk_index < total,
            "Chunk index or total is outside the supported range"
        );
    }
    ensure!(
        chunk.full_text.len() <= 64 * 1024 * 1024,
        "Chunk text exceeds 64 MiB"
    );
    match (chunk.chunk_start_char, chunk.chunk_end_char) {
        (Some(start), Some(end)) => ensure!(
            start >= 0 && end >= start && (end - start) as usize == chunk.full_text.len(),
            "Chunk byte offsets disagree with text length"
        ),
        (None, None) => {}
        _ => {
            return Err(anyhow!(
                "Chunk start and end offsets must both be present or both absent"
            ))
        }
    }
    Ok(())
}

pub fn validate_chunk(chunk: &IngestChunk, dims: usize) -> Result<()> {
    validate_chunk_shape(chunk)?;
    validate_embedding(&chunk.embedding, dims)
}

/// One canonical row per owner/document/chunk, including deterministic ordering
/// when old append-only writes share a millisecond timestamp.
fn latest_batches(batches: &[RecordBatch]) -> Result<Vec<RecordBatch>> {
    let mut latest = HashMap::<(DocumentKey, i32), (usize, usize, i64, u64)>::new();
    for (batch_index, batch) in batches.iter().enumerate() {
        for (row, chunk) in stored_chunks(batch)?.into_iter().enumerate() {
            let identity = (chunk.key, chunk.chunk_index);
            if latest.get(&identity).is_none_or(|(_, _, time, order)| {
                (*time, *order) <= (chunk.indexed_at, chunk.row_order)
            }) {
                latest.insert(
                    identity,
                    (batch_index, row, chunk.indexed_at, chunk.row_order),
                );
            }
        }
    }
    let mut selected = Vec::new();
    for (batch_index, batch) in batches.iter().enumerate() {
        let mut rows: Vec<_> = latest
            .values()
            .filter(|(index, _, _, _)| *index == batch_index)
            .map(|(_, row, _, _)| *row as u32)
            .collect();
        rows.sort_unstable();
        if rows.is_empty() {
            continue;
        }
        let selection = UInt32Array::from(rows);
        let columns = batch
            .columns()
            .iter()
            .map(|column| arrow_select::take::take(column, &selection, None))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        selected.push(RecordBatch::try_new(batch.schema(), columns)?);
    }
    Ok(selected)
}

fn with_generation(mut batch: RecordBatch, generation: &str) -> Result<RecordBatch> {
    if let Ok(position) = batch.schema().index_of("_rowid") {
        batch.remove_column(position);
    }
    let position = batch.schema().index_of("metadata_json")?;
    let existing = string_column(&batch, "metadata_json")?;
    let mut encoded = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let mut metadata = if existing.is_null(row) {
            serde_json::json!({})
        } else {
            serde_json::from_str(existing.value(row))
                .context("Stored metadata is not valid JSON")?
        };
        ensure!(
            metadata.is_object(),
            "Stored metadata must be a JSON object"
        );
        metadata["server_write_generation"] = generation.into();
        encoded.push(metadata.to_string());
    }
    let mut columns = batch.columns().to_vec();
    columns[position] = Arc::new(StringArray::from(encoded));
    Ok(RecordBatch::try_new(batch.schema(), columns)?)
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| anyhow!("Stored index is missing string column {name}"))
}

fn stored_chunks(batch: &RecordBatch) -> Result<Vec<StoredChunk>> {
    let owners = string_column(batch, "owner_id")?;
    let ids = string_column(batch, "doc_id")?;
    let indexes = batch
        .column_by_name("chunk_index")
        .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
        .ok_or_else(|| anyhow!("Stored index has no chunk_index"))?;
    let time = batch
        .column_by_name("indexed_at")
        .and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>());
    let string = |name: &str, row: usize| -> String {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .filter(|column| !column.is_null(row))
            .map(|column| column.value(row).to_owned())
            .unwrap_or_default()
    };
    let integer = |name: &str, row: usize| -> Option<i32> {
        batch
            .column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .filter(|column| !column.is_null(row))
            .map(|column| column.value(row))
    };
    (0..batch.num_rows())
        .map(|row| {
            ensure!(
                !owners.is_null(row) && !ids.is_null(row) && !indexes.is_null(row),
                "Stored document identity is null"
            );
            let title = string("title", row);
            Ok(StoredChunk {
                key: DocumentKey {
                    owner_id: owners.value(row).into(),
                    doc_id: ids.value(row).into(),
                },
                chunk_index: indexes.value(row),
                indexed_at: time
                    .filter(|column| !column.is_null(row))
                    .map(|column| column.value(row))
                    .unwrap_or(0),
                row_order: batch
                    .column_by_name("_rowid")
                    .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
                    .filter(|column| !column.is_null(row))
                    .map(|column| column.value(row))
                    .unwrap_or(0),
                title: if title.is_empty() {
                    string("filename", row)
                } else {
                    title
                },
                language: string("language", row),
                year: integer("year", row),
                headings: string("headings_text", row),
                text: string("full_text", row),
                start: integer("chunk_start_char", row),
                end: integer("chunk_end_char", row),
            })
        })
        .collect()
}

fn documents_from_batches(
    batches: &[RecordBatch],
    keys: &[DocumentKey],
) -> Result<Vec<(DocumentKey, Option<FtsDocument>)>> {
    let mut grouped: HashMap<DocumentKey, Vec<StoredChunk>> = HashMap::new();
    for batch in batches {
        for chunk in stored_chunks(batch)? {
            grouped.entry(chunk.key.clone()).or_default().push(chunk);
        }
    }
    keys.iter()
        .map(|key| {
            Ok((
                key.clone(),
                FtsDocument::from_chunks(grouped.remove(key).unwrap_or_default())?,
            ))
        })
        .collect()
}

// ── Arrow helpers ─────────────────────────────────────────────────────────────

fn chunk_to_record_batch(
    chunk: &IngestChunk,
    dims: usize,
    schema: &Arc<Schema>,
) -> Result<RecordBatch> {
    use arrow_array::builder::Float32Builder;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    // Build FixedSizeList for embedding.
    let mut flat = Float32Builder::new();
    let emb = &chunk.embedding;
    if emb.len() != dims {
        return Err(anyhow!(
            "embedding length {} != expected dims {}",
            emb.len(),
            dims
        ));
    }
    for &v in emb {
        flat.append_value(v);
    }
    let values_arr = Arc::new(flat.finish());
    let emb_arr = FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        dims as i32,
        values_arr,
        None,
    );

    // Tags list column. Protocol exposes `tags` as `Vec<String>` (always
    // present, possibly empty) — no Option unwrap needed.
    let mut tags_builder = ListBuilder::new(StringBuilder::new());
    for tag in chunk.tags.iter() {
        tags_builder.values().append_value(tag);
    }
    tags_builder.append(true);

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec![Some(format!(
                "{}:{}",
                chunk.doc_id, chunk.chunk_index
            ))])),
            Arc::new(StringArray::from(vec![Some(chunk.doc_id.as_str())])),
            Arc::new(StringArray::from(vec![Some(chunk.location_uri.as_str())])),
            Arc::new(StringArray::from(vec![Some(chunk.owner_id.as_str())])),
            Arc::new(StringArray::from(vec![Some(chunk.filename.as_str())])),
            Arc::new(StringArray::from(vec![chunk.title.as_deref()])),
            Arc::new(StringArray::from(vec![chunk.author.as_deref()])),
            Arc::new(Int32Array::from(vec![chunk.year])),
            Arc::new(StringArray::from(vec![Some(chunk.ext.as_str())])),
            Arc::new(StringArray::from(vec![Some(chunk.language.as_str())])),
            Arc::new(Int32Array::from(vec![None::<i32>])), // page_count
            Arc::new(StringArray::from(vec![chunk
                .headings
                .as_ref()
                .map(|h| h.join(" "))
                .as_deref()])),
            Arc::new(StringArray::from(vec![Some(chunk.full_text.as_str())])),
            Arc::new(StringArray::from(vec![chunk.full_text_md.as_deref()])),
            Arc::new(emb_arr),
            Arc::new(StringArray::from(vec![None::<&str>])), // embedding_sparse
            Arc::new(StringArray::from(vec![Some("server-stored")])),
            Arc::new(Int32Array::from(vec![chunk.chunk_index])),
            Arc::new(Int32Array::from(vec![chunk.chunk_total.unwrap_or(0)])),
            Arc::new(Int32Array::from(vec![chunk.chunk_start_char])),
            Arc::new(Int32Array::from(vec![chunk.chunk_end_char])),
            Arc::new(TimestampMillisecondArray::from(vec![now_ms])),
            Arc::new(StringArray::from(vec![Some(chunk.source_hash.as_str())])),
            Arc::new(tags_builder.finish()),
            Arc::new(StringArray::from(vec![None::<&str>])), // metadata_json
        ],
    )?;

    Ok(batch)
}

/// Convert Arrow `RecordBatch`es to `SearchHit`s.
/// If `score_map` is `Some`, use those scores (BM25 or RRF).
/// Otherwise derive similarity from the `_distance` column (1.0 − dist/2).
fn batches_to_results(
    batches: &[RecordBatch],
    score_map: Option<&HashMap<DocumentKey, f32>>,
    from_dist: bool,
) -> Result<Vec<SearchHit>> {
    let mut results = Vec::new();

    for batch in batches {
        let n = batch.num_rows();
        if n == 0 {
            continue;
        }

        macro_rules! str_col {
            ($name:literal) => {{
                batch
                    .column_by_name($name)
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            }};
        }
        macro_rules! i32_col {
            ($name:literal) => {{
                batch
                    .column_by_name($name)
                    .and_then(|c| c.as_any().downcast_ref::<Int32Array>())
            }};
        }
        macro_rules! f32_col {
            ($name:literal) => {{
                batch
                    .column_by_name($name)
                    .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
            }};
        }

        let doc_id_col = str_col!("doc_id");
        let loc_col = str_col!("location_uri");
        let owner_col = str_col!("owner_id");
        let title_col = str_col!("title");
        let author_col = str_col!("author");
        let year_col = i32_col!("year");
        let fname_col = str_col!("filename");
        let ext_col = str_col!("ext");
        let lang_col = str_col!("language");
        let text_col = str_col!("full_text");
        let chunk_col = i32_col!("chunk_index");
        let source_hash_col = str_col!("source_hash");
        let dist_col = f32_col!("_distance");
        let indexed_col = batch
            .column_by_name("indexed_at")
            .and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>());

        for i in 0..n {
            let doc_id = doc_id_col
                .map(|c| c.value(i).to_owned())
                .unwrap_or_default();
            if doc_id.is_empty() {
                continue;
            }

            let score = if from_dist {
                let dist = dist_col.map(|c| c.value(i)).unwrap_or(1.0);
                1.0 - dist / 2.0
            } else {
                score_map
                    .and_then(|m| {
                        m.get(&DocumentKey {
                            doc_id: doc_id.clone(),
                            owner_id: owner_col.map(|c| c.value(i).to_owned()).unwrap_or_default(),
                        })
                    })
                    .copied()
                    .unwrap_or(0.0)
            };

            let snippet = text_col
                .map(|c| {
                    let t = c.value(i);
                    if t.chars().count() > 300 {
                        format!("{}…", t.chars().take(300).collect::<String>())
                    } else {
                        t.to_owned()
                    }
                })
                .unwrap_or_default();

            results.push(SearchHit {
                doc_id,
                location_uri: loc_col.map(|c| c.value(i).to_owned()).unwrap_or_default(),
                owner_id: owner_col.map(|c| c.value(i).to_owned()).unwrap_or_default(),
                title: title_col.and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i).to_owned())
                    }
                }),
                author: author_col.and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i).to_owned())
                    }
                }),
                year: year_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) }),
                filename: fname_col.and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i).to_owned())
                    }
                }),
                // Protocol added `ext` to the wire shape — populate it from
                // the LanceDB `ext` column. Optional; null when missing.
                ext: ext_col.and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i).to_owned())
                    }
                }),
                language: lang_col.and_then(|c| {
                    if c.is_null(i) {
                        None
                    } else {
                        Some(c.value(i).to_owned())
                    }
                }),
                snippet,
                score,
                chunk_index: chunk_col.map(|c| c.value(i)).unwrap_or(0),
                source_hash: source_hash_col
                    .filter(|column| !column.is_null(i))
                    .map(|column| column.value(i).to_owned())
                    .unwrap_or_default(),
                indexed_at: indexed_col
                    .filter(|column| !column.is_null(i))
                    .map(|column| column.value(i))
                    .unwrap_or(0),
            });
        }
    }

    Ok(results)
}

// ── Error helper ──────────────────────────────────────────────────────────────

fn is_table_not_found(e: &lancedb::Error) -> bool {
    matches!(e, lancedb::Error::TableNotFound { .. })
}

/// Convert a unix-millis timestamp to a LanceDB-friendly ISO-8601 string
/// (UTC, "YYYY-MM-DD HH:MM:SS.sss" — DataFusion's preferred format for
/// timestamp literals). Stdlib-only — avoids pulling chrono into the server.
fn iso_from_ms(ms: i64) -> String {
    // Howard Hinnant's days-from-civil algorithm, adapted from
    // https://howardhinnant.github.io/date_algorithms.html. We use it to
    // turn the "days since 1970-01-01" portion into Y-M-D without chrono.
    let total_secs = ms.div_euclid(1000);
    let sub_ms = ms.rem_euclid(1000) as u32;
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);

    // days-from-civil → (y, m, d)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y0 = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y0 + 1 } else { y0 };

    let h = (secs_of_day / 3600) as u32;
    let mi = ((secs_of_day % 3600) / 60) as u32;
    let s = (secs_of_day % 60) as u32;

    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}.{sub_ms:03}")
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn iso_from_ms_unix_epoch() {
        assert_eq!(iso_from_ms(0), "1970-01-01 00:00:00.000");
    }

    #[test]
    fn iso_from_ms_known_date() {
        // 2024-01-01T00:00:00.000Z = 1_704_067_200_000 ms
        assert_eq!(iso_from_ms(1_704_067_200_000), "2024-01-01 00:00:00.000");
    }

    #[test]
    fn iso_from_ms_with_subsecond() {
        // 2024-01-01T12:34:56.789 UTC
        let ms = 1_704_067_200_000_i64 + 12 * 3_600_000 + 34 * 60_000 + 56_000 + 789;
        assert_eq!(iso_from_ms(ms), "2024-01-01 12:34:56.789");
    }
}

#[cfg(test)]
#[path = "index_contract_tests.rs"]
mod contract_tests;

#[path = "sync_cursor.rs"]
pub(crate) mod sync_cursor;

#[cfg(test)]
#[path = "sync_cursor_contract_tests.rs"]
mod sync_cursor_contract_tests;
