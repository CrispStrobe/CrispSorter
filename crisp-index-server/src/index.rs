/// Server-side index: LanceDB vector store + Tantivy FTS.
///
/// The client (CrispSorter) is responsible for chunking and embedding.
/// This server receives pre-computed dense vectors alongside the plain text and
/// stores both.  Full-text search (BM25) runs on the stored text; ANN search
/// uses the stored vectors.  Hybrid search merges both with RRF (k = 60).
///
/// Schema columns mirror the CrispSorter client exactly so that Lance files
/// can be moved between local and remote configurations without conversion.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use arrow_array::{
    Array, FixedSizeListArray, Float32Array, Int32Array, RecordBatch,
    StringArray, TimestampMillisecondArray,
    builder::{ListBuilder, StringBuilder},
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use crisp_index_protocol::{IngestBatch, IngestChunk, SearchFilters, SearchHit};
use futures_util::TryStreamExt;
use lancedb::{
    connect, Connection, Table,
    index::{Index, vector::IvfPqIndexBuilder},
    query::{ExecutableQuery, QueryBase},
    DistanceType,
};
use tantivy::{
    collector::TopDocs,
    doc,
    query::{BooleanQuery, Occur, QueryParser, TermQuery},
    schema::{
        IndexRecordOption, Schema as TantivySchema, SchemaBuilder,
        TextFieldIndexing, TextOptions, STORED, STRING,
    },
    Index as TantivyIndex, IndexWriter, ReloadPolicy, TantivyDocument, Term,
};

// ── Schema ────────────────────────────────────────────────────────────────────

const TABLE_NAME: &str   = "documents";
const EMB_FIELD:  &str   = "embedding";
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
        Field::new("id",              DataType::Utf8, false),
        Field::new("doc_id",          DataType::Utf8, false),
        Field::new("location_uri",    DataType::Utf8, false),
        Field::new("owner_id",        DataType::Utf8, false),
        Field::new("filename",        DataType::Utf8, true),
        Field::new("title",           DataType::Utf8, true),
        Field::new("author",          DataType::Utf8, true),
        Field::new("year",            DataType::Int32, true),
        Field::new("ext",             DataType::Utf8, true),
        Field::new("language",        DataType::Utf8, true),
        Field::new("page_count",      DataType::Int32, true),
        Field::new("headings_text",   DataType::Utf8, true),
        Field::new("full_text",       DataType::Utf8, true),
        Field::new("full_text_md",    DataType::Utf8, true),
        emb_field,
        Field::new("embedding_sparse",DataType::Utf8, true),
        Field::new("embedding_model", DataType::Utf8, true),
        Field::new("chunk_index",     DataType::Int32, false),
        Field::new("chunk_total",     DataType::Int32, false),
        Field::new("chunk_start_char",DataType::Int32, true),
        Field::new("chunk_end_char",  DataType::Int32, true),
        Field::new("indexed_at",      DataType::Timestamp(TimeUnit::Millisecond, None), false),
        Field::new("source_hash",     DataType::Utf8, false),
        Field::new("tags",            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))), true),
        Field::new("metadata_json",   DataType::Utf8, true),
    ]))
}

// Wire-format types (`IngestChunk`, `SearchHit`, `SearchFilters`) live in
// the `crisp-index-protocol` crate and are imported above. Internally we
// produce `SearchHit`s directly — they're a strict subset of CrispSorter's
// richer client-side `SearchResult` (which carries optional metadata fields
// the server doesn't populate yet). See
// `../../crisp-index-protocol/src/lib.rs` for per-field documentation.

// ── FTS hit (internal) ────────────────────────────────────────────────────────

pub struct FtsHit {
    pub doc_id: String,
    pub score:  f32,
}

// ── VectorStore ───────────────────────────────────────────────────────────────

pub struct VectorStore {
    _db:   Connection,
    table: Table,
    pub dims: usize,
}

