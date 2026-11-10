//! Versioned server full-text index, durable repair queue, and shared query semantics.
//!
//! The original `fts/` directory is never modified by migration. A new generation
//! is published only after its complete Tantivy commit succeeds. SQLite keeps the
//! active generation pointer and pending document repairs durable across crashes.
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, bail, ensure, Context, Result};
use crisp_index_protocol::{
    SearchCapabilities, SearchFilters, FTS_SCHEMA_VERSION, QUERY_LANGUAGE_VERSION,
};
use crisp_search_query::{text_analyzer, translate, SearchFields};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, RangeQuery, TermQuery, TermSetQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, INDEXED, STORED, STRING,
};
use tantivy::tokenizer::{PreTokenizedString, Token};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, SnippetGenerator, TantivyDocument,
    Term,
};

const TOKENIZER: &str = "crisp_shared_v1";
// Larger than the grammar's maximum proximity distance. Unknown chunk offsets
// must never create invented adjacency or proximity across separate chunks.
const UNKNOWN_BOUNDARY_GAP: usize = 2048;
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocumentKey {
    pub owner_id: String,
    pub doc_id: String,
}

impl DocumentKey {
    pub fn encoded(&self) -> String {
        // JSON encoding is unambiguous even when identifiers contain delimiters.
        serde_json::to_string(&(self.owner_id.as_str(), self.doc_id.as_str()))
            .expect("serializing two strings cannot fail")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredChunk {
    pub key: DocumentKey,
    pub chunk_index: i32,
    pub indexed_at: i64,
    #[serde(default)]
    pub row_order: u64,
    pub title: String,
    pub language: String,
    pub year: Option<i32>,
    pub headings: String,
    pub text: String,
    pub start: Option<i32>,
    pub end: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct FtsDocument {
    pub key: DocumentKey,
    pub title: String,
    pub language: String,
    pub year: Option<i32>,
    pub headings: String,
    /// Separate entries have unknown source continuity and cannot form phrases.
    pub body_segments: Vec<String>,
}

impl FtsDocument {
    pub fn from_chunks(chunks: Vec<StoredChunk>) -> Result<Option<Self>> {
        let expected_key = chunks.first().map(|chunk| chunk.key.clone());
        let mut latest = BTreeMap::<i32, StoredChunk>::new();
        for chunk in chunks {
            ensure!(
                Some(&chunk.key) == expected_key.as_ref(),
                "Document rebuild mixed different owners or documents"
            );
            ensure!(chunk.chunk_index >= 0, "Stored chunk index is negative");
            match latest.get(&chunk.chunk_index) {
                Some(previous)
                    if (previous.indexed_at, previous.row_order)
                        > (chunk.indexed_at, chunk.row_order) => {}
                _ => {
                    latest.insert(chunk.chunk_index, chunk);
                }
            }
        }
        let chunks: Vec<_> = latest.into_values().collect();
        let Some(first) = chunks.first() else {
            return Ok(None);
        };
        let key = first.key.clone();
        ensure!(
            chunks.iter().all(|c| c.key == key),
            "Document rebuild mixed different owners or documents"
        );
        let total_bytes = chunks
            .iter()
            .try_fold(0usize, |sum, c| sum.checked_add(c.text.len()))
            .ok_or_else(|| anyhow!("Document size overflow"))?;
        ensure!(
            total_bytes <= MAX_DOCUMENT_BYTES,
            "Document exceeds the 64 MiB full-text reconstruction limit"
        );
        let mut result = Self {
            key,
            title: first.title.clone(),
            language: first.language.clone(),
            year: first.year,
            headings: String::new(),
            body_segments: Vec::new(),
        };
        let mut seen_headings = HashSet::new();
        let mut current_end: Option<usize> = None;
        for chunk in chunks {
            if !chunk.headings.is_empty() && seen_headings.insert(chunk.headings.clone()) {
                if !result.headings.is_empty() {
                    result.headings.push('\n');
                }
                result.headings.push_str(&chunk.headings);
            }
            let range = match (chunk.start, chunk.end) {
                (Some(start), Some(end)) if start >= 0 && end >= start => {
                    ensure!(
                        (end - start) as usize == chunk.text.len(),
                        "Chunk byte offsets disagree with its text length"
                    );
                    Some((start as usize, end as usize))
                }
                (None, None) => None,
                _ => bail!("Stored chunk has incomplete or invalid source offsets"),
            };
            if let (Some(previous_end), Some((start, end))) = (current_end, range) {
                if start <= previous_end {
                    let previous = result
                        .body_segments
                        .last_mut()
                        .expect("a known end has a body");
                    let previous_start = previous_end
                        .checked_sub(previous.len())
                        .ok_or_else(|| anyhow!("Invalid reconstructed range"))?;
                    ensure!(start >= previous_start, "Chunk offsets move backwards");
                    let overlap = previous_end.min(end) - start;
                    let prior = previous
                        .get(start - previous_start..start - previous_start + overlap)
                        .ok_or_else(|| anyhow!("Chunk overlap splits a UTF-8 character"))?;
                    let incoming = chunk
                        .text
                        .get(..overlap)
                        .ok_or_else(|| anyhow!("Chunk overlap splits a UTF-8 character"))?;
                    ensure!(
                        prior == incoming,
                        "Overlapping chunks contain inconsistent source text"
                    );
                    if end > previous_end {
                        previous.push_str(&chunk.text[overlap..]);
                        current_end = Some(end);
                    }
                    continue;
                }
            }
            // Missing ranges and actual gaps remain distinct positional regions.
            current_end = range.map(|(_, end)| end);
            result.body_segments.push(chunk.text);
        }
        Ok(Some(result))
    }
}

#[derive(Clone)]
struct Fields {
    key: Field,
    doc_id: Field,
    owner: Field,
    language: Field,
    year: Field,
    search: SearchFields,
}

fn build_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let options = TextOptions::default().set_stored().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let key = builder.add_text_field("document_key", STRING | STORED);
    let doc_id = builder.add_text_field("doc_id", STRING | STORED);
    let owner = builder.add_text_field("owner_id", STRING | STORED);
    let language = builder.add_text_field("language", STRING);
    let year = builder.add_i64_field("year", INDEXED);
    let title = builder.add_text_field("title", options.clone());
    let headings = builder.add_text_field("headings", options.clone());
    let body = builder.add_text_field("body", options);
    (
        builder.build(),
        Fields {
            key,
            doc_id,
            owner,
            language,
            year,
            search: SearchFields {
                title,
                body,
                headings,
                body_translated: None,
            },
        },
    )
}

struct Generation {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

impl Generation {
    fn open(path: &Path) -> Result<Self> {
        let (schema, fields) = build_schema();
        let index = Index::open_in_dir(path).context("Open published full-text generation")?;
        ensure!(index.schema() == schema, "Published full-text schema does not match this server; preserve it and rebuild with a compatible version");
        index.tokenizers().register(TOKENIZER, text_analyzer());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            index,
            reader,
            fields,
        })
    }
}

pub struct GenerationBuilder {
    id: String,
    writer: IndexWriter,
    fields: Fields,
}

impl GenerationBuilder {
    pub fn add(&mut self, document: &FtsDocument) -> Result<()> {
        self.writer
            .add_document(index_document(document, &self.fields)?)?;
        Ok(())
    }
}

pub struct SealedGeneration {
    id: String,
    generation: Arc<Generation>,
}

pub struct FtsStore {
    root: PathBuf,
    active: RwLock<Option<Arc<Generation>>>,
    // A separate SQLite connection holds an OS-backed exclusive lock for this
    // server's lifetime. A crash releases it; two writers cannot share one root.
    _process_lease: Mutex<Connection>,
}

impl FtsStore {
    pub fn open_or_create(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let lease = Connection::open(root.join("server-lock.sqlite"))?;
        lease.busy_timeout(std::time::Duration::ZERO)?;
        lease
            .execute_batch("CREATE TABLE IF NOT EXISTS server_lock (id INTEGER); BEGIN EXCLUSIVE;")
            .context("Another server is already using this full-text directory")?;
        let result = Self {
            root: root.to_owned(),
            active: RwLock::new(None),
            _process_lease: Mutex::new(lease),
        };
        let db = result.state()?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);\
            CREATE TABLE IF NOT EXISTS pending_documents (owner_id TEXT NOT NULL, doc_id TEXT NOT NULL, PRIMARY KEY(owner_id,doc_id));
            CREATE TABLE IF NOT EXISTS pending_deletions (owner_id TEXT NOT NULL, doc_id TEXT NOT NULL, indexed_at INTEGER NOT NULL, PRIMARY KEY(owner_id,doc_id));
            CREATE TABLE IF NOT EXISTS deletion_events (owner_id TEXT NOT NULL, doc_id TEXT NOT NULL, indexed_at INTEGER NOT NULL, PRIMARY KEY(owner_id,doc_id,indexed_at));
            CREATE INDEX IF NOT EXISTS deletion_event_order ON deletion_events(indexed_at,owner_id,doc_id);")?;
        let active: Option<String> = db
            .query_row(
                "SELECT value FROM metadata WHERE key='active_generation'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = active {
            ensure!(
                id.starts_with("generation-")
                    && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "Invalid stored full-text generation identifier"
            );
            *result
                .active
                .write()
                .map_err(|_| anyhow!("Full-text generation lock poisoned"))? =
                Some(Arc::new(Generation::open(&root.join(id))?));
        }
        Ok(result)
    }

