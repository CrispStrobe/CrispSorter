//! P29.1 — Transfer queue with backpressure.
//!
//! Bounded-concurrency queue for cloud drive uploads and downloads.
//! Uses a `tokio::sync::Semaphore` to cap concurrent transfers (default 3)
//! and exponential backoff with jitter on transient failures.
//!
//! # Design
//!
//! The `CloudDrive` trait is synchronous (`reqwest::blocking`), so each
//! transfer runs inside `spawn_blocking`.  The semaphore permit is held
//! for the entire duration of the blocking call, ensuring the concurrency
//! bound is respected.
//!
//! Progress is broadcast per-job via `tokio::sync::watch` channels.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::OnceLock;
use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::JoinHandle;

// ── Configuration ────────────────────────────────────────────────────────

/// Maximum concurrent transfers (semaphore permits).
const DEFAULT_MAX_CONCURRENT: usize = 3;

/// Maximum retry attempts before a job is considered permanently failed.
const MAX_RETRIES: u32 = 5;

/// Base delay for exponential backoff (milliseconds).
const BACKOFF_BASE_MS: u64 = 500;

/// Maximum backoff delay cap (milliseconds).
const BACKOFF_CAP_MS: u64 = 30_000;

// ── Types ────────────────────────────────────────────────────────────────

/// Monotonically increasing job ID.
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
static SHARED_QUEUE: OnceLock<TransferQueue> = OnceLock::new();

/// Direction of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Upload,
    Download,
}

/// Progress state of a single transfer job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgress {
    pub job_id: u64,
    pub direction: TransferDirection,
    pub drive_id: String,
    pub remote_path: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// Durable provider checkpoint associated with this job, when the
    /// caller supplied one.  This is metadata only: the provider operation
    /// remains responsible for validating and consuming the checkpoint.
    pub resume_state: Option<String>,
    pub state: TransferState,
}

/// Lifecycle state of a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Waiting for a semaphore permit.
    Queued,
    /// Actively transferring.
    Active,
    /// Waiting before a retry attempt.
    Retrying { attempt: u32 },
    /// Completed successfully.
    Done,
    /// Failed after all retries exhausted.
    Failed { error: String },
    /// Cancelled by the user.
    Cancelled,
}

/// A transfer job submitted to the queue.  The caller holds on to the
/// `progress_rx` to monitor progress and the `handle` to await completion.
pub struct TransferHandle<T = Vec<u8>> {
    pub job_id: u64,
    pub progress_rx: watch::Receiver<TransferProgress>,
    pub handle: JoinHandle<Result<T>>,
    pub cancellation: TransferCancellation,
}

/// Cancellation control for a queued or active transfer.
#[derive(Clone, Default)]
pub struct TransferCancellation {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<Notify>,
    publication: Arc<Mutex<bool>>,
}

struct JobRecord {
    progress_rx: watch::Receiver<TransferProgress>,
    cancellation: TransferCancellation,
}

impl TransferCancellation {
    pub fn cancel(&self) {
        // A completed publication is final. The lock makes cancellation and
        // the final rename mutually exclusive; a late click cannot report a
        // successfully published file as cancelled.
        let published = self.publication.lock().expect("transfer publication lock poisoned");
        if *published { return; }
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn publish<T>(&self, publish: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut published = self.publication.lock().expect("transfer publication lock poisoned");
        anyhow::ensure!(!self.is_cancelled(), "transfer cancelled");
        let result = publish();
        if result.is_ok() || result.as_ref().err().is_some_and(|error|
            error.downcast_ref::<crate::drives::NonReplayableWrite>().is_some()) {
            // A lost publication acknowledgement cannot be undone by cancellation.
            // Keep an uncertain write failed and prevent automatic replay.
            *published = true;
            self.cancelled.store(false, Ordering::Release);
        }
        result
    }

    /// A synchronous provider returning success has already committed its
    /// upload. Cancellation cannot undo that mutation, even when requested
    /// while the provider was writing or waiting for its acknowledgement.
    fn uploaded(&self) {
        let mut published = self.publication.lock().expect("transfer publication lock poisoned");
        *published = true;
        self.cancelled.store(false, Ordering::Release);
    }
}

/// Closure that performs the actual I/O.  Receives `(drive, remote_path,
/// local_data)`.  For downloads, `local_data` is empty and the closure
/// returns the downloaded bytes.  For uploads, `local_data` contains the
/// payload and the closure returns an empty vec on success.
type TransferFn<T> = Arc<dyn Fn() -> Result<T> + Send + Sync + 'static>;

trait TransferOutput {
    fn byte_count(&self) -> u64;
}
impl TransferOutput for Vec<u8> {
    fn byte_count(&self) -> u64 { self.len() as u64 }
}
impl TransferOutput for u64 {
    fn byte_count(&self) -> u64 { *self }
}

struct CancellableWriter<'a> {
    writer: &'a mut dyn Write,
    cancellation: &'a TransferCancellation,
}
impl Write for CancellableWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancellation.is_cancelled() {
            // Interrupted is retried by write_all/io::copy, so cancellation
            // must use a terminal I/O error kind.
            return Err(std::io::Error::other("transfer cancelled"));
        }
        self.writer.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(std::io::Error::other("transfer cancelled"));
        }
        self.writer.flush()
    }
}

// ── TransferQueue ────────────────────────────────────────────────────────

/// Bounded-concurrency transfer queue.
#[derive(Clone)]
pub struct TransferQueue {
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    jobs: Arc<Mutex<HashMap<u64, JobRecord>>>,
}

impl TransferQueue {
    /// Return the process-wide application queue.  GUI, CLI-in-process
    /// helpers, and FUSE boundaries use this accessor so they share the same
    /// semaphore, cancellation registry, and terminal-job snapshots.
    pub fn shared() -> Self {
        SHARED_QUEUE.get_or_init(Self::new).clone()
    }

    /// Create a new queue with the default concurrency limit (3).
    pub fn new() -> Self {
        Self::with_concurrency(DEFAULT_MAX_CONCURRENT)
    }

