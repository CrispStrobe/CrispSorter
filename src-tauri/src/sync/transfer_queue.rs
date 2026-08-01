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
pub struct TransferHandle {
    pub job_id: u64,
    pub progress_rx: watch::Receiver<TransferProgress>,
    pub handle: JoinHandle<Result<Vec<u8>>>,
    pub cancellation: TransferCancellation,
}

/// Cancellation control for a queued or active transfer.
#[derive(Clone, Default)]
pub struct TransferCancellation {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<Notify>,
}

struct JobRecord {
    progress_rx: watch::Receiver<TransferProgress>,
    cancellation: TransferCancellation,
}

impl TransferCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Closure that performs the actual I/O.  Receives `(drive, remote_path,
/// local_data)`.  For downloads, `local_data` is empty and the closure
/// returns the downloaded bytes.  For uploads, `local_data` contains the
/// payload and the closure returns an empty vec on success.
type TransferFn = Arc<dyn Fn() -> Result<Vec<u8>> + Send + Sync + 'static>;

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
        let size = data.len() as u64;
        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);

        let initial = TransferProgress {
            job_id,
            direction: TransferDirection::Upload,
            drive_id: drive_id.clone(),
            remote_path: remote_path.to_string_lossy().into_owned(),
            bytes_done: 0,
            bytes_total: Some(size),
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
        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);

        let initial = TransferProgress {
            job_id,
            direction: TransferDirection::Download,
            drive_id: drive_id.clone(),
            remote_path: remote_path.to_string_lossy().into_owned(),
            bytes_done: 0,
            bytes_total: size_hint,
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
async fn run_with_retries(
    job_id: u64,
    direction: TransferDirection,
    drive_id: String,
    remote_path: PathBuf,
    bytes_total: Option<u64>,
    semaphore: Arc<Semaphore>,
    tx: watch::Sender<TransferProgress>,
    cancellation: TransferCancellation,
    transfer_fn: TransferFn,
) -> Result<Vec<u8>> {
    let remote_str = remote_path.to_string_lossy().into_owned();

    let make_progress = |state: TransferState, bytes_done: u64| TransferProgress {
        job_id,
        direction,
        drive_id: drive_id.clone(),
        remote_path: remote_str.clone(),
        bytes_done,
        bytes_total,
        state,
    };

    for attempt in 0..=MAX_RETRIES {
        if cancellation.is_cancelled() {
            let _ = tx.send(make_progress(TransferState::Cancelled, 0));
            return Err(anyhow!("transfer cancelled"));
        }
        // Acquire a fresh permit for every attempt. This prevents a sleeping
        // retry from occupying a transfer slot.
        let permit = tokio::select! {
            permit = semaphore.acquire() => permit.context("transfer queue semaphore closed")?,
            _ = cancellation.notify.notified() => {
                let _ = tx.send(make_progress(TransferState::Cancelled, 0));
                return Err(anyhow!("transfer cancelled"));
            }
        };
        let _ = tx.send(make_progress(TransferState::Active, 0));
        let transfer = transfer_fn.clone();
        let result = tokio::task::spawn_blocking(move || transfer())
            .await
            .context("transfer task panicked")?;
        drop(permit);

        if cancellation.is_cancelled() {
            let _ = tx.send(make_progress(TransferState::Cancelled, 0));
            return Err(anyhow!("transfer cancelled"));
        }

        match result {
            Ok(bytes) => {
                let done = bytes_total.unwrap_or(bytes.len() as u64);
                let _ = tx.send(make_progress(TransferState::Done, done));
                return Ok(bytes);
            }
            Err(error) if attempt < MAX_RETRIES && is_retryable(&error) => {
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
                return Err(error);
            }
        }
    }
    unreachable!("retry loop always returns")
}

fn is_retryable(error: &anyhow::Error) -> bool {
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
        let h = queue.submit_upload(
            "cancel-drive".into(),
            PathBuf::from("slow.txt"),
            vec![1],
            |_path, _data| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                Ok(())
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