    fn state(&self) -> Result<Connection> {
        let db = Connection::open(self.root.join("state.sqlite"))?;
        db.busy_timeout(std::time::Duration::from_secs(15))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        Ok(db)
    }

    pub fn needs_rebuild(&self) -> Result<bool> {
        Ok(self
            .active
            .read()
            .map_err(|_| anyhow!("Full-text generation lock poisoned"))?
            .is_none())
    }

    pub fn rebuild_spool_path(&self) -> PathBuf {
        self.root
            .join(format!("rebuild-{}.sqlite", uuid::Uuid::new_v4()))
    }

    pub fn document_count(&self) -> Result<u64> {
        Ok(self.generation()?.reader.searcher().num_docs())
    }

    pub fn begin_rebuild(&self) -> Result<GenerationBuilder> {
        let id = format!("generation-{}", uuid::Uuid::new_v4());
        let path = self.root.join(&id);
        std::fs::create_dir(&path)?;
        let (schema, fields) = build_schema();
        let index = Index::create_in_dir(&path, schema)?;
        index.tokenizers().register(TOKENIZER, text_analyzer());
        let writer = index.writer(50_000_000)?;
        Ok(GenerationBuilder { id, writer, fields })
    }

    pub fn seal_rebuild(&self, mut builder: GenerationBuilder) -> Result<SealedGeneration> {
        builder
            .writer
            .commit()
            .context("Commit rebuilt full-text generation")?;
        builder.writer.wait_merging_threads()?;
        let generation = Arc::new(Generation::open(&self.root.join(&builder.id))?);
        #[cfg(unix)]
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(SealedGeneration {
            id: builder.id,
            generation,
        })
    }