    /// Create a new queue with a custom concurrency limit.
    pub fn with_concurrency(max_concurrent: usize) -> Self {
        let max = max_concurrent.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(max)),
            max_concurrent: max,
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Number of permits (concurrent transfer slots).
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Number of currently active transfers.
    pub fn active_count(&self) -> usize {
        self.max_concurrent - self.semaphore.available_permits()
    }

    /// Return a bounded snapshot for the transfer drawer and automation
    /// clients.  Recent terminal jobs are retained so the UI can show the
    /// result of a transfer that completed between polls.
    pub fn snapshot(&self) -> Vec<TransferProgress> {
        let mut jobs: Vec<_> = self
            .jobs
            .lock()
            .expect("transfer queue job registry poisoned")
            .values()
            .map(|job| job.progress_rx.borrow().clone())
            .collect();
        jobs.sort_by_key(|job| job.job_id);
        jobs
    }

    /// Cancel a queued or active job by ID. Returns `false` when the job is
    /// not known to this application queue.
    pub fn cancel(&self, job_id: u64) -> bool {
        let jobs = self
            .jobs
            .lock()
            .expect("transfer queue job registry poisoned");
        if let Some(job) = jobs.get(&job_id) {
            job.cancellation.cancel();
            true
        } else {
            false
        }
    }

    fn register_job(
        &self,
        job_id: u64,
        progress_rx: watch::Receiver<TransferProgress>,
        cancellation: TransferCancellation,
    ) {
        let mut jobs = self
            .jobs
            .lock()
            .expect("transfer queue job registry poisoned");
        jobs.insert(
            job_id,
            JobRecord {
                progress_rx,
                cancellation,
            },
        );
        // Evict the oldest terminal entries only; active jobs must remain
        // cancellable even when many transfers have been submitted.
        while jobs.len() > 256 {
            let candidate = jobs
                .iter()
                .filter(|(_, job)| {
                    matches!(
                        &job.progress_rx.borrow().state,
                        TransferState::Done
                            | TransferState::Failed { .. }
                            | TransferState::Cancelled
                    )
                })
                .map(|(id, _)| *id)
                .min();
            match candidate {
                Some(id) => {
                    jobs.remove(&id);
                }
                None => break,
            }
        }
    }

    /// Submit an upload job.  The `data` bytes will be written to
    /// `remote_path` on the drive identified by `drive_id`.
    ///
    /// The returned `TransferHandle` lets the caller monitor progress and
    /// await completion.  The `JoinHandle` resolves to an empty `Vec<u8>`
    /// on success.
    pub fn submit_upload(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        data: Vec<u8>,
        write_fn: impl Fn(&Path, &[u8]) -> Result<()> + Send + Sync + 'static,
    ) -> TransferHandle {
        self.submit_upload_with_resume(drive_id, remote_path, data, None, write_fn)
    }

    /// Submit an upload and expose its durable provider checkpoint in queue
    /// snapshots.  The ordinary upload API remains unchanged for callers
    /// that do not support native resume.
    pub fn submit_upload_with_resume(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        data: Vec<u8>,
        resume_state: Option<PathBuf>,
        write_fn: impl Fn(&Path, &[u8]) -> Result<()> + Send + Sync + 'static,
    ) -> TransferHandle {
        let size = data.len() as u64;
        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);

        let initial = TransferProgress {
            job_id,
            direction: TransferDirection::Upload,
            drive_id: drive_id.clone(),
            remote_path: remote_path.to_string_lossy().into_owned(),
            bytes_done: 0,
            bytes_total: Some(size),
            resume_state: resume_state.as_ref().map(|p| p.display().to_string()),
            state: TransferState::Queued,
        };
        let (tx, rx) = watch::channel(initial);
        let sem = self.semaphore.clone();
        let cancellation = TransferCancellation::default();
        let task_cancellation = cancellation.clone();
        self.register_job(job_id, rx.clone(), cancellation.clone());

        let handle = tokio::spawn(async move {
            run_with_retries(
                job_id,
                TransferDirection::Upload,
                drive_id,
                remote_path.clone(),
                Some(size),
                resume_state.as_ref().map(|p| p.display().to_string()),
                sem,
                tx,
                task_cancellation,
                Arc::new(move || {
                    write_fn(&remote_path, &data)?;
                    Ok(Vec::new())
                }),
            )
            .await
        });