impl VectorStore {
    pub async fn open_or_create(data_dir: &Path, dims: usize) -> Result<Self> {
        let lance_dir = data_dir.join("lance");
        std::fs::create_dir_all(&lance_dir)?;
        let uri = lance_dir.to_string_lossy().into_owned();
        let db  = connect(&uri).execute().await
            .with_context(|| format!("LanceDB connect {uri}"))?;

        let table = match db.open_table(TABLE_NAME).execute().await {
            Ok(t) => t,
            Err(e) if is_table_not_found(&e) => {
                let schema = build_lance_schema(dims);
                db.create_empty_table(TABLE_NAME, schema).execute().await
                    .context("create LanceDB table")?
            }
            Err(e) => return Err(e).context("open LanceDB table"),
        };

        Ok(VectorStore { _db: db, table, dims })
    }

    /// Store a single pre-embedded chunk.
    pub async fn ingest(&self, chunk: &IngestChunk) -> Result<()> {
        let schema = build_lance_schema(self.dims);
        let batch  = chunk_to_record_batch(chunk, self.dims, &schema)?;
        let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.table.add(reader).execute().await.context("LanceDB add")?;
        Ok(())
    }

    /// Store many pre-embedded chunks in one LanceDB add call.
    pub async fn ingest_batch(&self, chunks: &[IngestChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let schema = build_lance_schema(self.dims);
        let batches: Result<Vec<RecordBatch>> = chunks
            .iter()
            .map(|chunk| chunk_to_record_batch(chunk, self.dims, &schema))
            .collect();
        let reader = arrow_array::RecordBatchIterator::new(
            batches?.into_iter().map(Ok::<RecordBatch, arrow_schema::ArrowError>),
            schema,
        );
        self.table.add(reader).execute().await.context("LanceDB add batch")?;
        Ok(())
    }

    /// ANN vector search.  Converts cosine distance → similarity score.
    pub async fn search_vector(
        &self,
        embedding: &[f32],
        filters:   &SearchFilters,
        limit:     usize,
    ) -> Result<Vec<SearchHit>> {
        let mut q = self.table
            .vector_search(embedding)?
            .distance_type(DistanceType::Cosine)
            .limit(limit);

        if let Some(sql) = filters.to_lance_sql() {
            q = q.only_if(sql);
        }

        let batches: Vec<RecordBatch> = q.execute().await?.try_collect().await?;
        batches_to_results(&batches, None, /* use distance */ true)
    }

    /// Fetch rows by doc_id list (for RRF hydration).
    pub async fn fetch_by_doc_ids(&self, doc_ids: &[String]) -> Result<Vec<RecordBatch>> {
        if doc_ids.is_empty() { return Ok(vec![]); }
        let ids_sql = doc_ids.iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let filter = format!("doc_id IN ({ids_sql})");
        let batches: Vec<RecordBatch> = self.table.query()
            .only_if(filter)
            .limit(doc_ids.len() * 4)
            .execute().await?.try_collect().await?;
        Ok(batches)
    }

    /// Delete all chunks for a document.
    pub async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        self.table.delete(&filter).await.context("LanceDB delete")?;
        Ok(())
    }

    /// Update the location_uri for all chunks of a document.
    pub async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        let filter = format!("doc_id = '{}'", doc_id.replace('\'', "''"));
        let value  = format!("'{}'", new_uri.replace('\'', "''"));
        self.table.update()
            .only_if(filter)
            .column("location_uri", value)
            .execute()
            .await
            .context("LanceDB update_location")?;
        Ok(())
    }

    /// Total row count.
    pub async fn count(&self) -> Result<usize> {
        Ok(self.table.count_rows(None).await?)
    }

    /// Approximate document count (distinct doc_ids — cheaper than COUNT DISTINCT).
    /// We count rows where chunk_index = 0.
    pub async fn doc_count(&self) -> Result<usize> {
        Ok(self.table.count_rows(Some("chunk_index = 0".to_owned())).await?)
    }

    /// Build IVF-PQ vector index.  Call after bulk ingest (≥10 000 rows).
    pub async fn build_ivf_pq(&self) -> Result<()> {
        self.table.create_index(&[EMB_FIELD], Index::IvfPq(
            IvfPqIndexBuilder::default()
                .distance_type(DistanceType::Cosine)
                .num_partitions(256)
                .num_sub_vectors((self.dims / 8) as u32),
        ))
        .execute()
        .await
        .context("build IVF-PQ index")?;
        Ok(())
    }
}