    pub fn publish_generation(&self, sealed: SealedGeneration) -> Result<()> {
        let mut db = self.state()?;
        let transaction = db.transaction()?;
        transaction.execute("INSERT INTO metadata(key,value) VALUES('active_generation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [&sealed.id])?;
        transaction.execute("DELETE FROM pending_documents WHERE NOT EXISTS (SELECT 1 FROM pending_deletions WHERE pending_deletions.owner_id=pending_documents.owner_id AND pending_deletions.doc_id=pending_documents.doc_id)", [])?;
        transaction.commit()?;
        *self
            .active
            .write()
            .map_err(|_| anyhow!("Full-text generation lock poisoned"))? = Some(sealed.generation);
        Ok(())
    }

    pub fn publish_rebuild(&self, builder: GenerationBuilder) -> Result<()> {
        self.publish_generation(self.seal_rebuild(builder)?)
    }

    /// A persisted server clock keeps delta watermarks safe across fast writes
    /// and wall-clock corrections. Reserving a value without a write is harmless.
    pub fn next_write_timestamp(&self) -> Result<i64> {
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        )?;
        self.advance_timestamp(now, true)
    }

    pub fn observe_timestamp(&self, observed: i64) -> Result<()> {
        self.advance_timestamp(observed, false)?;
        Ok(())
    }