        TransferHandle {
            job_id,
            progress_rx: rx,
            handle,
            cancellation,
        }
    }

    /// Submit a download job.  The file at `remote_path` on the drive
    /// identified by `drive_id` will be read.
    ///
    /// The `JoinHandle` resolves to the downloaded bytes on success.
    pub fn submit_download(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        size_hint: Option<u64>,
        read_fn: impl Fn(&Path) -> Result<Vec<u8>> + Send + Sync + 'static,
    ) -> TransferHandle {
        self.submit_download_with_resume(drive_id, remote_path, size_hint, None, read_fn)
    }

    /// Submit a download with optional durable provider checkpoint metadata.
    pub fn submit_download_with_resume(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        size_hint: Option<u64>,
        resume_state: Option<PathBuf>,
        read_fn: impl Fn(&Path) -> Result<Vec<u8>> + Send + Sync + 'static,
    ) -> TransferHandle {
        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);

        let initial = TransferProgress {
            job_id,
            direction: TransferDirection::Download,
            drive_id: drive_id.clone(),
            remote_path: remote_path.to_string_lossy().into_owned(),
            bytes_done: 0,
            bytes_total: size_hint,
            resume_state: resume_state.as_ref().map(|p| p.display().to_string()),
            state: TransferState::Queued,
        };
        let (tx, rx) = watch::channel(initial);
        let sem = self.semaphore.clone();
        let cancellation = TransferCancellation::default();
        let task_cancellation = cancellation.clone();
        self.register_job(job_id, rx.clone(), cancellation.clone());

        let handle = tokio::spawn(async move {
            run_with_retries(
                job_id,
                TransferDirection::Download,
                drive_id,
                remote_path.clone(),
                size_hint,
                resume_state.as_ref().map(|p| p.display().to_string()),
                sem,
                tx,
                task_cancellation,
                Arc::new(move || read_fn(&remote_path)),
            )
            .await
        });

        TransferHandle {
            job_id,
            progress_rx: rx,
            handle,
            cancellation,
        }
    }

    /// Submit a transfer against an owned drive handle. Keeping this adapter
    /// here makes the queue boundary explicit for provider call sites and
    /// avoids repeating subtly different `Arc` capture closures.
    pub fn submit_drive_upload(
        &self,
        drive_id: String,
        drive: Arc<dyn crate::drives::CloudDrive>,
        remote_path: PathBuf,
        data: Vec<u8>,
    ) -> TransferHandle {
        self.submit_upload(drive_id, remote_path, data, move |path, data| {
            drive.write_file(path, data)
        })
    }

    /// Download through an owned provider handle and the shared queue.
    pub fn submit_drive_download(
        &self,
        drive_id: String,
        drive: Arc<dyn crate::drives::CloudDrive>,
        remote_path: PathBuf,
        size_hint: Option<u64>,
    ) -> TransferHandle {
        self.submit_download(drive_id, remote_path, size_hint, move |path| {
            drive.read_file(path)
        })
    }

    /// Byte-array IPC has an explicit memory limit. File downloads, restore,
    /// sync and index staging use `submit_drive_download_to_file` instead.
    pub fn submit_drive_download_bounded(
        &self,
        drive_id: String,
        drive: Arc<dyn crate::drives::CloudDrive>,
        remote_path: PathBuf,
        limit: u64,
    ) -> TransferHandle {
        self.submit_download(drive_id, remote_path, None, move |path| {
            crate::drives::read_file_bounded(drive.as_ref(), path, limit)
        })
    }

    /// Stream into a private file beside the destination, verify its size,
    /// and publish only a completed transfer. Each retry starts with a fresh
    /// private stage; failures before publication and cancellation before commit
    /// preserve the previous destination. A publication error may already have
    /// replaced it, so it stays failed and is never automatically retried.
    pub fn submit_drive_download_to_file(
        &self,
        drive_id: String,
        drive: Arc<dyn crate::drives::CloudDrive>,
        remote_path: PathBuf,
        local_path: PathBuf,
        size_hint: Option<u64>,
    ) -> TransferHandle<u64> {
        self.submit_download_to_file(drive_id, remote_path, local_path, size_hint,
            move |path, writer| drive.read_file_to_writer(path, writer))
    }

    pub fn submit_download_to_file(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        local_path: PathBuf,
        size_hint: Option<u64>,
        read_fn: impl Fn(&Path, &mut dyn Write) -> Result<u64> + Send + Sync + 'static,
    ) -> TransferHandle<u64> {
        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let initial = TransferProgress {
            job_id, direction: TransferDirection::Download, drive_id: drive_id.clone(),
            remote_path: remote_path.to_string_lossy().into_owned(), bytes_done: 0,
            bytes_total: size_hint, resume_state: None, state: TransferState::Queued,
        };
        let (tx, rx) = watch::channel(initial);
        let sem = self.semaphore.clone();
        let cancellation = TransferCancellation::default();
        let task_cancellation = cancellation.clone();
        let write_cancellation = cancellation.clone();
        self.register_job(job_id, rx.clone(), cancellation.clone());
        let handle = tokio::spawn(async move {
            let source_path = remote_path.clone();
            run_with_retries(job_id, TransferDirection::Download, drive_id, remote_path,
                size_hint, None, sem, tx, task_cancellation, Arc::new(move || {
                    let log = crate::operation_log::OperationLog::start("storage.materialize",
                        serde_json::json!({"mode":"stream", "job_id":job_id}));
                    let result = (|| {
                        let stage = crate::drives::materialize::stage_file(&local_path, size_hint, |writer| {
                            let mut writer = CancellableWriter { writer, cancellation: &write_cancellation };
                            read_fn(&source_path, &mut writer)
                        })?;
                        log.step("validated", serde_json::json!({"bytes":stage.bytes()}));
                        write_cancellation.publish(|| stage.publish())
                    })();
                    match &result {
                        Ok(bytes) => log.succeed(serde_json::json!({"bytes":bytes})),
                        Err(_) if write_cancellation.is_cancelled() => log.cancel(),
                        Err(_) => log.fail("materialization_failed"),
                    }
                    result
                })).await
        });
        TransferHandle { job_id, progress_rx: rx, handle, cancellation }
    }

    /// Synchronous file-target adapter for filesystem/FUSE consumers. It uses
    /// the same semaphore, cancellation registry and staged publication path.
    pub fn download_to_file_blocking(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        local_path: PathBuf,
        size_hint: Option<u64>,
        read_fn: impl Fn(&Path, &mut dyn Write) -> Result<u64> + Send + Sync + 'static,
    ) -> Result<u64> {
        let queue = self.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().context("creating transfer queue runtime")?;
            let _guard = runtime.enter();
            let transfer = queue.submit_download_to_file(drive_id, remote_path, local_path, size_hint, read_fn);
            runtime.block_on(transfer.handle).context("transfer queue task failed")?
        }).join().map_err(|_| anyhow!("transfer queue worker panicked"))?
    }

    /// Run an upload from a synchronous caller while still using this queue's
    /// semaphore, retry policy, and job registry.  A short-lived runtime is
    /// isolated on a worker thread so this is safe from FUSE/provider code
    /// that cannot await the async queue.
    pub fn upload_blocking(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        data: Vec<u8>,
        write_fn: impl Fn(&Path, &[u8]) -> Result<()> + Send + Sync + 'static,
    ) -> Result<()> {
        let queue = self.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new()
                .context("creating transfer queue runtime")?;
            let _guard = runtime.enter();
            let transfer = queue.submit_upload(drive_id, remote_path, data, write_fn);
            match runtime.block_on(transfer.handle) {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(anyhow!("transfer queue task failed: {error}")),
            }
        })
        .join()
        .map_err(|_| anyhow!("transfer queue worker panicked"))?
    }

    /// Run a download from a synchronous caller through the shared queue.
    pub fn download_blocking(
        &self,
        drive_id: String,
        remote_path: PathBuf,
        size_hint: Option<u64>,
        read_fn: impl Fn(&Path) -> Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Result<Vec<u8>> {
        let queue = self.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new()
                .context("creating transfer queue runtime")?;
            let _guard = runtime.enter();
            let transfer = queue.submit_download(drive_id, remote_path, size_hint, read_fn);
            match runtime.block_on(transfer.handle) {
                Ok(Ok(data)) => Ok(data),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(anyhow!("transfer queue task failed: {error}")),
            }
        })
        .join()
        .map_err(|_| anyhow!("transfer queue worker panicked"))?
    }
}

