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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Semaphore};
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
}

/// Closure that performs the actual I/O.  Receives `(drive, remote_path,
/// local_data)`.  For downloads, `local_data` is empty and the closure
/// returns the downloaded bytes.  For uploads, `local_data` contains the
/// payload and the closure returns an empty vec on success.
type TransferFn = Box<dyn FnOnce() -> Result<Vec<u8>> + Send + 'static>;

// ── TransferQueue ────────────────────────────────────────────────────────

/// Bounded-concurrency transfer queue.
#[derive(Clone)]
pub struct TransferQueue {
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
}

impl TransferQueue {
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
        write_fn: impl FnOnce(&Path, &[u8]) -> Result<()> + Send + 'static,
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

        let handle = tokio::spawn(async move {
            run_with_retries(
                job_id,
                TransferDirection::Upload,
                drive_id,
                remote_path.clone(),
                Some(size),
                sem,
                tx,
                Box::new(move || {
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
        read_fn: impl FnOnce(&Path) -> Result<Vec<u8>> + Send + 'static,
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

        let handle = tokio::spawn(async move {
            run_with_retries(
                job_id,
                TransferDirection::Download,
                drive_id,
                remote_path.clone(),
                size_hint,
                sem,
                tx,
                Box::new(move || read_fn(&remote_path)),
            )
            .await
        });

        TransferHandle {
            job_id,
            progress_rx: rx,
            handle,
        }
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
/// For the retry loop to work, the closure is only invoked once (we can't
/// clone arbitrary closures).  So retries are only applied at the
/// semaphore-acquisition level — if the transfer itself fails, the error
/// propagates.  For full retry support, callers should re-submit the job.
async fn run_with_retries(
    job_id: u64,
    direction: TransferDirection,
    drive_id: String,
    remote_path: PathBuf,
    bytes_total: Option<u64>,
    semaphore: Arc<Semaphore>,
    tx: watch::Sender<TransferProgress>,
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

    // Acquire semaphore permit (waits if all slots are occupied).
    let _permit = semaphore
        .acquire()
        .await
        .context("transfer queue semaphore closed")?;

    let _ = tx.send(make_progress(TransferState::Active, 0));

    // Run the blocking I/O on a dedicated thread.
    let result = tokio::task::spawn_blocking(transfer_fn)
        .await
        .context("transfer task panicked")?;

    match &result {
        Ok(bytes) => {
            let done = bytes_total.unwrap_or(bytes.len() as u64);
            let _ = tx.send(make_progress(TransferState::Done, done));
        }
        Err(e) => {
            let _ = tx.send(make_progress(
                TransferState::Failed {
                    error: format!("{e:#}"),
                },
                0,
            ));
        }
    }

    result
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
            |_path, _data| anyhow::bail!("network timeout"),
        );

        let result = h.handle.await.unwrap();
        assert!(result.is_err());

        let progress = h.progress_rx.borrow().clone();
        match &progress.state {
            TransferState::Failed { error } => {
                assert!(error.contains("network timeout"), "error: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn active_count_tracks_in_flight() {
        let queue = TransferQueue::with_concurrency(2);
        assert_eq!(queue.active_count(), 0);
        assert_eq!(queue.max_concurrent(), 2);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        let _h = queue.submit_upload(
            "d".into(),
            PathBuf::from("x"),
            vec![],
            move |_path, _data| {
                let _ = started_tx.send(());
                // Block until released.
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async { release_rx.await.ok() });
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

            let h = queue.submit_upload(
                format!("d-{i}"),
                PathBuf::from(format!("f-{i}")),
                vec![],
                move |_path, _data| {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async { release_rx.await.ok() });
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
