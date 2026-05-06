//! Background ingest scheduler.
//!
//! Phase 7.4.2b of PLAN P7. Wraps the per-path ingest entry point
//! (`index_ingest_path` / equivalent direct call into `IngestPipeline`)
//! with a queue + worker so the user can enqueue an entire folder's
//! worth of paths and walk away while CrispSorter chews on it in the
//! background.
//!
//! Design notes:
//!
//! * **One worker, one queue.** No fan-out: the embedder is GIL-style
//!   (Mutex'd) and the LanceDB writer doesn't benefit from parallel
//!   inserters at our row-count scale. Adding workers later is a
//!   single line change once we measure that the embedder isn't the
//!   bottleneck.
//! * **State held in AppState** (`Mutex<BackgroundIngest>`). The
//!   worker grabs the state-lock briefly per iteration to pop a path,
//!   then releases for the long-running extract + embed + write.
//!   Status / counters are read-mostly from the frontend so the
//!   short-hold pattern keeps the UI responsive.
//! * **No restart-resume yet.** The queue is purely in-memory; an app
//!   restart loses pending paths. Persisting via tauri-plugin-store
//!   is a follow-up that 7.4.3's diff-based incremental updates make
//!   trivial (just re-walk the active catalogs on startup, the
//!   mtime-skip there will idempotently re-enqueue what's actually
//!   new).
//! * **Polite throttling.** A configurable `sleep_between_ms`
//!   (default 50ms) between ingests yields back to the runtime so
//!   foreground searches stay snappy. 7.4.4 will swap this for
//!   real QoS (pause during user-driven embedder calls).

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

/// RAII guard for marking a foreground operation as in-flight (PLAN
/// P7.4.4). Increment on construction, decrement on drop. Stored on
/// `AppState.foreground_active`; the bg_ingest worker checks the
/// counter at the top of each iteration and yields back to the
/// runtime if non-zero so foreground queries don't get stuck behind
/// a background embed.
///
/// Use as `let _g = ForegroundGuard::new(state.foreground_active.clone());`
/// at the entry of every foreground-blocking command (search,
/// reranker calls, etc.).
pub struct ForegroundGuard {
    counter: Arc<AtomicUsize>,
}

impl ForegroundGuard {
    pub fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One queued path + the metadata fields that future ingest will use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingIngest {
    pub path: PathBuf,
    pub owner_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
}

/// What the worker is doing right now. The frontend uses this to drive
/// pause / resume / cancel button states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BgStatus {
    Idle,
    Running,
    Paused,
    Stopping,
}

/// Snapshot returned by `status` — JSON-friendly counts the frontend
/// poll can render straight into a status badge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgStatusSnapshot {
    pub status: BgStatus,
    pub pending: usize,
    pub current: Option<String>,
    pub done: usize,
    pub errored: usize,
    pub last_error: Option<String>,
}

/// Mutable state held inside `AppState`. The worker takes the inner
/// lock briefly per iteration; tauri commands take it briefly per call.
pub struct BackgroundIngest {
    queue: VecDeque<PendingIngest>,
    status: BgStatus,
    current: Option<String>,
    done: usize,
    errored: usize,
    last_error: Option<String>,
    /// Some(handle) while a worker task is alive. The task observes
    /// `status == Stopping` to exit cleanly between iterations.
    worker: Option<tokio::task::JoinHandle<()>>,
    /// ms to sleep between ingest iterations — keeps the foreground
    /// runtime responsive. Fixed 50ms for now; 7.4.4 wires the real
    /// QoS that pauses on foreground embedder activity.
    pub sleep_between_ms: u64,
}

impl Default for BackgroundIngest {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            status: BgStatus::Idle,
            current: None,
            done: 0,
            errored: 0,
            last_error: None,
            worker: None,
            sleep_between_ms: 50,
        }
    }
}

impl BackgroundIngest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> BgStatusSnapshot {
        BgStatusSnapshot {
            status: self.status,
            pending: self.queue.len(),
            current: self.current.clone(),
            done: self.done,
            errored: self.errored,
            last_error: self.last_error.clone(),
        }
    }

    /// Append paths to the queue. Idempotent — duplicate paths land
    /// twice (the worker will let the underlying ingest dedup by
    /// source_hash). Caller should pre-dedup if duplicates would
    /// embarrass the user.
    pub fn enqueue(&mut self, items: Vec<PendingIngest>) {
        self.queue.extend(items);
    }

    pub fn pause(&mut self) {
        if matches!(self.status, BgStatus::Running) {
            self.status = BgStatus::Paused;
        }
    }

    pub fn resume(&mut self) {
        if matches!(self.status, BgStatus::Paused) {
            self.status = BgStatus::Running;
        }
    }

    /// Mark the worker for shutdown. The worker observes the flag at
    /// the top of each iteration and exits; the queue is preserved.
    pub fn cancel(&mut self) {
        if !matches!(self.status, BgStatus::Idle) {
            self.status = BgStatus::Stopping;
        }
    }

    /// Drop every pending entry and reset counters. Doesn't touch the
    /// worker — pair with `cancel` to fully stop.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.done = 0;
        self.errored = 0;
        self.current = None;
        self.last_error = None;
    }
}