// ── FtsStore ──────────────────────────────────────────────────────────────────

pub struct FtsStore {
    index:  TantivyIndex,
    schema: TantivySchema,
}

impl FtsStore {
    pub fn open_or_create(fts_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(fts_dir)?;

        let mut sb = SchemaBuilder::new();
        let text_opts = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_index_option(IndexRecordOption::WithFreqsAndPositions)
                .set_tokenizer("en_stem"),
        );
        sb.add_text_field("doc_id",   STRING | STORED);
        sb.add_text_field("owner_id", STRING | STORED);
        sb.add_text_field("language", STRING | STORED);
        sb.add_text_field("headings", text_opts.clone());
        sb.add_text_field("body",     text_opts);
        let schema = sb.build();

        let mut index = if fts_dir.join("meta.json").exists() {
            TantivyIndex::open_in_dir(fts_dir)?
        } else {
            TantivyIndex::create_in_dir(fts_dir, schema.clone())?
        };

        index.set_default_multithread_executor()?;
        Ok(FtsStore { index, schema })
    }

    fn writer(&self) -> Result<IndexWriter> {
        Ok(self.index.writer(50_000_000)?)
    }

    /// Add (or update) a document in the Tantivy index.
    pub fn add_document(
        &self,
        doc_id:   &str,
        owner_id: &str,
        language: &str,
        headings: &str,
        body:     &str,
    ) -> Result<()> {
        let mut writer = self.writer()?;
        let doc_id_field   = self.schema.get_field("doc_id").unwrap();
        let owner_id_field = self.schema.get_field("owner_id").unwrap();
        let language_field = self.schema.get_field("language").unwrap();
        let headings_field = self.schema.get_field("headings").unwrap();
        let body_field     = self.schema.get_field("body").unwrap();

        // Delete existing entry for this doc_id (re-ingest is idempotent).
        let term = Term::from_field_text(doc_id_field, doc_id);
        writer.delete_term(term);

        writer.add_document(doc!(
            doc_id_field   => doc_id,
            owner_id_field => owner_id,
            language_field => language,
            headings_field => headings,
            body_field     => body,
        ))?;
        writer.commit()?;
        Ok(())
    }

    /// Add or replace many documents, committing once at the end.
    pub fn add_documents(&self, docs: &[(&str, &str, &str, &str, &str)]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer()?;
        let doc_id_field = self.schema.get_field("doc_id").unwrap();
        let owner_id_field = self.schema.get_field("owner_id").unwrap();
        let language_field = self.schema.get_field("language").unwrap();
        let headings_field = self.schema.get_field("headings").unwrap();
        let body_field = self.schema.get_field("body").unwrap();

        for (doc_id, owner_id, language, headings, body) in docs {
            let term = Term::from_field_text(doc_id_field, doc_id);
            writer.delete_term(term);
            writer.add_document(doc!(
                doc_id_field   => *doc_id,
                owner_id_field => *owner_id,
                language_field => *language,
                headings_field => *headings,
                body_field     => *body,
            ))?;
        }
        writer.commit()?;
        Ok(())
    }

    pub fn delete_document(&self, doc_id: &str) -> Result<()> {
        let mut writer = self.writer()?;
        let doc_id_field = self.schema.get_field("doc_id").unwrap();
        let term = Term::from_field_text(doc_id_field, doc_id);
        writer.delete_term(term);
        writer.commit()?;
        Ok(())
    }

    /// BM25 full-text search.
    pub fn search(
        &self,
        query_text: &str,
        filters:    &SearchFilters,
        limit:      usize,
    ) -> Result<Vec<FtsHit>> {
        let reader = self.index.reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let searcher = reader.searcher();

        let body_field  = self.schema.get_field("body").unwrap();
        let doc_id_field = self.schema.get_field("doc_id").unwrap();
        let owner_id_field = self.schema.get_field("owner_id").unwrap();

        let qp = QueryParser::for_index(&self.index, vec![body_field]);
        let parsed = qp.parse_query(query_text)
            .unwrap_or_else(|_| {
                // Fall back to a term search on the raw text.
                let term = Term::from_field_text(body_field, query_text);
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqsAndPositions))
            });

        // Owner-id pre-filter.
        let combined: Box<dyn tantivy::query::Query> = if let Some(oid) = &filters.owner_id {
            let owner_term = Term::from_field_text(owner_id_field, oid);
            let owner_q    = TermQuery::new(owner_term, IndexRecordOption::Basic);
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, parsed),
                (Occur::Must, Box::new(owner_q)),
            ]))
        } else {
            parsed
        };

        let top = searcher.search(&combined, &TopDocs::with_limit(limit))?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(tantivy::schema::OwnedValue::Str(id)) =
                doc.get_first(doc_id_field)
            {
                hits.push(FtsHit { doc_id: id.clone(), score });
            }
        }
        Ok(hits)
    }
}