impl Default for TransferQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ── Internal: retry loop ─────────────────────────────────────────────────

/// Compute backoff duration: min(base * 2^attempt, cap) + jitter.
fn backoff_duration(attempt: u32) -> std::time::Duration {
    let base = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(12));
    let capped = base.min(BACKOFF_CAP_MS);
    // Simple jitter: ±25% via wrapping arithmetic on the attempt counter.
    let jitter_pct = ((attempt as u64).wrapping_mul(7) % 50) as i64 - 25;
    let jittered = (capped as i64 + (capped as i64 * jitter_pct / 100)).max(100) as u64;
    std::time::Duration::from_millis(jittered)
}

/// Run a transfer with semaphore gating and retries.
///
/// Because `CloudDrive` methods are synchronous, the actual I/O closure
/// runs inside `spawn_blocking`.  On transient failure, we release the
/// semaphore permit, sleep with backoff, then re-acquire.
///
async fn run_with_retries<T: TransferOutput + Send + 'static>(
    job_id: u64,
    direction: TransferDirection,
    drive_id: String,
    remote_path: PathBuf,
    bytes_total: Option<u64>,
    resume_state: Option<String>,
    semaphore: Arc<Semaphore>,
    tx: watch::Sender<TransferProgress>,
    cancellation: TransferCancellation,
    transfer_fn: TransferFn<T>,
) -> Result<T> {
    let operation = match direction {
        TransferDirection::Upload => "storage.upload",
        TransferDirection::Download => "storage.download",
    };
    let log = crate::operation_log::OperationLog::start(operation,
        serde_json::json!({"mode":if direction == TransferDirection::Upload { "upload" } else { "download" },
            "bytes":bytes_total, "job_id":job_id}));
    let remote_str = remote_path.to_string_lossy().into_owned();

    let make_progress = |state: TransferState, bytes_done: u64| TransferProgress {
        job_id,
        direction,
        drive_id: drive_id.clone(),
        remote_path: remote_str.clone(),
        bytes_done,
        bytes_total,
        resume_state: resume_state.clone(),
        state,
    };

    for attempt in 0..=MAX_RETRIES {
        if cancellation.is_cancelled() {
            let _ = tx.send(make_progress(TransferState::Cancelled, 0));
            log.cancel();
            return Err(anyhow!("transfer cancelled"));
        }
        // Acquire a fresh permit for every attempt. This prevents a sleeping
        // retry from occupying a transfer slot.
        let permit = tokio::select! {
            permit = semaphore.acquire() => permit.context("transfer queue semaphore closed")?,
            _ = cancellation.notify.notified() => {
                let _ = tx.send(make_progress(TransferState::Cancelled, 0));
                log.cancel();
                return Err(anyhow!("transfer cancelled"));
            }
        };
        if cancellation.is_cancelled() {
            let _ = tx.send(make_progress(TransferState::Cancelled, 0));
            log.cancel();
            return Err(anyhow!("transfer cancelled"));
        }
        let _ = tx.send(make_progress(TransferState::Active, 0));
        log.step("transfer", serde_json::json!({"attempt":attempt + 1}));
        let transfer = transfer_fn.clone();
        let result = match tokio::task::spawn_blocking(move || transfer()).await {
            Ok(result) => result,
            Err(error) => {
                let _ = tx.send(make_progress(TransferState::Failed { error: "transfer task panicked".into() }, 0));
                log.fail("transfer_task_panicked");
                return Err(error).context("transfer task panicked");
            }
        };
        drop(permit);

        if direction == TransferDirection::Upload && result.is_ok() {
            cancellation.uploaded();
        }
        if cancellation.is_cancelled() {
            let _ = tx.send(make_progress(TransferState::Cancelled, 0));
            log.cancel();
            // Preserve a provider's failure as the cause when cancellation
            // stopped further attempts; it must never be turned into success.
            return match result {
                Err(error) => Err(error.context("transfer cancelled")),
                Ok(_) => Err(anyhow!("transfer cancelled")),
            };
        }

        match result {
            Ok(bytes) => {
                let done = if direction == TransferDirection::Upload {
                    bytes_total.unwrap_or_else(|| bytes.byte_count())
                } else { bytes.byte_count() };
                let _ = tx.send(make_progress(TransferState::Done, done));
                log.succeed(serde_json::json!({"bytes":done}));
                return Ok(bytes);
            }
            Err(error) if attempt < MAX_RETRIES && is_retryable(&error) => {
                log.warn("transfer_retry", serde_json::json!({"attempt":attempt + 1}));
                let _ = tx.send(make_progress(
                    TransferState::Retrying {
                        attempt: attempt + 1,
                    },
                    0,
                ));
                tokio::select! {
                    _ = tokio::time::sleep(backoff_duration(attempt)) => {}
                    _ = cancellation.notify.notified() => {
                        let _ = tx.send(make_progress(TransferState::Cancelled, 0));
                        log.cancel();
                        return Err(anyhow!("transfer cancelled"));
                    }
                }
            }
            Err(error) => {
                let _ = tx.send(make_progress(
                    TransferState::Failed {
                        error: format!("{error:#}"),
                    },
                    0,
                ));
                log.fail("transfer_failed");
                return Err(error);
            }
        }
    }
    unreachable!("retry loop always returns")
}

