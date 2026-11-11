//! Disk-backed complete lexical match sets. Immutable scan handles keep source
//! rows and lexical membership aligned; live-generation checks reject drift.
use super::{DocumentKey, PreparedSearch, SearchIndex, TABLE_NAME};
use anyhow::{anyhow, ensure, Context, Result};
use arrow_array::{Array, Int32Array, ListArray, RecordBatch, StringArray, TimestampMillisecondArray};
use crisp_index_protocol::*;
use futures_util::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions, Select};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeSet, HashMap}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

const SESSION_LIFETIME: Duration = Duration::from_secs(15 * 60);
const SCAN_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_SESSIONS: usize = 8;
const MAX_SCANS: usize = 2;
const MAX_DOCUMENTS: u64 = 1_000_000;

#[derive(Debug)]
pub enum CompleteError { Changed, Cancelled, Expired, Busy, TimedOut }
impl std::fmt::Display for CompleteError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::Changed => "The index or query scope changed; count matches again before exporting",
            Self::Cancelled => "The complete match scan was cancelled",
            Self::Expired => "The complete match session expired or is unavailable; count matches again",
            Self::Busy => "The server's complete match scan capacity is in use; close an earlier scan and retry",
            Self::TimedOut => "The complete match scan exceeded 120 seconds; narrow the source scope and retry",
        })
    }
}
impl std::error::Error for CompleteError {}

#[derive(Default)]
struct Cancellation { cancelled: AtomicBool, changed: tokio::sync::Notify }
impl Cancellation {
    fn cancel(&self) { self.cancelled.store(true,Ordering::Release); self.changed.notify_waiters(); }
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) { return Err(CompleteError::Cancelled.into()); } Ok(())
    }
    async fn cancelled(&self) {
        loop {
            let changed = self.changed.notified(); tokio::pin!(changed); changed.as_mut().enable();
            if self.cancelled.load(Ordering::Acquire) { return; }
            changed.await;
        }
    }
}
struct Ready { file: tempfile::TempPath, summary: CompleteSearchSummary, scope: CompleteSearchScope }
struct Session {
    request_id: String, id: String, created: Instant, cancellation: Arc<Cancellation>,
    ready: Mutex<Option<Arc<Ready>>>,
}
#[derive(Default)]
struct RegistryState { sessions: HashMap<String,Arc<Session>>, cancelled: HashMap<String,Instant> }
#[derive(Default)]
pub(super) struct CompleteRegistry { state: Mutex<RegistryState>, reaper_started: AtomicBool }
impl CompleteRegistry {
    fn clean(state: &mut RegistryState) {
        state.sessions.retain(|_, session| {
            let keep = session.created.elapsed() < SESSION_LIFETIME && !session.cancellation.cancelled.load(Ordering::Acquire);
            if !keep { session.cancellation.cancel(); } keep
        });
        state.cancelled.retain(|_,created|created.elapsed() < SESSION_LIFETIME);
    }
    fn start(self: &Arc<Self>, request_id: &str) -> Result<Arc<Session>> {
        let mut state = self.state.lock().map_err(|_|anyhow!("Complete scan registry lock failed"))?;
        Self::clean(&mut state);
        if state.cancelled.contains_key(request_id) { return Err(CompleteError::Cancelled.into()); }
        ensure!(!state.sessions.contains_key(request_id), "Complete search request ID is already in use");
        let building = state.sessions.values().filter(|session|session.ready.lock().map(|value|value.is_none()).unwrap_or(true)).count();
        if state.sessions.len() >= MAX_SESSIONS || building >= MAX_SCANS { return Err(CompleteError::Busy.into()); }
        let session = Arc::new(Session { request_id:request_id.into(),id:uuid::Uuid::new_v4().to_string(),created:Instant::now(),
            cancellation:Arc::new(Cancellation::default()),ready:Mutex::new(None) });
        state.sessions.insert(request_id.into(),session.clone());
        if !self.reaper_started.swap(true,Ordering::AcqRel) {
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    let Some(registry) = weak.upgrade() else { break; };
                    if let Ok(mut state) = registry.state.lock() { CompleteRegistry::clean(&mut state); };
                }
            });
        }
        Ok(session)
    }
    fn find(&self, id: &str) -> Result<Arc<Session>> {
        let mut state = self.state.lock().map_err(|_|anyhow!("Complete scan registry lock failed"))?;
        Self::clean(&mut state);
        state.sessions.values().find(|session|session.id == id).cloned().ok_or_else(||CompleteError::Expired.into())
    }
    pub(super) fn cancel(&self, request_id: &str) -> Result<()> {
        let mut state = self.state.lock().map_err(|_|anyhow!("Complete scan registry lock failed"))?;
        Self::clean(&mut state);
        if let Some(session) = state.sessions.remove(request_id) { session.cancellation.cancel(); }
        // Remember an early cancellation so request arrival order cannot revive
        // a scan. The bounded tombstone set also expires automatically.
        if state.cancelled.len() >= 256 {
            if let Some(oldest) = state.cancelled.iter().min_by_key(|(_,time)|**time).map(|(id,_)|id.clone()) { state.cancelled.remove(&oldest); }
        }
        state.cancelled.insert(request_id.into(),Instant::now());
        Ok(())
    }
}
struct BuildGuard { registry:Arc<CompleteRegistry>, session:Arc<Session>, complete:bool }
impl Drop for BuildGuard {
    fn drop(&mut self) {
        if !self.complete { self.session.cancellation.cancel(); let _ = self.registry.cancel(&self.session.request_id); }
    }
}