/// Spawn the worker task if not already running. Safe to call repeatedly
/// — the second call is a no-op when a worker is alive.
///
/// Takes `Arc<Mutex<BackgroundIngest>>` so the worker holds its own
/// reference (the Tauri State<'_, …> isn't 'static).
pub fn ensure_worker(state: Arc<Mutex<BackgroundIngest>>, app: AppHandle) {
    let state_for_check = state.clone();
    tokio::spawn(async move {
        // Quick guard — if a worker's already alive, exit.
        {
            let mut g = state_for_check.lock().await;
            if g.worker.is_some() {
                return;
            }
            g.status = BgStatus::Running;
        }

        let worker_state = state_for_check.clone();
        let app_for_worker = app.clone();
        let h = tokio::spawn(worker_loop(worker_state, app_for_worker));
        state_for_check.lock().await.worker = Some(h);
    });
}

/// The worker loop — drains the queue, calls the per-path ingest, emits
/// progress events, sleeps between iterations.
async fn worker_loop(state: Arc<Mutex<BackgroundIngest>>, app: AppHandle) {
    use crate::AppState;
    use tauri::Manager;
    let foreground = app.state::<AppState>().foreground_active.clone();

    loop {
        // ── QoS: yield to foreground (P7.4.4) ───────────────────────────
        // If any foreground command is in-flight (search, reranker, …),
        // sleep 100ms and re-check. The check is a single atomic read so
        // we pay essentially nothing in the steady "no foreground"
        // state, and a typical search returns in well under 100ms so
        // we'll never block ingest for long.
        while foreground.load(Ordering::Relaxed) > 0 {
            // Honour Stopping even while waiting on foreground.
            if matches!(state.lock().await.status, BgStatus::Stopping) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // ── Take next item under the lock; release immediately. ─────────
        let next = {
            let mut g = state.lock().await;
            if matches!(g.status, BgStatus::Stopping) {
                g.status = BgStatus::Idle;
                g.current = None;
                g.worker = None;
                let _ = app.emit("bg-ingest:status", g.snapshot());
                return;
            }
            if matches!(g.status, BgStatus::Paused) {
                drop(g);
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            match g.queue.pop_front() {
                Some(item) => {
                    g.current = Some(item.path.to_string_lossy().into_owned());
                    let _ = app.emit("bg-ingest:status", g.snapshot());
                    item
                }
                None => {
                    g.status = BgStatus::Idle;
                    g.current = None;
                    g.worker = None;
                    let _ = app.emit("bg-ingest:status", g.snapshot());
                    return;
                }
            }
        };

        // ── Do the actual ingest off-lock. ──────────────────────────────
        let result = ingest_one(&next, &app).await;

        // ── Update counters under the lock. ─────────────────────────────
        let sleep_ms = {
            let mut g = state.lock().await;
            match result {
                Ok(()) => g.done += 1,
                Err(e) => {
                    g.errored += 1;
                    g.last_error = Some(e);
                }
            }
            g.current = None;
            let _ = app.emit("bg-ingest:status", g.snapshot());
            g.sleep_between_ms
        };
        if sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
        }
    }
}

/// Per-path ingest. Reads the file, runs the P7.4.1 extractor,
/// computes source_hash, builds a RawDocument, and feeds the existing
/// `IngestPipeline` from `AppState.index`. Returns Ok on success, or
/// Err(message) — both sides are reported via the status snapshot.
///
/// PLAN P7.4.3 — mtime-skip: stat the file first, look up the indexed
/// mtime via `LocalIndex::indexed_mtime_for_uri`, return Ok(()) without
/// doing any work if the index already has this file at the same or
/// newer mtime. The skip-on-success counts in `done` so the user sees
/// progress even when nothing's actually changing.
async fn ingest_one(item: &PendingIngest, app: &AppHandle) -> Result<(), String> {
    use crate::AppState;
    use sha2::{Digest, Sha256};
    use tauri::Manager;

    // Pull the pipeline + the local index out of AppState. We need
    // both: pipeline to do the actual ingest, local-index to do the
    // mtime-skip lookup.
    let app_state = app.state::<AppState>();
    let (pipeline, local) = {
        let g = app_state.index.lock().await;
        if !g.config.enabled {
            return Err("Index is disabled in settings".into());
        }
        let pipe = g
            .pipeline
            .clone()
            .ok_or_else(|| "No local ingest pipeline (remote backend?)".to_string())?;
        let local = g.local.clone();
        (pipe, local)
    };

    let p = item.path.clone();

    // ── mtime-skip (P7.4.3) ────────────────────────────────────────────
    // Stat the file *before* reading it. If the documents table already
    // has this location at the same / newer mtime, we're done — no
    // hash, no extract, no embed.
    let file_mtime: Option<i64> = std::fs::metadata(&p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    if let (Some(file_mt), Some(local)) = (file_mtime, local.as_ref()) {
        let owner = item
            .owner_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::nil().to_string());
        let probe_uri = crate::index::location::FileLocation::Local {
            user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()),
            machine_id: uuid::Uuid::nil(),
            path: p.clone(),
        }
        .to_uri();
        if let Ok(Some(indexed_mt)) = local.indexed_mtime_for_uri(&probe_uri).await {
            if indexed_mt >= file_mt {
                // Idempotent skip — caller treats Ok as a success and
                // bumps `done`. The frontend status badge will show
                // "N done" climbing without doing real work, which is
                // exactly what we want for a "rescan that found
                // nothing new" UX.
                return Ok(());
            }
        }
    }
    // File read off the runtime — pdf_extract / large reads can block.
    let bytes = tokio::task::spawn_blocking({
        let p = p.clone();
        move || std::fs::read(&p)
    })
    .await
    .map_err(|e| format!("read join: {e}"))?
    .map_err(|e| format!("reading {}: {e}", p.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let source_hash = hex::encode(h.finalize());

    let extracted = tokio::task::spawn_blocking({
        let p = p.clone();
        move || crate::extractors::extract_text_from_path(&p)
    })
    .await
    .map_err(|e| format!("extract join: {e}"))?
    .map_err(|e| format!("extracting {}: {e}", p.display()))?;

    let owner = item
        .owner_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::nil().to_string());
    let loc = crate::index::location::FileLocation::Local {
        user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()),
        machine_id: uuid::Uuid::nil(),
        path: p.clone(),
    };

    let raw = crate::index::ingest::RawDocument {
        full_text: extracted.full_text,
        full_text_md: String::new(),
        headings: extracted.headings,
        title: item.title.clone(),
        author: item.author.clone(),
        year: item.year,
        filename: p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ext: extracted.ext,
        language: item.language.clone().unwrap_or_default(),
        source_hash,
        location_uri: loc.to_uri(),
        owner_id: owner,
        tags: Vec::new(),
        // PLAN P7.4.3 — stat the file for mtime so re-ingest can skip
        // if the row is already present at the same mtime.
        mtime_unix: std::fs::metadata(&p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        // PLAN P7.6 — tag with the source volume's stable id so a
        // future search-time filter can hide rows from currently-
        // unmounted volumes. Best-effort; None when the helper fails.
        volume_id: crate::volume::volume_id_for_path(&p),
    };

    pipeline
        .ingest_document(raw)
        .await
        .map_err(|e| e.to_string())
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_appends_in_order() {
        let mut s = BackgroundIngest::new();
        let items = vec![
            PendingIngest {
                path: PathBuf::from("/a"),
                owner_id: None,
                title: None,
                author: None,
                year: None,
                language: None,
            },
            PendingIngest {
                path: PathBuf::from("/b"),
                owner_id: None,
                title: None,
                author: None,
                year: None,
                language: None,
            },
        ];
        s.enqueue(items);
        assert_eq!(s.snapshot().pending, 2);
        assert_eq!(s.snapshot().status, BgStatus::Idle);
    }

    #[test]
    fn pause_only_works_when_running() {
        let mut s = BackgroundIngest::new();
        s.pause();
        // Idle → pause is a no-op; status stays Idle.
        assert_eq!(s.snapshot().status, BgStatus::Idle);
        s.status = BgStatus::Running;
        s.pause();
        assert_eq!(s.snapshot().status, BgStatus::Paused);
    }

    #[test]
    fn foreground_guard_increments_and_drops() {
        let counter = Arc::new(AtomicUsize::new(0));
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        {
            let _g = ForegroundGuard::new(counter.clone());
            assert_eq!(counter.load(Ordering::Relaxed), 1);
            {
                let _g2 = ForegroundGuard::new(counter.clone());
                assert_eq!(counter.load(Ordering::Relaxed), 2);
            }
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn clear_resets_counters() {
        let mut s = BackgroundIngest::new();
        s.done = 5;
        s.errored = 2;
        s.last_error = Some("boom".into());
        s.queue.push_back(PendingIngest {
            path: PathBuf::from("/x"),
            owner_id: None,
            title: None,
            author: None,
            year: None,
            language: None,
        });
        s.clear();
        let snap = s.snapshot();
        assert_eq!(snap.done, 0);
        assert_eq!(snap.errored, 0);
        assert_eq!(snap.pending, 0);
        assert!(snap.last_error.is_none());
    }
}