// ── SearchIndex — combined store ──────────────────────────────────────────────

/// Combines `VectorStore` + `FtsStore` and implements hybrid RRF search.
pub struct SearchIndex {
    pub vector: Arc<VectorStore>,
    pub fts:    Arc<FtsStore>,
}

impl SearchIndex {
    pub fn new(vector: Arc<VectorStore>, fts: Arc<FtsStore>) -> Self {
        SearchIndex { vector, fts }
    }

    pub async fn ingest_batch(&self, batch: &IngestBatch) -> Result<()> {
        if batch.chunks.is_empty() {
            return Ok(());
        }
        self.vector.ingest_batch(&batch.chunks).await?;

        let first_chunks: Vec<(&str, &str, &str, String, &str)> = batch
            .chunks
            .iter()
            .filter(|c| c.chunk_index == 0)
            .map(|c| {
                (
                    c.doc_id.as_str(),
                    c.owner_id.as_str(),
                    c.language.as_str(),
                    c.headings.as_ref().map(|h| h.join(" ")).unwrap_or_default(),
                    c.full_text.as_str(),
                )
            })
            .collect();

        let fts_docs: Vec<(&str, &str, &str, &str, &str)> = first_chunks
            .iter()
            .map(|(doc_id, owner_id, language, headings, body)| {
                (*doc_id, *owner_id, *language, headings.as_str(), *body)
            })
            .collect();
        self.fts.add_documents(&fts_docs)?;
        Ok(())
    }