impl SearchIndex {
    async fn complete_generation_locked(&self, scope:&CompleteSearchScope) -> Result<String> {
        let mut hash = Sha256::new();
        hash.update(self.fts.snapshot_stamp()?.as_bytes());
        hash.update(self.vector.table.version().await?.to_le_bytes());
        hash.update(serde_json::to_vec(scope)?);
        Ok(hash.finalize().iter().map(|byte|format!("{byte:02x}")).collect())
    }
    pub async fn complete_generation(&self, scope:&CompleteSearchScope) -> Result<CompleteGeneration> {
        scope.validate().map_err(|error|anyhow!(error))?;
        let _guard = self.read_guard().await?;
        self.fts.prepare(&scope.query,&scope.filters)?;
        Ok(CompleteGeneration {version:COMPLETE_SEARCH_VERSION,scope:COMPLETE_SEARCH_SCOPE.into(),query:scope.query.clone(),filters:scope.filters.clone(),
            generation:self.complete_generation_locked(scope).await?})
    }
    pub async fn complete_summary(&self, request:CompleteSearchRequest) -> Result<CompleteSearchSummary> {
        request.validate().map_err(|error|anyhow!(error))?;
        let session = self.complete.start(&request.request_id)?;
        let mut guard = BuildGuard {registry:self.complete.clone(),session:session.clone(),complete:false};
        let work = self.build_complete(&request,&session);
        let result = tokio::select! {
            biased;
            _ = session.cancellation.cancelled() => Err(CompleteError::Cancelled.into()),
            value = tokio::time::timeout(SCAN_TIMEOUT,work) => match value { Ok(value)=>value,Err(_)=>Err(CompleteError::TimedOut.into()) },
        }?;
        session.cancellation.check()?;
        guard.complete = true;
        Ok(result)
    }
    async fn build_complete(&self, request:&CompleteSearchRequest, session:&Arc<Session>) -> Result<CompleteSearchSummary> {
        let cancellation = session.cancellation.clone();
        let guard = self.read_guard().await?;
        cancellation.check()?;
        let generation = self.complete_generation_locked(&request.scope).await?;
        if request.expected_generation.as_ref().is_some_and(|expected|expected != &generation) { return Err(CompleteError::Changed.into()); }
        let table = self.vector._db.open_table(TABLE_NAME).execute().await?;
        table.checkout(self.vector.table.version().await?).await?;
        let prepared = Arc::new(self.fts.prepare(&request.scope.query,&request.scope.filters)?);
        drop(guard);
        let filter = request.scope.filters.to_lance_sql().map(|sql|format!("chunk_index >= 0 AND ({sql})"))
            .unwrap_or_else(||"chunk_index >= 0".into());
        let mut options = QueryExecutionOptions::default(); options.max_batch_length = 128;
        let mut rows = table.query().only_if(filter).select(Select::columns(&[
            "owner_id","doc_id","chunk_index","title","filename","author","location_uri","url","source_hash","indexed_at","ext","language","year","tags","container_member",
        ])).execute_with_options(options).await?;
        let cancel = cancellation.clone();
        let mut accumulator = tokio::task::spawn_blocking(move || Accumulator::new(cancel)).await??;
        while let Some(batch) = rows.try_next().await? {
            cancellation.check()?;
            let prepared = prepared.clone();
            accumulator = tokio::task::spawn_blocking(move || { accumulator.add_batch(&batch,&prepared)?; Ok::<_,anyhow::Error>(accumulator) }).await??;
        }
        let (file,matching_documents,scanned_rows,facets) = tokio::task::spawn_blocking(move || accumulator.finish()).await??;
        cancellation.check()?;
        let _guard = self.read_guard().await?;
        if self.complete_generation_locked(&request.scope).await? != generation { return Err(CompleteError::Changed.into()); }
        let summary = CompleteSearchSummary {version:COMPLETE_SEARCH_VERSION,scope:COMPLETE_SEARCH_SCOPE.into(),query:request.scope.query.clone(),filters:request.scope.filters.clone(),
            generation,request_id:request.request_id.clone(),session_id:session.id.clone(),expires_at_ms:now_ms()+SESSION_LIFETIME.saturating_sub(session.created.elapsed()).as_millis() as i64,
            matching_documents,scanned_rows,facet_value_limit:COMPLETE_FACET_VALUE_LIMIT,facets};
        summary.validate_for(request).map_err(|error|anyhow!(error))?;
        cancellation.check()?;
        *session.ready.lock().map_err(|_|anyhow!("Complete session lock failed"))? = Some(Arc::new(Ready {file,summary:summary.clone(),scope:request.scope.clone()}));
        Ok(summary)
    }
    pub async fn complete_page(&self, request:CompletePageRequest) -> Result<CompletePage> {
        request.validate().map_err(|error|anyhow!(error))?;
        let session = self.complete.find(&request.session_id)?;
        session.cancellation.check()?;
        tokio::select! {
            biased;
            _ = session.cancellation.cancelled() => Err(CompleteError::Cancelled.into()),
            result = tokio::time::timeout(SCAN_TIMEOUT, async {
        let ready = session.ready.lock().map_err(|_|anyhow!("Complete session lock failed"))?.clone().ok_or(CompleteError::Busy)?;
        if request.generation != ready.summary.generation || self.complete_generation(&ready.scope).await?.generation != request.generation {
            return Err(CompleteError::Changed.into());
        }
        ensure!(request.offset <= ready.summary.matching_documents,"Complete page offset exceeds match count");
        let read = ready.clone(); let page_request = request.clone(); let cancellation = session.cancellation.clone();
        let result = tokio::task::spawn_blocking(move || read_page(&read,&page_request,&cancellation)).await??;
        session.cancellation.check()?;
        if self.complete_generation(&ready.scope).await?.generation != request.generation { return Err(CompleteError::Changed.into()); }
        result.validate_for(&ready.scope,&request).map_err(|error|anyhow!(error))?;
        Ok(result)
            }) => match result {
                Ok(value)=>value,
                Err(_)=>{ let _=self.complete.cancel(&session.request_id); Err(CompleteError::TimedOut.into()) },
            },
        }
    }