    fn advance_timestamp(&self, observed: i64, increment: bool) -> Result<i64> {
        let mut db = self.state()?;
        let transaction = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let last: Option<String> = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key='last_write_time'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let last = last
            .map(|value| value.parse::<i64>())
            .transpose()?
            .unwrap_or(0);
        let minimum = if increment {
            last.checked_add(1)
                .ok_or_else(|| anyhow!("Server write clock overflow"))?
        } else {
            last
        };
        let next = observed.max(minimum);
        transaction.execute("INSERT INTO metadata(key,value) VALUES('last_write_time',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[next.to_string()])?;
        transaction.commit()?;
        Ok(next)
    }

    pub fn mark_pending(&self, keys: &[DocumentKey]) -> Result<()> {
        let mut db = self.state()?;
        let transaction = db.transaction()?;
        for key in keys {
            transaction.execute(
                "INSERT OR IGNORE INTO pending_documents(owner_id,doc_id) VALUES(?1,?2)",
                params![key.owner_id, key.doc_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Record deletion intent before Lance publication. Recovery emits a feed
    /// event only after the vector store confirms the document is absent.
    pub fn mark_pending_delete(&self, keys: &[DocumentKey], indexed_at: i64) -> Result<()> {
        ensure!(indexed_at > 0, "Deletion timestamp must be positive");
        let mut db = self.state()?;
        let transaction = db.transaction()?;
        for key in keys {
            transaction.execute(
                "INSERT OR IGNORE INTO pending_documents(owner_id,doc_id) VALUES(?1,?2)",
                params![key.owner_id, key.doc_id],
            )?;
            transaction.execute("INSERT INTO pending_deletions(owner_id,doc_id,indexed_at) VALUES(?1,?2,?3) ON CONFLICT(owner_id,doc_id) DO UPDATE SET indexed_at=excluded.indexed_at",params![key.owner_id,key.doc_id,indexed_at])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn sync_horizon(&self) -> Result<i64> {
        let db = self.state()?;
        let value: Option<String> = db
            .query_row(
                "SELECT value FROM metadata WHERE key='last_write_time'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.map(|value| value.parse()).transpose()?.unwrap_or(0))
    }

    /// Latest deletion per identity within the frozen feed horizon. History is
    /// append-only so an older cursor still sees its original deletion event.
    pub fn tombstones_page(
        &self,
        since_ms: i64,
        horizon: i64,
        after: Option<&crisp_index_protocol::SyncCursor>,
        limit: usize,
    ) -> Result<Vec<crisp_index_protocol::SyncDeletion>> {
        ensure!(
            since_ms >= 0 && horizon >= 0,
            "Sync timestamps cannot be negative"
        );
        ensure!(
            (1..=1001).contains(&limit),
            "Deletion page limit must be between 1 and 1001"
        );
        let db = self.state()?;
        let mut query=db.prepare("WITH latest AS (SELECT owner_id,doc_id,MAX(indexed_at) AS indexed_at FROM deletion_events WHERE indexed_at>=?1 AND indexed_at<=?2 GROUP BY owner_id,doc_id) SELECT owner_id,doc_id,indexed_at FROM latest WHERE ?3 IS NULL OR (indexed_at,owner_id,doc_id)>(?3,?4,?5) ORDER BY indexed_at,owner_id,doc_id LIMIT ?6")?;
        let rows = query.query_map(
            params![
                since_ms,
                horizon,
                after.map(|cursor| cursor.indexed_at),
                after.map(|cursor| cursor.owner_id.as_str()),
                after.map(|cursor| cursor.doc_id.as_str()),
                limit as i64
            ],
            |row| {
                Ok(crisp_index_protocol::SyncDeletion {
                    owner_id: row.get(0)?,
                    doc_id: row.get(1)?,
                    indexed_at: row.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn pending(&self) -> Result<Vec<DocumentKey>> {
        let db = self.state()?;
        let mut statement = db.prepare(
            "SELECT owner_id,doc_id FROM pending_documents ORDER BY owner_id,doc_id LIMIT 256",
        )?;
        let rows = statement.query_map([], |r| {
            Ok(DocumentKey {
                owner_id: r.get(0)?,
                doc_id: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn finish_repairs(&self, documents: &[(DocumentKey, Option<FtsDocument>)]) -> Result<()> {
        if documents.is_empty() {
            return Ok(());
        }
        let generation = self.generation()?;
        let mut writer = generation.index.writer(50_000_000)?;
        for (key, document) in documents {
            writer.delete_term(Term::from_field_text(generation.fields.key, &key.encoded()));
            if let Some(document) = document {
                ensure!(&document.key == key, "Repair document identity mismatch");
                writer.add_document(index_document(document, &generation.fields)?)?;
            }
        }
        writer.commit().context("Commit full-text repairs")?;
        generation.reader.reload()?;
        let mut db = self.state()?;
        let transaction = db.transaction()?;
        for (key, document) in documents {
            if document.is_none() {
                transaction.execute("INSERT OR IGNORE INTO deletion_events(owner_id,doc_id,indexed_at) SELECT owner_id,doc_id,indexed_at FROM pending_deletions WHERE owner_id=?1 AND doc_id=?2",params![key.owner_id,key.doc_id])?;
            }
            transaction.execute(
                "DELETE FROM pending_deletions WHERE owner_id=?1 AND doc_id=?2",
                params![key.owner_id, key.doc_id],
            )?;
            transaction.execute(
                "DELETE FROM pending_documents WHERE owner_id=?1 AND doc_id=?2",
                params![key.owner_id, key.doc_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn generation(&self) -> Result<Arc<Generation>> {
        self.active
            .read()
            .map_err(|_| anyhow!("Full-text generation lock poisoned"))?
            .clone()
            .ok_or_else(|| anyhow!("Full-text rebuild has not completed"))
    }

    pub fn capabilities(&self) -> Result<SearchCapabilities> {
        Ok(SearchCapabilities {
            api_version: 1,
            metadata_filter_version: crisp_index_protocol::METADATA_FILTER_VERSION,
            stored_passage_version: crisp_index_protocol::STORED_PASSAGE_VERSION,
            query_language_version: QUERY_LANGUAGE_VERSION,
            fts_schema_version: FTS_SCHEMA_VERSION,
            fts_ready: !self.needs_rebuild()? && self.pending()?.is_empty(),
            strict_hybrid_constraints: true,
            searchable_fields: vec!["title".into(), "body".into(), "headings".into()],
            chunk_boundary_policy: "verified byte offsets join source text; legacy chunks remain separate positional regions".into(),
        })
    }

    pub fn prepare(&self, text: &str, filters: &SearchFilters) -> Result<PreparedSearch> {
        self.prepare_optional(Some(text), filters)
    }

    pub fn prepare_filters(&self, filters: &SearchFilters) -> Result<PreparedSearch> {
        self.prepare_optional(None, filters)
    }

    fn prepare_optional(
        &self,
        text: Option<&str>,
        filters: &SearchFilters,
    ) -> Result<PreparedSearch> {
        filters.validate().map_err(|error| anyhow!(error))?;
        ensure!(
            self.pending()?.is_empty(),
            "Pending full-text writes must be repaired before searching"
        );
        let generation = self.generation()?;
        let searcher = generation.reader.searcher();
        let query: Box<dyn Query> = match text {
            Some(text) => translate(text, &searcher, &generation.fields.search)?,
            None => Box::new(AllQuery),
        };
        let mut clauses = vec![(Occur::Must, query)];
        for (value, field) in [
            (&filters.owner_id, generation.fields.owner),
            (&filters.language, generation.fields.language),
        ] {
            if let Some(value) = value {
                clauses.push((
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, value),
                        IndexRecordOption::Basic,
                    )),
                ));
            }
        }
        if filters.year_min.is_some() || filters.year_max.is_some() {
            if let (Some(min), Some(max)) = (filters.year_min, filters.year_max) {
                ensure!(min <= max, "year_min exceeds year_max");
            }
            let lower = filters
                .year_min
                .map(|v| std::ops::Bound::Included(v as i64))
                .unwrap_or(std::ops::Bound::Unbounded);
            let upper = filters
                .year_max
                .map(|v| std::ops::Bound::Included(v as i64))
                .unwrap_or(std::ops::Bound::Unbounded);
            clauses.push((
                Occur::Must,
                Box::new(RangeQuery::new_i64_bounds("year".into(), lower, upper)),
            ));
        }
        Ok(PreparedSearch {
            searcher,
            query: Box::new(BooleanQuery::new(clauses)),
            fields: generation.fields.clone(),
        })
    }
}

fn index_document(document: &FtsDocument, fields: &Fields) -> Result<TantivyDocument> {
    let mut doc = TantivyDocument::default();
    doc.add_text(fields.key, document.key.encoded());
    doc.add_text(fields.doc_id, &document.key.doc_id);
    doc.add_text(fields.owner, &document.key.owner_id);
    doc.add_text(fields.language, &document.language);
    if let Some(year) = document.year {
        doc.add_i64(fields.year, year as i64);
    }
    doc.add_text(fields.search.title, &document.title);
    doc.add_text(fields.search.headings, &document.headings);
    let mut text = String::new();
    let mut tokens = Vec::<Token>::new();
    let mut base_position = 0usize;
    let mut analyzer = text_analyzer();
    for body in &document.body_segments {
        ensure!(
            text.len().saturating_add(body.len()) <= MAX_DOCUMENT_BYTES,
            "Document exceeds the 64 MiB full-text size limit"
        );
        if !text.is_empty() {
            text.push('\n');
        }
        let base_offset = text.len();
        let mut stream = analyzer.token_stream(body);
        let mut last_position = base_position;
        while stream.advance() {
            let mut token = stream.token().clone();
            token.position = token
                .position
                .checked_add(base_position)
                .ok_or_else(|| anyhow!("Document token position overflow"))?;
            token.offset_from += base_offset;
            token.offset_to += base_offset;
            last_position = token.position;
            tokens.push(token);
        }
        base_position = last_position
            .checked_add(UNKNOWN_BOUNDARY_GAP)
            .ok_or_else(|| anyhow!("Document token position overflow"))?;
        ensure!(
            base_position < u32::MAX as usize,
            "Document has too many token positions"
        );
        text.push_str(body);
    }
    doc.add_pre_tokenized_text(fields.search.body, PreTokenizedString { text, tokens });
    Ok(doc)
}

#[derive(Clone, Debug)]
pub struct FtsHit {
    pub key: DocumentKey,
    pub score: f32,
    pub snippet: String,
}

pub struct PreparedSearch {
    searcher: Searcher,
    query: Box<dyn Query>,
    fields: Fields,
}

impl PreparedSearch {
    pub fn hits(&self, limit: usize) -> Result<Vec<FtsHit>> {
        ensure!(limit <= 1000, "Full-text candidate limit exceeds 1000");
        self.hits_page(limit, 0)
    }

    pub fn document_count(&self) -> usize { self.searcher.num_docs() as usize }

    /// Keep one pinned lexical snapshot while applying scalar metadata filters.
    pub fn hits_page(&self, limit: usize, offset: usize) -> Result<Vec<FtsHit>> {
        if limit == 0 { return Ok(Vec::new()); }
        ensure!(offset.checked_add(limit).is_some(), "Full-text page range overflow");
        let mut snippets =
            SnippetGenerator::create(&self.searcher, self.query.as_ref(), self.fields.search.body)?;
        snippets.set_max_num_chars(400);
        self.searcher
            .search(&self.query, &TopDocs::with_limit(limit).and_offset(offset))?
            .into_iter()
            .map(|(score, address)| {
                let doc: TantivyDocument = self.searcher.doc(address)?;
                let key = DocumentKey {
                    owner_id: stored_str(&doc, self.fields.owner)?.into(),
                    doc_id: stored_str(&doc, self.fields.doc_id)?.into(),
                };
                let snippet = snippets.snippet_from_doc(&doc).fragment().to_owned();
                Ok(FtsHit {
                    key,
                    score,
                    snippet,
                })
            })
            .collect()
    }

    pub fn matching_keys(&self, keys: &[DocumentKey]) -> Result<HashSet<DocumentKey>> {
        ensure!(
            keys.len() <= 1000,
            "Hybrid constraint verification accepts at most 1000 candidates"
        );
        if keys.is_empty() {
            return Ok(HashSet::new());
        }
        let candidates = TermSetQuery::new(
            keys.iter()
                .map(|key| Term::from_field_text(self.fields.key, &key.encoded())),
        );
        let combined = BooleanQuery::new(vec![
            (Occur::Must, self.query.box_clone()),
            (Occur::Must, Box::new(candidates)),
        ]);
        self.searcher
            .search(&combined, &TopDocs::with_limit(keys.len()))?
            .into_iter()
            .map(|(_, address)| {
                let doc: TantivyDocument = self.searcher.doc(address)?;
                Ok(DocumentKey {
                    owner_id: stored_str(&doc, self.fields.owner)?.into(),
                    doc_id: stored_str(&doc, self.fields.doc_id)?.into(),
                })
            })
            .collect()
    }
}

fn stored_str(doc: &TantivyDocument, field: Field) -> Result<&str> {
    match doc.get_first(field) {
        Some(tantivy::schema::OwnedValue::Str(text)) => Ok(text),
        _ => bail!("Full-text document is missing a required stored identifier"),
    }
}