    pub async fn search_text(
        &self,
        query:   &str,
        filters: &SearchFilters,
        limit:   usize,
    ) -> Result<Vec<SearchHit>> {
        let hits = self.fts.search(query, filters, limit)?;
        if hits.is_empty() { return Ok(vec![]); }

        let ids: Vec<String>           = hits.iter().map(|h| h.doc_id.clone()).collect();
        let score_map: HashMap<String, f32> = hits.into_iter().map(|h| (h.doc_id, h.score)).collect();

        let batches = self.vector.fetch_by_doc_ids(&ids).await?;
        let mut results = batches_to_results(&batches, Some(&score_map), false)?;
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    pub async fn search_vector(
        &self,
        embedding: &[f32],
        filters:   &SearchFilters,
        limit:     usize,
    ) -> Result<Vec<SearchHit>> {
        self.vector.search_vector(embedding, filters, limit).await
    }

    pub async fn search_hybrid(
        &self,
        query:     &str,
        embedding: &[f32],
        filters:   &SearchFilters,
        limit:     usize,
    ) -> Result<Vec<SearchHit>> {
        let vector_arc = self.vector.clone();
        let fts_arc    = self.fts.clone();
        let emb_owned  = embedding.to_owned();
        let q_owned    = query.to_owned();
        let f_fts      = SearchFilters { owner_id: filters.owner_id.clone(), language: filters.language.clone(), year_min: filters.year_min, year_max: filters.year_max };
        let f_vec      = SearchFilters { owner_id: filters.owner_id.clone(), language: filters.language.clone(), year_min: filters.year_min, year_max: filters.year_max };

        let fts_task = tokio::spawn(async move {
            fts_arc.search(&q_owned, &f_fts, limit * 2)
        });
        let vec_task = tokio::spawn(async move {
            vector_arc.search_vector(&emb_owned, &f_vec, limit * 2).await
        });

        let (fts_res, vec_res) = tokio::try_join!(fts_task, vec_task)?;
        let fts_hits  = fts_res?;
        let vec_hits  = vec_res?;

        let merged = rrf_merge(&fts_hits, &vec_hits, 60, limit);
        if merged.is_empty() { return Ok(vec![]); }

        let ids: Vec<String>      = merged.iter().map(|(id, _)| id.clone()).collect();
        let score_map: HashMap<String, f32> = merged.into_iter().collect();
        let batches = self.vector.fetch_by_doc_ids(&ids).await?;

        let mut results = batches_to_results(&batches, Some(&score_map), false)?;
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }
}

// ── RRF ───────────────────────────────────────────────────────────────────────

fn rrf_merge(
    fts_hits: &[FtsHit],
    vec_hits: &[SearchHit],
    k:        usize,
    limit:    usize,
) -> Vec<(String, f32)> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    for (rank, hit) in fts_hits.iter().enumerate() {
        *scores.entry(hit.doc_id.clone()).or_insert(0.0) += 1.0 / (k + rank + 1) as f32;
    }
    for (rank, hit) in vec_hits.iter().enumerate() {
        *scores.entry(hit.doc_id.clone()).or_insert(0.0) += 1.0 / (k + rank + 1) as f32;
    }
    let mut ranked: Vec<(String, f32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);
    ranked
}

// ── Arrow helpers ─────────────────────────────────────────────────────────────

fn chunk_to_record_batch(
    chunk:  &IngestChunk,
    dims:   usize,
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
        return Err(anyhow!("embedding length {} != expected dims {}", emb.len(), dims));
    }
    for &v in emb { flat.append_value(v); }
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
    for tag in chunk.tags.iter() { tags_builder.values().append_value(tag); }
    tags_builder.append(true);

    let batch = RecordBatch::try_new(schema.clone(), vec![
        Arc::new(StringArray::from(vec![Some(format!("{}:{}", chunk.doc_id, chunk.chunk_index))])),
        Arc::new(StringArray::from(vec![Some(chunk.doc_id.as_str())])),
        Arc::new(StringArray::from(vec![Some(chunk.location_uri.as_str())])),
        Arc::new(StringArray::from(vec![Some(chunk.owner_id.as_str())])),
        Arc::new(StringArray::from(vec![Some(chunk.filename.as_str())])),
        Arc::new(StringArray::from(vec![chunk.title.as_deref()])),
        Arc::new(StringArray::from(vec![chunk.author.as_deref()])),
        Arc::new(Int32Array::from(vec![chunk.year])),
        Arc::new(StringArray::from(vec![Some(chunk.ext.as_str())])),
        Arc::new(StringArray::from(vec![Some(chunk.language.as_str())])),
        Arc::new(Int32Array::from(vec![None::<i32>])),  // page_count
        Arc::new(StringArray::from(vec![chunk.headings.as_ref().map(|h| h.join(" ")).as_deref()])),
        Arc::new(StringArray::from(vec![Some(chunk.full_text.as_str())])),
        Arc::new(StringArray::from(vec![chunk.full_text_md.as_deref()])),
        Arc::new(emb_arr),
        Arc::new(StringArray::from(vec![None::<&str>])),  // embedding_sparse
        Arc::new(StringArray::from(vec![Some("server-stored")])),
        Arc::new(Int32Array::from(vec![chunk.chunk_index])),
        Arc::new(Int32Array::from(vec![0i32])),           // chunk_total unknown at server
        Arc::new(Int32Array::from(vec![None::<i32>])),    // chunk_start_char
        Arc::new(Int32Array::from(vec![None::<i32>])),    // chunk_end_char
        Arc::new(TimestampMillisecondArray::from(vec![now_ms])),
        Arc::new(StringArray::from(vec![Some(chunk.source_hash.as_str())])),
        Arc::new(tags_builder.finish()),
        Arc::new(StringArray::from(vec![None::<&str>])),  // metadata_json
    ])?;

    Ok(batch)
}