    pub fn cancel_complete(&self, request_id:&str) -> Result<CompleteCancelResponse> {
        ensure!(valid_request_id(request_id),"Invalid complete request ID");
        self.complete.cancel(request_id)?;
        Ok(CompleteCancelResponse {version:COMPLETE_SEARCH_VERSION,request_id:request_id.into(),cancelled:true})
    }
    pub async fn complete_destination(&self, request:CompleteDestinationRequest) -> Result<CompleteDestinationResponse> {
        request.validate().map_err(|error|anyhow!(error))?;
        let session={
            let mut state=self.complete.state.lock().map_err(|_|anyhow!("Complete scan registry lock failed"))?;
            CompleteRegistry::clean(&mut state);
            state.sessions.get(&request.request_id).cloned().ok_or(CompleteError::Expired)?
        };
        let ready=session.ready.lock().map_err(|_|anyhow!("Complete session lock failed"))?.clone().ok_or(CompleteError::Busy)?;
        if ready.scope != request.scope || ready.summary.generation != request.generation { return Err(CompleteError::Changed.into()); }
        tokio::select! {
            biased;
            _ = session.cancellation.cancelled() => Err(CompleteError::Cancelled.into()),
            result = tokio::time::timeout(SCAN_TIMEOUT,async {
        let _guard = self.read_guard().await?;
        if self.complete_generation_locked(&request.scope).await? != request.generation { return Err(CompleteError::Changed.into()); }
        let paths: BTreeSet<_> = request.destination_paths.iter().flat_map(|path|crate::source_paths::lexical_path_aliases(path)).collect();
        ensure!(!paths.is_empty(),"Destination has no absolute lexical path");
        let mut options = QueryExecutionOptions::default(); options.max_batch_length=256;
        let mut rows=self.vector.table.query().select(Select::columns(&["location_uri"])).execute_with_options(options).await?;
        let mut protected=false;
        while let Some(batch)=rows.try_next().await? {
            session.cancellation.check()?;
            let uris=batch.column_by_name("location_uri").and_then(|c|c.as_any().downcast_ref::<StringArray>()).context("Missing stored source URI")?;
            if uris.iter().flatten().any(|uri|crate::source_paths::source_path_aliases(uri).iter().any(|path|paths.contains(path))) { protected=true;break; }
        }
        Ok(CompleteDestinationResponse {version:COMPLETE_SEARCH_VERSION,generation:request.generation,protected})
            }) => match result {
                Ok(value)=>value,
                Err(_)=>{ let _=self.complete.cancel(&session.request_id); Err(CompleteError::TimedOut.into()) },
            },
        }
    }
}