fn is_retryable(error: &anyhow::Error) -> bool {
    if error.downcast_ref::<crate::drives::NonReplayableWrite>().is_some() {
        return false;
    }
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "tempor",
        "connection",
        "network",
        "429",
        "500",
        "502",
        "503",
        "504",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    #[test]
    fn shared_accessor_reuses_one_job_registry() {
        let first = TransferQueue::shared();
        let second = TransferQueue::shared();
        assert!(Arc::ptr_eq(&first.jobs, &second.jobs));
        assert_eq!(first.max_concurrent(), second.max_concurrent());
    }

    #[test]
    fn backoff_increases_with_attempt() {
        let d0 = backoff_duration(0);
        let d1 = backoff_duration(1);
        let d3 = backoff_duration(3);
        // Each attempt should be >= previous (modulo jitter).
        // With attempt 0: base = 500ms, attempt 3: base = 4000ms (capped at 30s).
        assert!(d0.as_millis() <= 1000, "attempt 0 too high: {:?}", d0);
        assert!(d1.as_millis() >= 500, "attempt 1 too low: {:?}", d1);
        assert!(d3.as_millis() >= 2000, "attempt 3 too low: {:?}", d3);
    }

    #[test]
    fn backoff_caps_at_max() {
        let d20 = backoff_duration(20);
        // Should be capped at BACKOFF_CAP_MS (30s) ± 25% jitter.
        assert!(
            d20.as_millis() <= 40_000,
            "attempt 20 exceeded cap: {:?}",
            d20
        );
    }

    #[test]
    fn blocking_adapter_runs_upload_and_download_through_registry() {
        let queue = TransferQueue::with_concurrency(1);
        let uploaded = Arc::new(Mutex::new(Vec::new()));
        let uploaded_for_write = uploaded.clone();
        queue
            .upload_blocking(
                "drive".into(),
                PathBuf::from("remote.txt"),
                b"upload".to_vec(),
                move |_path, data| {
                    uploaded_for_write.lock().unwrap().extend_from_slice(data);
                    Ok(())
                },
            )
            .unwrap();

        let downloaded = queue
            .download_blocking(
                "drive".into(),
                PathBuf::from("remote.txt"),
                Some(8),
                |_path| Ok(b"download".to_vec()),
            )
            .unwrap();

        assert_eq!(&*uploaded.lock().unwrap(), b"upload");
        assert_eq!(downloaded, b"download");
        let snapshot = queue.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.iter().all(|job| job.state == TransferState::Done));
    }

    #[tokio::test]
    async fn queue_respects_concurrency_limit() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let queue = TransferQueue::with_concurrency(2);

        let mut handles = Vec::new();
        for i in 0..6 {
            let active = active.clone();
            let max_seen = max_seen.clone();
            let h = queue.submit_upload(
                format!("drive-{i}"),
                PathBuf::from(format!("file-{i}.txt")),
                vec![i as u8; 10],
                move |_path, _data| {
                    let prev = active.fetch_add(1, Ordering::SeqCst);
                    // Record the maximum concurrent count.
                    let current = prev + 1;
                    max_seen.fetch_max(current, Ordering::SeqCst);
                    // Simulate some work.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                },
            );
            handles.push(h);
        }

        // Wait for all to complete.
        for h in handles {
            h.handle.await.unwrap().unwrap();
        }

        let observed_max = max_seen.load(Ordering::SeqCst);
        assert!(
            observed_max <= 2,
            "max concurrent {observed_max} exceeded limit of 2"
        );
    }

    #[tokio::test]
    async fn owned_drive_adapters_route_read_and_write_through_queue() {
        let root = tempfile::tempdir().unwrap();
        let drive: Arc<dyn crate::drives::CloudDrive> =
            Arc::new(crate::drives::LocalDrive::new("test-local", root.path()));
        let queue = TransferQueue::with_concurrency(1);

        let upload = queue.submit_drive_upload(
            "local-test".into(),
            drive.clone(),
            PathBuf::from("nested/file.txt"),
            b"queued provider bytes".to_vec(),
        );
        upload.handle.await.unwrap().unwrap();

        let download = queue.submit_drive_download(
            "local-test".into(),
            drive,
            PathBuf::from("nested/file.txt"),
            Some(21),
        );
        let bytes = download.handle.await.unwrap().unwrap();
        assert_eq!(bytes, b"queued provider bytes");
        assert_eq!(queue.snapshot().len(), 2);
        assert!(queue
            .snapshot()
            .iter()
            .all(|job| job.state == TransferState::Done));
    }

    struct StreamingOnlyDrive {
        size: usize,
        writes: Arc<AtomicUsize>,
    }

    impl crate::drives::CloudDrive for StreamingOnlyDrive {
        fn label(&self) -> &str { "streaming-only" }
        fn drive_type(&self) -> crate::drives::DriveType { crate::drives::DriveType::Local }
        fn list_dir(&self, _: &Path) -> Result<Vec<crate::drives::DirEntry>> { Ok(Vec::new()) }
        fn read_file(&self, _: &Path) -> Result<Vec<u8>> {
            panic!("disk-bound consumer called the whole-file API")
        }
        fn write_file(&self, _: &Path, _: &[u8]) -> Result<()> { anyhow::bail!("read-only test drive") }
        fn delete(&self, _: &Path) -> Result<()> { anyhow::bail!("read-only test drive") }
        fn stat(&self, _: &Path) -> Result<crate::drives::FileStat> {
            Ok(crate::drives::FileStat { size: self.size as u64, is_dir: false, mtime_unix: None })
        }
        fn read_file_to_writer(&self, _: &Path, writer: &mut dyn Write) -> Result<u64> {
            let chunk = [73u8; 4096];
            let mut remaining = self.size;
            while remaining > 0 {
                let length = remaining.min(chunk.len());
                writer.write_all(&chunk[..length])?;
                self.writes.fetch_add(1, Ordering::SeqCst);
                remaining -= length;
            }
            Ok(self.size as u64)
        }
    }

    #[tokio::test]
    async fn file_target_uses_streaming_provider_and_returns_only_a_byte_count() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("result.bin");
        let writes = Arc::new(AtomicUsize::new(0));
        let size = 8 * 1024 * 1024 + 7;
        let drive = Arc::new(StreamingOnlyDrive { size, writes: writes.clone() });
        let queue = TransferQueue::new();
        let transfer = queue.submit_drive_download_to_file("mounted".into(), drive,
            "large.bin".into(), destination.clone(), Some(size as u64));
        assert_eq!(transfer.handle.await.unwrap().unwrap(), size as u64);
        assert_eq!(std::fs::metadata(&destination).unwrap().len(), size as u64);
        assert!(std::fs::read(&destination).unwrap().iter().all(|byte| *byte == 73));
        assert_eq!(writes.load(Ordering::SeqCst), size.div_ceil(4096));
        assert_eq!(transfer.progress_rx.borrow().bytes_done, size as u64);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn partial_retry_starts_a_fresh_private_file_and_preserves_previous_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("result.bin");
        std::fs::write(&destination, b"previous").unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let checked_destination = destination.clone();
        let counted = attempts.clone();
        let queue = TransferQueue::new();
        let transfer = queue.submit_download_to_file("mounted".into(), "source.bin".into(),
            destination.clone(), Some(4), move |_, writer| {
                assert_eq!(std::fs::read(&checked_destination)?, b"previous");
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    writer.write_all(b"bad")?;
                    anyhow::bail!("transient network timeout");
                }
                writer.write_all(b"good")?;
                Ok(4)
            });
        assert_eq!(transfer.handle.await.unwrap().unwrap(), 4);
        assert_eq!(std::fs::read(destination).unwrap(), b"good");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn file_target_size_mismatch_never_replaces_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("result.bin");
        std::fs::write(&destination, b"previous").unwrap();
        let queue = TransferQueue::new();
        let transfer = queue.submit_download_to_file("mounted".into(), "source.bin".into(),
            destination.clone(), Some(20), |_, writer| {
                writer.write_all(b"short")?;
                Ok(5)
            });
        assert!(transfer.handle.await.unwrap().is_err());
        assert!(matches!(transfer.progress_rx.borrow().state, TransferState::Failed { .. }));
        assert_eq!(std::fs::read(destination).unwrap(), b"previous");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn active_file_cancellation_stops_writer_and_leaves_previous_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("result.bin");
        std::fs::write(&destination, b"previous").unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let release = Mutex::new(Some(release_rx));
        let queue = TransferQueue::new();
        let transfer = queue.submit_download_to_file("mounted".into(), "source.bin".into(),
            destination.clone(), None, move |_, writer| {
                writer.write_all(b"partial")?;
                started.lock().unwrap().take().unwrap().send(()).unwrap();
                tokio::runtime::Handle::current().block_on(release.lock().unwrap().take().unwrap()).unwrap();
                writer.write_all(b"must not be written")?;
                panic!("cancellation did not stop the streaming producer");
            });
        started_rx.await.unwrap();
        transfer.cancellation.cancel();
        release_tx.send(()).unwrap();
        assert!(transfer.handle.await.unwrap().is_err());
        assert_eq!(transfer.progress_rx.borrow().state, TransferState::Cancelled);
        assert_eq!(std::fs::read(destination).unwrap(), b"previous");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn file_target_obeys_queue_backpressure_and_queued_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let queue = TransferQueue::with_concurrency(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let release = Mutex::new(Some(release_rx));
        let first = queue.submit_download_to_file("mounted".into(), "one".into(),
            root.path().join("one"), Some(1), move |_, writer| {
                started.lock().unwrap().take().unwrap().send(()).unwrap();
                tokio::runtime::Handle::current().block_on(release.lock().unwrap().take().unwrap()).unwrap();
                writer.write_all(b"1")?;
                Ok(1)
            });
        started_rx.await.unwrap();
        let second = queue.submit_download_to_file("mounted".into(), "two".into(),
            root.path().join("two"), None, |_, _| panic!("queued cancellation performed I/O"));
        assert_eq!(second.progress_rx.borrow().state, TransferState::Queued);
        assert_eq!(queue.active_count(), 1);
        second.cancellation.cancel();
        assert!(second.handle.await.unwrap().is_err());
        assert_eq!(second.progress_rx.borrow().state, TransferState::Cancelled);
        assert!(!root.path().join("two").exists());
        release_tx.send(()).unwrap();
        first.handle.await.unwrap().unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn cancellation_after_publication_keeps_completed_receipt() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("result.bin");
        let queue = TransferQueue::new();
        let transfer = queue.submit_download_to_file("mounted".into(), "source.bin".into(),
            destination.clone(), Some(4), |_, writer| { writer.write_all(b"done")?; Ok(4) });
        transfer.handle.await.unwrap().unwrap();
        transfer.cancellation.cancel();
        assert!(!transfer.cancellation.is_cancelled());
        assert_eq!(transfer.progress_rx.borrow().state, TransferState::Done);
        assert_eq!(std::fs::read(destination).unwrap(), b"done");
    }

    #[tokio::test]
    async fn provider_panic_records_failure_and_drops_the_private_file() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("result.bin");
        let queue = TransferQueue::new();
        let transfer = queue.submit_download_to_file("mounted".into(), "source.bin".into(),
            destination.clone(), None, |_, writer| {
                writer.write_all(b"partial")?;
                panic!("provider panic");
            });
        assert!(transfer.handle.await.unwrap().is_err());
        assert!(matches!(transfer.progress_rx.borrow().state, TransferState::Failed { .. }));
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn upload_reports_done_state() {
        let queue = TransferQueue::new();
        let h = queue.submit_upload(
            "test-drive".into(),
            PathBuf::from("doc.pdf"),
            b"hello world".to_vec(),
            |_path, _data| Ok(()),
        );

        h.handle.await.unwrap().unwrap();

        let progress = h.progress_rx.borrow().clone();
        assert_eq!(progress.state, TransferState::Done);
        assert_eq!(progress.bytes_total, Some(11));
    }

    #[tokio::test]
    async fn confirmed_upload_success_wins_cancellation_during_write_or_acknowledgement() {
        for publish_before_cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("uploaded.bin");
            let provider_destination = destination.clone();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let started = Mutex::new(Some(started_tx));
            let release = Mutex::new(Some(release_rx));
            let queue = TransferQueue::with_concurrency(1);
            let transfer = queue.submit_upload("provider".into(), "uploaded.bin".into(),
                b"committed".to_vec(), move |_, bytes| {
                    if publish_before_cancel { std::fs::write(&provider_destination, bytes)?; }
                    started.lock().unwrap().take().unwrap().send(()).unwrap();
                    tokio::runtime::Handle::current().block_on(release.lock().unwrap().take().unwrap()).unwrap();
                    if !publish_before_cancel { std::fs::write(&provider_destination, bytes)?; }
                    Ok(())
                });
            started_rx.await.unwrap();
            assert!(queue.cancel(transfer.job_id));
            release_tx.send(()).unwrap();
            transfer.handle.await.unwrap().unwrap();
            assert_eq!(std::fs::read(destination).unwrap(), b"committed");
            assert_eq!(transfer.progress_rx.borrow().state, TransferState::Done);
            assert_eq!(transfer.progress_rx.borrow().bytes_done, 9);
            assert!(!transfer.cancellation.is_cancelled());
            transfer.cancellation.cancel();
            assert!(!transfer.cancellation.is_cancelled(), "published upload accepted a late cancellation");
            assert_eq!(queue.snapshot()[0].state, TransferState::Done);
        }
    }

    #[tokio::test]
    async fn failed_upload_is_never_completed_and_cancel_preserves_the_provider_error() {
        for cancel in [false, true] {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let started = Mutex::new(Some(started_tx));
            let release = Mutex::new(Some(release_rx));
            let queue = TransferQueue::with_concurrency(1);
            let transfer = queue.submit_upload("provider".into(), "rejected.bin".into(),
                b"payload".to_vec(), move |_, _| {
                    started.lock().unwrap().take().unwrap().send(()).unwrap();
                    tokio::runtime::Handle::current().block_on(release.lock().unwrap().take().unwrap()).unwrap();
                    anyhow::bail!("permanent provider rejection")
                });
            started_rx.await.unwrap();
            if cancel { transfer.cancellation.cancel(); }
            release_tx.send(()).unwrap();
            let error = transfer.handle.await.unwrap().unwrap_err();
            assert!(format!("{error:#}").contains("permanent provider rejection"));
            let state = transfer.progress_rx.borrow().state.clone();
            if cancel { assert_eq!(state, TransferState::Cancelled); }
            else { assert!(matches!(state, TransferState::Failed { .. })); }
            assert_eq!(transfer.progress_rx.borrow().bytes_done, 0);
        }
    }

    #[tokio::test]
    async fn queued_upload_cancellation_prevents_provider_invocation() {
        let queue = TransferQueue::with_concurrency(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started_tx));
        let release = Mutex::new(Some(release_rx));
        let first = queue.submit_upload("provider".into(), "first.bin".into(), vec![1], move |_, _| {
            started.lock().unwrap().take().unwrap().send(()).unwrap();
            tokio::runtime::Handle::current().block_on(release.lock().unwrap().take().unwrap()).unwrap();
            Ok(())
        });
        started_rx.await.unwrap();
        let second = queue.submit_upload("provider".into(), "second.bin".into(), vec![2],
            |_, _| panic!("cancelled queued upload reached the provider"));
        assert_eq!(second.progress_rx.borrow().state, TransferState::Queued);
        second.cancellation.cancel();
        assert!(second.handle.await.unwrap().is_err());
        assert_eq!(second.progress_rx.borrow().state, TransferState::Cancelled);
        release_tx.send(()).unwrap();
        first.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn possibly_published_upload_is_not_retried_after_transient_failure() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("published.bin");
        let destination = target.clone();
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&attempts);
        let queue = TransferQueue::with_concurrency(1);
        let transfer = queue.submit_upload("smb".into(), "published.bin".into(), b"bytes".to_vec(),
            move |_, bytes| {
                calls.fetch_add(1, Ordering::SeqCst);
                std::fs::write(&destination, bytes)?;
                Err(crate::drives::NonReplayableWrite::mark(anyhow!("connection timeout after publication")))
            });
        let error = transfer.handle.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("connection timeout after publication"));
        assert!(error.downcast_ref::<crate::drives::NonReplayableWrite>().is_some());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(target).unwrap(), b"bytes");
        assert!(matches!(&queue.snapshot()[0].state, TransferState::Failed { .. }));
    }

    #[tokio::test]
    async fn resume_checkpoint_is_exposed_in_progress_and_snapshot() {
        let queue = TransferQueue::new();
        let checkpoint = tempfile::tempdir().unwrap().path().join("upload-state.json");
        let h = queue.submit_upload_with_resume(
            "native-drive".into(),
            PathBuf::from("large.bin"),
            vec![1, 2, 3],
            Some(checkpoint.clone()),
            |_path, _data| Ok(()),
        );

        assert_eq!(
            h.progress_rx.borrow().resume_state.as_deref(),
            Some(checkpoint.to_string_lossy().as_ref())
        );
        h.handle.await.unwrap().unwrap();
        assert_eq!(
            queue.snapshot()[0].resume_state.as_deref(),
            Some(checkpoint.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn snapshot_retains_completed_job() {
        let queue = TransferQueue::new();
        let h = queue.submit_upload(
            "snapshot-drive".into(),
            PathBuf::from("doc.txt"),
            b"hello".to_vec(),
            |_path, _data| Ok(()),
        );
        let job_id = h.job_id;
        h.handle.await.unwrap().unwrap();

        let jobs = queue.snapshot();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, job_id);
        assert_eq!(jobs[0].state, TransferState::Done);
    }

    #[tokio::test]
    async fn cancel_by_job_id_reaches_active_transfer() {
        let queue = TransferQueue::new();
        let h = queue.submit_download(
            "cancel-drive".into(),
            PathBuf::from("slow.txt"),
            Some(1),
            |_path| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                Ok(vec![1])
            },
        );
        let mut progress = h.progress_rx.clone();
        loop {
            progress.changed().await.unwrap();
            if progress.borrow().state == TransferState::Active {
                break;
            }
        }
        assert!(queue.cancel(h.job_id));
        assert!(h.handle.await.unwrap().is_err());
        assert_eq!(queue.snapshot()[0].state, TransferState::Cancelled);
        assert!(!queue.cancel(u64::MAX));
    }

    #[tokio::test]
    async fn download_returns_bytes() {
        let queue = TransferQueue::new();
        let h = queue.submit_download(
            "test-drive".into(),
            PathBuf::from("doc.pdf"),
            Some(5),
            |_path| Ok(b"bytes".to_vec()),
        );

        let result = h.handle.await.unwrap().unwrap();
        assert_eq!(result, b"bytes");

        let progress = h.progress_rx.borrow().clone();
        assert_eq!(progress.state, TransferState::Done);
        assert_eq!(progress.direction, TransferDirection::Download);
    }

    #[tokio::test]
    async fn failed_transfer_reports_error() {
        let queue = TransferQueue::new();
        let h = queue.submit_upload(
            "test-drive".into(),
            PathBuf::from("fail.txt"),
            vec![1, 2, 3],
            |_path, _data| anyhow::bail!("permanent validation failure"),
        );

        let result = h.handle.await.unwrap();
        assert!(result.is_err());

        let progress = h.progress_rx.borrow().clone();
        match &progress.state {
            TransferState::Failed { error } => {
                assert!(
                    error.contains("permanent validation failure"),
                    "error: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transient_transfer_retries_and_reports_done() {
        let queue = TransferQueue::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let h = queue.submit_upload(
            "test-drive".into(),
            PathBuf::from("retry.txt"),
            vec![1, 2, 3],
            {
                let attempts = attempts.clone();
                move |_path, _data| {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        anyhow::bail!("transient network timeout")
                    }
                    Ok(())
                }
            },
        );

        h.handle.await.unwrap().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(h.progress_rx.borrow().state, TransferState::Done);
    }

    #[tokio::test]
    async fn cancellation_interrupts_retry_backoff() {
        let queue = TransferQueue::new();
        let h = queue.submit_upload(
            "test-drive".into(),
            PathBuf::from("cancel.txt"),
            vec![1],
            |_path, _data| anyhow::bail!("transient network timeout"),
        );
        let mut progress = h.progress_rx.clone();
        loop {
            progress.changed().await.unwrap();
            if matches!(progress.borrow().state, TransferState::Retrying { .. }) {
                break;
            }
        }
        h.cancellation.cancel();
        let error = h.handle.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(progress.borrow().state, TransferState::Cancelled);
    }

    #[tokio::test]
    async fn active_count_tracks_in_flight() {
        let queue = TransferQueue::with_concurrency(2);
        assert_eq!(queue.active_count(), 0);
        assert_eq!(queue.max_concurrent(), 2);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));

        let _h = queue.submit_upload(
            "d".into(),
            PathBuf::from("x"),
            vec![],
            move |_path, _data| {
                if let Some(tx) = started_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                // Block until released.
                let rt = tokio::runtime::Handle::current();
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rt.block_on(async { rx.await.ok() });
                }
                Ok(())
            },
        );

        // Wait for the transfer to start.
        started_rx.await.unwrap();
        assert_eq!(queue.active_count(), 1);

        // Release it.
        let _ = release_tx.send(());
    }

    #[tokio::test]
    async fn fourth_job_waits_when_limit_is_three() {
        let queue = TransferQueue::with_concurrency(3);

        // Use channels to control when each job finishes.
        let mut release_txs = Vec::new();
        let mut handles = Vec::new();

        for i in 0..4 {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            release_txs.push(release_tx);
            let release_rx = Arc::new(Mutex::new(Some(release_rx)));

            let h = queue.submit_upload(
                format!("d-{i}"),
                PathBuf::from(format!("f-{i}")),
                vec![],
                move |_path, _data| {
                    let rt = tokio::runtime::Handle::current();
                    if let Some(rx) = release_rx.lock().unwrap().take() {
                        rt.block_on(async { rx.await.ok() });
                    }
                    Ok(())
                },
            );
            handles.push(h);
        }

        // Give tasks time to acquire permits.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 3 should be active, 4th should be queued.
        assert_eq!(queue.active_count(), 3);

        // The 4th job should still be in Queued state.
        let fourth_state = handles[3].progress_rx.borrow().state.clone();
        assert_eq!(fourth_state, TransferState::Queued);

        // Release one → 4th should start.
        let _ = release_txs.remove(0).send(());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let fourth_state = handles[3].progress_rx.borrow().state.clone();
        assert!(
            matches!(fourth_state, TransferState::Active | TransferState::Done),
            "4th job should be active or done, got {fourth_state:?}"
        );

        // Release remaining.
        for tx in release_txs {
            let _ = tx.send(());
        }
        for h in handles {
            let _ = h.handle.await;
        }
    }

    #[test]
    fn transfer_state_serde_round_trips() {
        let states = vec![
            TransferState::Queued,
            TransferState::Active,
            TransferState::Retrying { attempt: 3 },
            TransferState::Done,
            TransferState::Failed {
                error: "boom".into(),
            },
            TransferState::Cancelled,
        ];
        for s in states {
            let json = serde_json::to_string(&s).unwrap();
            let back: TransferState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }
}