/// Convert Arrow `RecordBatch`es to `SearchHit`s.
/// If `score_map` is `Some`, use those scores (BM25 or RRF).
/// Otherwise derive similarity from the `_distance` column (1.0 − dist/2).
fn batches_to_results(
    batches:   &[RecordBatch],
    score_map: Option<&HashMap<String, f32>>,
    from_dist: bool,
) -> Result<Vec<SearchHit>> {
    let mut results = Vec::new();

    for batch in batches {
        let n = batch.num_rows();
        if n == 0 { continue; }

        macro_rules! str_col {
            ($name:literal) => {{
                batch.column_by_name($name)
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            }};
        }
        macro_rules! i32_col {
            ($name:literal) => {{
                batch.column_by_name($name)
                    .and_then(|c| c.as_any().downcast_ref::<Int32Array>())
            }};
        }
        macro_rules! f32_col {
            ($name:literal) => {{
                batch.column_by_name($name)
                    .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
            }};
        }

        let doc_id_col  = str_col!("doc_id");
        let loc_col     = str_col!("location_uri");
        let owner_col   = str_col!("owner_id");
        let title_col   = str_col!("title");
        let author_col  = str_col!("author");
        let year_col    = i32_col!("year");
        let fname_col   = str_col!("filename");
        let ext_col     = str_col!("ext");
        let lang_col    = str_col!("language");
        let text_col    = str_col!("full_text");
        let chunk_col   = i32_col!("chunk_index");
        let dist_col    = f32_col!("_distance");

        for i in 0..n {
            let doc_id = doc_id_col.map(|c| c.value(i).to_owned()).unwrap_or_default();
            if doc_id.is_empty() { continue; }

            let score = if from_dist {
                let dist = dist_col.map(|c| c.value(i)).unwrap_or(1.0);
                1.0 - dist / 2.0
            } else {
                score_map
                    .and_then(|m| m.get(&doc_id))
                    .copied()
                    .unwrap_or(0.0)
            };

            let snippet = text_col.map(|c| {
                let t = c.value(i);
                if t.len() > 300 { format!("{}…", &t[..300]) } else { t.to_owned() }
            }).unwrap_or_default();

            results.push(SearchHit {
                doc_id,
                location_uri: loc_col.map(|c| c.value(i).to_owned()).unwrap_or_default(),
                owner_id:     owner_col.map(|c| c.value(i).to_owned()).unwrap_or_default(),
                title:        title_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i).to_owned()) }),
                author:       author_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i).to_owned()) }),
                year:         year_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) }),
                filename:     fname_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i).to_owned()) }),
                // Protocol added `ext` to the wire shape — populate it from
                // the LanceDB `ext` column. Optional; null when missing.
                ext:          ext_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i).to_owned()) }),
                language:     lang_col.and_then(|c| if c.is_null(i) { None } else { Some(c.value(i).to_owned()) }),
                snippet,
                score,
                chunk_index:  chunk_col.map(|c| c.value(i)).unwrap_or(0),
            });
        }
    }

    Ok(results)
}

// ── Error helper ──────────────────────────────────────────────────────────────

fn is_table_not_found(e: &lancedb::Error) -> bool {
    matches!(e, lancedb::Error::TableNotFound { .. })
}
