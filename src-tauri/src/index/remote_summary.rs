//! Generation-bound remote lexical snapshots and streamed atomic reports.
use std::{future::Future, io::BufWriter, path::PathBuf, sync::Arc};
use anyhow::{anyhow, bail, ensure, Result};
use crisp_index_protocol::{CompleteDestinationRequest, CompleteDocument, CompletePageRequest, CompleteSearchRequest,
    CompleteSearchScope, CompleteSearchSummary, CompleteSort, COMPLETE_SEARCH_SCOPE, COMPLETE_SEARCH_VERSION,
    MAX_COMPLETE_PAGE_ROWS};
use rusqlite::{params, Connection};

use super::{remote_client::{complete_search_scope, RemoteClient}, report_output::{self, StagedReport}, schema::SearchFilters,
    search_report::{check_cancelled, ReportDocument, ReportFormat, ReportMetadata, ReportReceipt, ReportSort, ReportStream, ScanCancellation}};

pub(crate) async fn cancellable<T>(cancellation: &Arc<ScanCancellation>, work: impl Future<Output = Result<T>>) -> Result<T> {
    check_cancelled(&Some(cancellation.clone()))?;
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => bail!("The matching-document scan was cancelled"),
        result = work => result,
    }
}

/// Also owns a scan that has not returned a session yet. Dropping a cancelled
/// HTTP request closes its server-side work by the original request identity.
struct SessionLease { client: Arc<RemoteClient>, request_id: String, closed: bool }
impl SessionLease {
    fn new(client: Arc<RemoteClient>, request_id: String) -> Self { Self { client, request_id, closed: false } }
    async fn close(&mut self) {
        if self.client.close_complete_search(&self.request_id).await.is_ok() { self.closed = true; }
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        if self.closed { return; }
        let client = self.client.clone(); let request_id = self.request_id.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { let _ = client.close_complete_search(&request_id).await; });
        }
    }
}

pub async fn summarize(client: Arc<RemoteClient>, scope: CompleteSearchScope, request_id: String,
    cancellation: Arc<ScanCancellation>) -> Result<CompleteSearchSummary> {
    let mut lease = SessionLease::new(client.clone(), request_id.clone());
    let request = CompleteSearchRequest { scope: scope.clone(), request_id, expected_generation: None };
    let result = async {
        let summary = cancellable(&cancellation, client.complete_search(&request)).await?;
        let generation = cancellable(&cancellation, client.complete_generation(&scope)).await?;
        ensure!(generation.generation == summary.generation,
            "The remote index changed while counting matches. Count again to refresh the totals.");
        Ok(summary)
    }.await;
    lease.close().await;
    check_cancelled(&Some(cancellation))?;
    result
}

pub struct RemoteExportOptions {
    pub request_id: String,
    pub path: PathBuf,
    pub format: ReportFormat,
    pub sort: ReportSort,
    pub expected_generation: String,
    pub protected_roots: Vec<PathBuf>,
}

pub struct PreparedRemoteReport {
    staging: StagedReport,
    receipt: ReportReceipt,
    session: SessionLease,
    destination: CompleteDestinationRequest,
    cancellation: Arc<ScanCancellation>,
}
impl PreparedRemoteReport {
    pub fn destination(&self) -> &std::path::Path { self.staging.destination() }

    /// The desktop adapter keeps its active configuration stable and may add
    /// local source/index validation in this final synchronous callback.
    pub async fn publish(self, validate_local: impl FnOnce() -> Result<()> + Send + 'static) -> Result<ReportReceipt> {
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let mut session = self.session;
            let mut receipt = self.receipt;
            let publication = self.staging.publish(|| {
                check_cancelled(&Some(self.cancellation.clone()))?;
                runtime.block_on(cancellable(&self.cancellation, session.client.complete_destination(&self.destination)))?;
                validate_local()?;
                self.cancellation.begin_publish()?;
                Ok(())
            });
            self.cancellation.finish_publish();
            runtime.block_on(session.close());
            receipt.path = publication?;
            Ok(receipt)
        }).await?
    }
}