fn now_ms()->i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64 }
// Close SQLite before deleting its file, including on cancellation and on
// platforms that forbid unlinking an open database.
struct Accumulator { database:Connection, file:tempfile::TempPath, cancellation:Arc<Cancellation>, scanned:u64 }
impl Accumulator {
    fn new(cancellation:Arc<Cancellation>) -> Result<Self> {
        cancellation.check()?;
        let file=tempfile::Builder::new().prefix("crisp-complete-").suffix(".sqlite").tempfile()?.into_temp_path();
        let database=Connection::open(&file)?;
        database.execute_batch("PRAGMA page_size=4096; PRAGMA max_page_count=131072; PRAGMA cache_size=-2048; PRAGMA temp_store=FILE;
            CREATE TABLE documents(owner TEXT NOT NULL,doc_id TEXT NOT NULL,representative TEXT NOT NULL,metadata TEXT NOT NULL,
                sort_title TEXT NOT NULL,sort_filename TEXT NOT NULL,sort_year INTEGER,sort_location TEXT NOT NULL,member_receipt TEXT NOT NULL,PRIMARY KEY(owner,doc_id)) WITHOUT ROWID;
            CREATE TRIGGER consistent_member_receipt BEFORE INSERT ON documents
                WHEN EXISTS(SELECT 1 FROM documents WHERE owner=NEW.owner AND doc_id=NEW.doc_id AND member_receipt!=NEW.member_receipt)
                BEGIN SELECT RAISE(ABORT,'Stored member chunks have inconsistent provenance; refresh the root source'); END;
            CREATE TABLE facets(kind TEXT NOT NULL,value TEXT NOT NULL,owner TEXT NOT NULL,doc_id TEXT NOT NULL,PRIMARY KEY(kind,value,owner,doc_id)) WITHOUT ROWID;")?;
        let check=cancellation.clone(); database.progress_handler(1000,Some(move ||check.cancelled.load(Ordering::Acquire)))?;
        Ok(Self {file,database,cancellation,scanned:0})
    }
    fn add_batch(&mut self,batch:&RecordBatch,prepared:&PreparedSearch)->Result<()> {
        self.cancellation.check()?;
        let mut rows=Vec::with_capacity(batch.num_rows());
        for i in 0..batch.num_rows() { rows.push(document_from_row(batch,i)?); }
        self.scanned=self.scanned.checked_add(rows.len() as u64).context("Scanned row count overflow")?;
        let keys:Vec<_>=rows.iter().map(|row|DocumentKey {owner_id:row.owner_id.clone(),doc_id:row.doc_id.clone()}).collect();
        let allowed=prepared.matching_keys(&keys)?;
        self.cancellation.check()?;
        let transaction=self.database.transaction()?;
        {
            let mut doc=transaction.prepare_cached("INSERT INTO documents VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                ON CONFLICT(owner,doc_id) DO UPDATE SET representative=excluded.representative,metadata=excluded.metadata,sort_title=excluded.sort_title,
                sort_filename=excluded.sort_filename,sort_year=excluded.sort_year,sort_location=excluded.sort_location WHERE excluded.representative<documents.representative")?;
            let mut facet=transaction.prepare_cached("INSERT OR IGNORE INTO facets VALUES(?1,?2,?3,?4)")?;
            let mut clear=transaction.prepare_cached("DELETE FROM facets WHERE owner=?1 AND doc_id=?2 AND kind!='tag'")?;
            for row in rows {
                self.cancellation.check()?;
                if !allowed.contains(&DocumentKey {owner_id:row.owner_id.clone(),doc_id:row.doc_id.clone()}) {continue;}
                row.validate_container_provenance().map_err(anyhow::Error::msg)?;
                let metadata=serde_json::to_string(&row)?;
                ensure!(metadata.len()<=128*1024,"One complete result record exceeds 128 KiB; reduce oversized source metadata");
                let representative=format!("{:010}:{metadata}",row.representative_chunk_index);
                if doc.execute(params![row.owner_id,row.doc_id,representative,metadata,row.title.as_deref().or(row.filename.as_deref()).unwrap_or_default(),row.filename.as_deref().unwrap_or_default(),row.year,row.location_uri,serde_json::to_string(&row.container_member)?])?>0 {
                    clear.execute(params![row.owner_id,row.doc_id])?;
                    for (kind,value) in [("extension",row.extension.clone()),("language",row.language.clone()),("year",row.year.map(|year|year.to_string()))] {
                        if let Some(value)=value {if !value.is_empty(){facet.execute(params![kind,value,row.owner_id,row.doc_id])?;}}
                    }
                }
                for tag in row.tags {if !tag.is_empty(){facet.execute(params!["tag",tag,row.owner_id,row.doc_id])?;}}
            }
        }
        transaction.commit()?;
        Ok(())
    }
    fn facet(&self,kind:&str)->Result<CompleteFacet> {
        let distinct=self.database.query_row("SELECT COUNT(*) FROM (SELECT value FROM facets WHERE kind=?1 GROUP BY value)",[kind],|row|row.get::<_,i64>(0))? as u64;
        let missing=self.database.query_row("SELECT COUNT(*) FROM documents d WHERE NOT EXISTS(SELECT 1 FROM facets f WHERE f.kind=?1 AND f.owner=d.owner AND f.doc_id=d.doc_id)",[kind],|row|row.get::<_,i64>(0))? as u64;
        let mut query=self.database.prepare("SELECT value,COUNT(*) AS n FROM facets WHERE kind=?1 GROUP BY value ORDER BY n DESC,value ASC LIMIT ?2")?;
        let values=query.query_map(params![kind,COMPLETE_FACET_VALUE_LIMIT as i64],|row|Ok(CompleteFacetValue {value:row.get(0)?,documents:row.get::<_,i64>(1)? as u64}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(CompleteFacet {omitted_values:distinct.saturating_sub(values.len() as u64),distinct_values:distinct,values,missing_documents:missing})
    }
    fn finish(self)->Result<(tempfile::TempPath,u64,u64,CompleteFacets)> {
        self.cancellation.check()?;
        let count=self.database.query_row("SELECT COUNT(*) FROM documents",[],|row|row.get::<_,i64>(0))? as u64;
        ensure!(count<=MAX_DOCUMENTS,"Complete search exceeds one million documents; narrow the source scope");
        let facets=CompleteFacets {extensions:self.facet("extension")?,languages:self.facet("language")?,years:self.facet("year")?,tags:self.facet("tag")?};
        self.database.execute_batch("CREATE INDEX by_filename ON documents(sort_filename COLLATE NOCASE,owner,doc_id);
            CREATE INDEX by_title ON documents(sort_title COLLATE NOCASE,owner,doc_id);
            CREATE INDEX by_location ON documents(sort_location COLLATE NOCASE,owner,doc_id);
            CREATE INDEX by_year_asc ON documents((sort_year IS NULL),sort_year,owner,doc_id);
            CREATE INDEX by_year_desc ON documents((sort_year IS NULL),sort_year DESC,owner,doc_id);")?;
        self.cancellation.check()?;
        drop(self.database);
        Ok((self.file,count,self.scanned,facets))
    }
}
fn sort_sql(sort:CompleteSort)->&'static str {
    match sort {
        CompleteSort::Filename=>"sort_filename COLLATE NOCASE,owner,doc_id",
        CompleteSort::Title=>"sort_title COLLATE NOCASE,owner,doc_id",
        CompleteSort::Location=>"sort_location COLLATE NOCASE,owner,doc_id",
        CompleteSort::YearAsc=>"sort_year IS NULL,sort_year,owner,doc_id",
        CompleteSort::YearDesc=>"sort_year IS NULL,sort_year DESC,owner,doc_id",
    }
}
fn read_page(ready:&Ready,request:&CompletePageRequest,cancellation:&Arc<Cancellation>)->Result<CompletePage> {
    cancellation.check()?;
    let database=Connection::open_with_flags(&ready.file,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let cancel=cancellation.clone();database.progress_handler(1000,Some(move ||cancel.cancelled.load(Ordering::Acquire)))?;
    let mut query=database.prepare(&format!("SELECT metadata FROM documents ORDER BY {} LIMIT ?1 OFFSET ?2",sort_sql(request.sort)))?;
    let mut records=query.query(params![request.limit as i64,request.offset as i64])?;
    let mut result=CompletePage {version:COMPLETE_SEARCH_VERSION,scope:COMPLETE_SEARCH_SCOPE.into(),query:ready.scope.query.clone(),filters:ready.scope.filters.clone(),
        generation:ready.summary.generation.clone(),offset:request.offset,documents:Vec::new(),next_offset:None,matching_documents:ready.summary.matching_documents};
    let mut bytes=serde_json::to_vec(&result)?.len()+128;
    while let Some(record)=records.next()? {
        cancellation.check()?;
        let metadata:String=record.get(0)?;
        let mut document:CompleteDocument=serde_json::from_str(&metadata)?;
        let mut tags=database.prepare("SELECT value FROM facets WHERE kind='tag' AND owner=?1 AND doc_id=?2 ORDER BY value")?;
        document.tags.clear();
        let mut record_bytes=serde_json::to_vec(&document)?.len();
        let mut stored_tags=tags.query(params![document.owner_id,document.doc_id])?;
        while let Some(row)=stored_tags.next()? {
            cancellation.check()?;
            let tag:String=row.get(0)?;
            record_bytes=record_bytes.checked_add(serde_json::to_vec(&tag)?.len()+1).context("Tag size overflow")?;
            ensure!(document.tags.len()<4096 && record_bytes<=128*1024,
                "One document's combined tags exceed the 4096-tag or 128 KiB report record limit");
            document.tags.push(tag);
        }
        let size=serde_json::to_vec(&document)?.len()+1;
        if bytes+size>MAX_COMPLETE_RESPONSE_BYTES {
            ensure!(!result.documents.is_empty(),"One complete result exceeds the response byte limit");break;
        }
        bytes+=size;result.documents.push(document);
    }
    let end=result.offset+result.documents.len() as u64;
    result.next_offset=(end<result.matching_documents).then_some(end);
    ensure!(serde_json::to_vec(&result)?.len()<=MAX_COMPLETE_RESPONSE_BYTES,"Complete result page exceeds its encoded limit");
    Ok(result)
}
fn document_from_row(batch:&RecordBatch,row:usize)->Result<CompleteDocument> {
    let string=|name:&str|batch.column_by_name(name).and_then(|c|c.as_any().downcast_ref::<StringArray>()).filter(|c|!c.is_null(row)).map(|c|c.value(row).to_owned());
    let number=|name:&str|batch.column_by_name(name).and_then(|c|c.as_any().downcast_ref::<Int32Array>()).filter(|c|!c.is_null(row)).map(|c|c.value(row));
    let tags=batch.column_by_name("tags").and_then(|c|c.as_any().downcast_ref::<ListArray>()).filter(|c|!c.is_null(row)).map(|c|c.value(row))
        .and_then(|value|value.as_any().downcast_ref::<StringArray>().map(|v|v.iter().flatten().filter(|v|!v.is_empty()).map(str::to_owned).collect())).unwrap_or_default();
    Ok(CompleteDocument {doc_id:string("doc_id").context("Missing stored document ID")?,owner_id:string("owner_id").context("Missing stored owner")?,
        representative_chunk_index:number("chunk_index").context("Missing stored chunk index")?,title:string("title"),filename:string("filename"),author:string("author"),
        location_uri:string("location_uri").context("Missing source URI")?,url:string("url"),source_hash:string("source_hash").unwrap_or_default(),
        indexed_at:batch.column_by_name("indexed_at").and_then(|c|c.as_any().downcast_ref::<TimestampMillisecondArray>()).filter(|c|!c.is_null(row)).map(|c|c.value(row)).unwrap_or(0),
        extension:string("ext").filter(|v|!v.is_empty()),language:string("language").filter(|v|!v.is_empty()),year:number("year"),tags,
        container_member:string("container_member").as_deref().map(serde_json::from_str).transpose()?})
}