pub async fn prepare_export(client: Arc<RemoteClient>, query: String, filters: SearchFilters,
    options: RemoteExportOptions, cancellation: Arc<ScanCancellation>) -> Result<PreparedRemoteReport> {
    let scope = complete_search_scope(&query, &filters)?;
    let generation = client.unbind_generation(&options.expected_generation)?;
    let session = SessionLease::new(client.clone(), options.request_id.clone());
    let request = CompleteSearchRequest { scope: scope.clone(), request_id: options.request_id,
        expected_generation: Some(generation.clone()) };
    let summary = cancellable(&cancellation, client.complete_search(&request)).await?;
    let destination = CompleteDestinationRequest { version: COMPLETE_SEARCH_VERSION, request_id: request.request_id.clone(), generation: generation.clone(),
        scope: scope.clone(), destination_paths: report_output::destination_path_aliases(&options.path)? };
    cancellable(&cancellation, client.complete_destination(&destination)).await?;
    let metadata = ReportMetadata { scope: COMPLETE_SEARCH_SCOPE, query, effective_query: scope.query.clone(),
        filters: serde_json::to_value(&filters)?, generation: options.expected_generation.clone(),
        matching_documents: summary.matching_documents, sort: options.sort };
    let format = options.format; let sort = options.sort;
    let setup_cancellation = cancellation.clone();
    let mut output = tokio::task::spawn_blocking(move || {
        let staging = StagedReport::new(&options.path, &options.protected_roots, &[])?;
        RemoteOutput::new(staging, metadata, format, sort, setup_cancellation)
    }).await??;
    let mut offset = 0;
    loop {
        let page_request = CompletePageRequest { version: COMPLETE_SEARCH_VERSION, session_id: summary.session_id.clone(),
            generation: generation.clone(), offset, limit: MAX_COMPLETE_PAGE_ROWS, sort: wire_sort(sort) };
        let page = cancellable(&cancellation, client.complete_page(&scope, &page_request, summary.matching_documents)).await?;
        let next = page.next_offset;
        output = tokio::task::spawn_blocking(move || { output.push(page.documents)?; Ok::<_, anyhow::Error>(output) }).await??;
        if let Some(next) = next { offset = next; } else { break; }
    }
    let staging = tokio::task::spawn_blocking(move || output.finish()).await??;
    check_cancelled(&Some(cancellation.clone()))?;
    // Page generation checks protect membership; the final destination receipt
    // repeats a full-source collision check after local output flush/sync.
    let receipt = ReportReceipt { path: staging.destination().to_owned(), documents: summary.matching_documents,
        scope: COMPLETE_SEARCH_SCOPE, generation: options.expected_generation, format, sort };
    Ok(PreparedRemoteReport { staging, receipt, session, destination, cancellation })
}

fn wire_sort(sort: ReportSort) -> CompleteSort {
    match sort { ReportSort::Filename => CompleteSort::Filename, ReportSort::Title => CompleteSort::Title,
        ReportSort::YearAsc => CompleteSort::YearAsc, ReportSort::YearDesc => CompleteSort::YearDesc,
        ReportSort::Location => CompleteSort::Location }
}

fn report_document(value: CompleteDocument) -> ReportDocument {
    ReportDocument { doc_id: value.doc_id, owner_id: value.owner_id, chunk_index: value.representative_chunk_index,
        container_member: value.container_member,
        title: value.title, filename: value.filename, author: value.author, location_uri: value.location_uri,
        url: value.url, source_hash: Some(value.source_hash), indexed_at: Some(value.indexed_at),
        extension: value.extension, language: value.language, year: value.year, tags: value.tags }
}

/// The server sends globally sorted bounded pages. Keep only the last record
/// in memory and disk-backed identities to reject duplicates across pages.
struct RemoteOutput {
    staging: StagedReport,
    writer: ReportStream,
    seen: Connection,
    _file: tempfile::NamedTempFile,
    previous: Option<ReportDocument>,
    sort: ReportSort,
    cancellation: Arc<ScanCancellation>,
}
impl RemoteOutput {
    fn new(mut staging: StagedReport, metadata: ReportMetadata, format: ReportFormat, sort: ReportSort,
        cancellation: Arc<ScanCancellation>) -> Result<Self> {
        check_cancelled(&Some(cancellation.clone()))?;
        let file = tempfile::NamedTempFile::new()?;
        let seen = Connection::open(file.path())?;
        let progress = cancellation.clone();
        seen.progress_handler(1000, Some(move || progress.is_cancelled()))?;
        seen.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
            PRAGMA temp_store=FILE; PRAGMA mmap_size=0;
            CREATE TABLE seen(owner TEXT NOT NULL,doc_id TEXT NOT NULL,PRIMARY KEY(owner,doc_id)) WITHOUT ROWID;")?;
        let writer = ReportStream::start(staging.file_mut(), metadata, format)?;
        Ok(Self { staging, writer, seen, _file: file, previous: None, sort, cancellation })
    }

    fn push(&mut self, documents: Vec<CompleteDocument>) -> Result<()> {
        ensure!(documents.len() <= MAX_COMPLETE_PAGE_ROWS, "The remote report page exceeds its row limit");
        let transaction = self.seen.transaction()?;
        let destination = self.staging.destination().to_owned();
        let mut output = BufWriter::new(self.staging.file_mut());
        for document in documents {
            check_cancelled(&Some(self.cancellation.clone()))?;
            let document = report_document(document);
            report_output::reject_source_destination(&destination, &document.location_uri)?;
            if let Some(member) = &document.container_member {
                report_output::reject_source_destination(&destination,&member.root_location_uri)?;
            }
            ensure!(self.previous.as_ref().is_none_or(|previous| self.sort.compare(previous, &document).is_lt()),
                "The server returned duplicated or out-of-order complete results. The previous report was preserved.");
            transaction.execute("INSERT INTO seen(owner,doc_id) VALUES (?1,?2)", params![document.owner_id, document.doc_id])
                .map_err(|error| anyhow!("The server repeated a document across complete report pages: {error}"))?;
            self.writer.push(&mut output, &document)?;
            self.previous = Some(document);
        }
        std::io::Write::flush(&mut output)?;
        transaction.commit()?;
        Ok(())
    }

    fn finish(mut self) -> Result<StagedReport> {
        check_cancelled(&Some(self.cancellation.clone()))?;
        self.writer.finish(self.staging.file_mut())?;
        Ok(self.staging)
    }
}
