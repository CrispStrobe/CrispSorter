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
//! * **Durable source receipts.** Pending, active and failed paths are kept in
//!   SQLite before progress is acknowledged. Restart recovers unfinished work
//!   into a paused queue; explicit Resume retries failed paths as well. Duplicate
//!   requests for a source coalesce, with one follow-up retained if it is active.
//! * **Polite throttling.** A configurable `sleep_between_ms`
//!   (default 50ms) between ingests yields back to the runtime so
//!   foreground searches stay snappy. 7.4.4 will swap this for
//!   real QoS (pause during user-driven embedder calls).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

mod queue;

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
    pub active: usize,
    pub current: Option<String>,
    pub done: usize,
    pub errored: usize,
    pub last_error: Option<String>,
}

/// Max seconds the extractor may run before we classify the file as
/// `TaskFailureReason::Timeout` and fall through to the L2 path.
/// Five minutes covers slow PDFs; OCR-heavy scans may still time out
/// (that's intentional — they get a retryable Timeout badge).
const EXTRACTION_TIMEOUT_SECS: u64 = 300;

/// Mutable state held inside `AppState`. The worker takes the inner
/// lock briefly per iteration; tauri commands take it briefly per call.
pub struct BackgroundIngest {
    queue: VecDeque<PendingIngest>,
    failed: BTreeMap<String, PendingIngest>,
    in_flight: HashSet<String>,
    followup_keys: HashSet<String>,
    storage: Option<queue::QueueStore>,
    storage_dir: Option<PathBuf>,
    status: BgStatus,
    current: Option<String>,
    done: usize,
    errored: usize,
    last_error: Option<String>,
    /// How many worker tasks to run in parallel. Default 1 (safe for
    /// single-writer LanceDB). Raise to 2–4 when the embedder is the
    /// bottleneck (CPU/GPU-bound) rather than IO.
    pub concurrency: usize,
    /// Live count of running worker tasks. Workers self-decrement on exit;
    /// `ensure_worker` uses this to avoid spawning extras.
    active_workers: Arc<AtomicUsize>,
    /// ms to sleep between ingest iterations — keeps the foreground
    /// runtime responsive.
    pub sleep_between_ms: u64,
    /// Whether to attempt OCR on scanned images / empty-text PDFs.
    pub ocr_enabled: bool,
    /// Which OCR tier to use when `ocr_enabled` is true.
    /// "auto" | "tier1" | "tier2" | "tier3"
    pub ocr_tier: String,
    /// PaddleOCR recognition language model.
    /// "auto" | "latin" | "cjk"
    pub ocr_rec_lang: String,
    /// Smart OCR pipeline (C++ orchestrator) config. When
    /// `ocr_pipeline.enabled` is true, image OCR routes through the
    /// CrispEmbed pipeline (cleanup + NAFNet + routing + accept-gate)
    /// instead of the legacy tier ladder. Default disabled.
    pub ocr_pipeline: crate::extractors::OcrPipelineConfig,
}

impl Default for BackgroundIngest {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            failed: BTreeMap::new(),
            in_flight: HashSet::new(),
            followup_keys: HashSet::new(),
            storage: None,
            storage_dir: None,
            status: BgStatus::Idle,
            current: None,
            done: 0,
            errored: 0,
            last_error: None,
            concurrency: 1,
            active_workers: Arc::new(AtomicUsize::new(0)),
            sleep_between_ms: 50,
            ocr_enabled: false,
            ocr_tier: "auto".to_owned(),
            ocr_rec_lang: "auto".to_owned(),
            ocr_pipeline: crate::extractors::OcrPipelineConfig::default(),
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
            pending: self.queue.len() + self.followup_keys.len(),
            active: self.active_workers.load(Ordering::Relaxed),
            current: self.current.clone(),
            done: self.done,
            errored: self.errored,
            last_error: self.last_error.clone(),
        }
    }

    pub fn attach_storage(&mut self, data_dir: &std::path::Path) -> anyhow::Result<()> {
        if self.storage.is_some() {
            anyhow::ensure!(self.storage_dir.as_deref() == Some(data_dir), "Background queue is already attached to another data directory");
            self.recover_stopped_claims()?;
            return Ok(());
        }
        let mut store = queue::QueueStore::open(data_dir)?;
        let recovery = store.recover()?;
        // Validate the complete snapshot before publishing any recovered state.
        self.apply_recovery(recovery)?;
        self.storage = Some(store);
        self.storage_dir = Some(data_dir.to_owned());
        if let Err(error) = self.persist_settings() {
            self.storage = None;
            self.storage_dir = None;
            return Err(error);
        }
        Ok(())
    }

    fn apply_recovery(&mut self, recovery: queue::RecoveredQueue) -> anyhow::Result<()> {
        let mut pending = VecDeque::new();
        let mut failed = BTreeMap::new();
        for entry in recovery.entries {
            let item: PendingIngest = serde_json::from_str(&entry.payload)
                .map_err(|error| anyhow::anyhow!("Invalid saved background source {}: {error}", entry.key))?;
            anyhow::ensure!(source_key(&item)? == entry.key, "Saved background source identity does not match its receipt");
            match entry.status.as_str() {
                "pending" => pending.push_back(item),
                "error" => { failed.insert(entry.key, item); }
                _ => anyhow::bail!("Unexpected saved background source state"),
            }
        }
        let options = recovery.settings.as_deref().map(serde_json::from_str::<BackgroundOptions>).transpose()?;
        self.queue = pending;
        self.failed = failed;
        self.in_flight.clear();
        self.followup_keys.clear();
        self.done = recovery.done;
        self.errored = self.failed.len();
        self.last_error = recovery.last_error;
        self.current = None;
        self.status = if self.queue.is_empty() && self.failed.is_empty() { BgStatus::Idle } else { BgStatus::Paused };
        if let Some(options) = options {
            self.ocr_enabled = options.ocr_enabled;
            self.ocr_tier = options.ocr_tier;
            self.ocr_rec_lang = options.ocr_rec_lang;
            self.ocr_pipeline = options.ocr_pipeline;
        }
        Ok(())
    }

    fn store(&self) -> anyhow::Result<&queue::QueueStore> {
        self.storage.as_ref().ok_or_else(|| anyhow::anyhow!("Background queue storage is not initialized"))
    }

    fn store_mut(&mut self) -> anyhow::Result<&mut queue::QueueStore> {
        self.storage.as_mut().ok_or_else(|| anyhow::anyhow!("Background queue storage is not initialized"))
    }

    pub fn persist_settings(&self) -> anyhow::Result<()> {
        let options = BackgroundOptions {
            ocr_enabled: self.ocr_enabled, ocr_tier: self.ocr_tier.clone(),
            ocr_rec_lang: self.ocr_rec_lang.clone(), ocr_pipeline: self.ocr_pipeline.clone(),
        };
        let payload = serde_json::to_string(&options)?;
        // Reject settings which cannot be restored (for example non-finite
        // confidence values) before replacing the saved options.
        let _: BackgroundOptions = serde_json::from_str(&payload)?;
        self.store()?.save_settings(&payload)
    }

    /// Durable coalescing by owner and source path. Requests arriving while a
    /// source is active become one follow-up, never a concurrent extraction.
    pub fn enqueue(&mut self, items: Vec<PendingIngest>) -> anyhow::Result<usize> {
        self.recover_stopped_claims()?;
        let mut positions = HashMap::new();
        let mut unique = Vec::<(String, PendingIngest)>::new();
        for item in items {
            let key = source_key(&item)?;
            if let Some(&position) = positions.get(&key) { unique[position] = (key, item); }
            else { positions.insert(key.clone(), unique.len()); unique.push((key, item)); }
        }
        let payloads = unique.iter().map(|(key, item)| Ok((key.clone(), serde_json::to_string(item)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.store_mut()?.enqueue(&payloads)?;
        let mut queued_positions = self.queue.iter().enumerate().map(|(index, item)| Ok((source_key(item)?, index)))
            .collect::<anyhow::Result<HashMap<_, _>>>()?;
        let mut added = 0;
        for (key, item) in unique {
            if self.in_flight.contains(&key) {
                if self.followup_keys.insert(key) { added += 1; }
            } else if let Some(&position) = queued_positions.get(&key) {
                self.queue[position] = item;
            } else {
                self.failed.remove(&key);
                queued_positions.insert(key, self.queue.len());
                self.queue.push_back(item);
                added += 1;
            }
        }
        self.errored = self.failed.len();
        Ok(added)
    }

    fn recover_stopped_claims(&mut self) -> anyhow::Result<()> {
        if self.active_workers.load(Ordering::Relaxed) == 0 && !self.in_flight.is_empty() {
            let recovery = self.store_mut()?.recover()?;
            self.apply_recovery(recovery)?;
        }
        Ok(())
    }

    fn claim_next(&mut self) -> anyhow::Result<Option<PendingIngest>> {
        let position = self.queue.iter().position(|item| source_key(item).is_ok_and(|key| !self.in_flight.contains(&key)));
        let Some(position) = position else { return Ok(None); };
        let item = self.queue[position].clone();
        let key = source_key(&item)?;
        self.store()?.claim(&key)?;
        self.queue.remove(position);
        self.in_flight.insert(key);
        self.current = Some(item.path.to_string_lossy().into_owned());
        Ok(Some(item))
    }

    fn finish_item(&mut self, item: &PendingIngest, error: Option<&str>) -> anyhow::Result<()> {
        let key = source_key(item)?;
        let error = error.map(|value| value.chars().take(1024).collect::<String>());
        let followup = self.store_mut()?.complete(&key, error.as_deref())?;
        let followup = followup.map(|payload| serde_json::from_str::<PendingIngest>(&payload)).transpose()?;
        self.in_flight.remove(&key);
        self.followup_keys.remove(&key);
        if let Some(followup) = followup { self.queue.push_back(followup); }
        else if error.is_some() { self.failed.insert(key, item.clone()); }
        if let Some(error) = error { self.last_error = Some(error); }
        else { self.done += 1; }
        self.errored = self.failed.len();
        self.current = None;
        Ok(())
    }

    pub fn pause(&mut self) -> anyhow::Result<()> {
        if matches!(self.status, BgStatus::Running) {
            self.store()?.save_status("paused")?;
            self.status = BgStatus::Paused;
        }
        Ok(())
    }

    pub fn resume(&mut self) -> anyhow::Result<()> {
        self.recover_stopped_claims()?;
        if matches!(self.status, BgStatus::Paused | BgStatus::Idle)
            && (!self.queue.is_empty() || !self.failed.is_empty() || self.status == BgStatus::Paused) {
            self.store()?.retry_errors()?;
            self.queue.extend(std::mem::take(&mut self.failed).into_values());
            self.errored = 0;
            self.store()?.save_status("running")?;
            self.status = BgStatus::Running;
        }
        Ok(())
    }

    /// Stops claiming new work; pending paths and failed outcomes remain saved.
    pub fn cancel(&mut self) -> anyhow::Result<()> {
        if !matches!(self.status, BgStatus::Idle) || !self.queue.is_empty() {
            self.store()?.save_status("paused")?;
            self.status = BgStatus::Stopping;
        }
        Ok(())
    }

    /// Forget pending/failed requests only after every live worker has drained.
    pub fn clear(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(self.active_workers.load(Ordering::Relaxed) == 0,
            "Wait for background processing to stop before clearing the queue");
        self.store_mut()?.clear()?;
        self.queue.clear();
        self.failed.clear();
        self.in_flight.clear();
        self.followup_keys.clear();
        self.done = 0;
        self.errored = 0;
        self.current = None;
        self.last_error = None;
        self.status = BgStatus::Idle;
        Ok(())
    }

    fn start_workers(&mut self) -> anyhow::Result<(usize, Arc<AtomicUsize>)> {
        self.recover_stopped_claims()?;
        let active = self.active_workers.load(Ordering::Relaxed);
        if self.queue.is_empty() || self.status == BgStatus::Stopping
            || (self.status == BgStatus::Paused && active > 0) {
            return Ok((0, self.active_workers.clone()));
        }
        self.store()?.save_status("running")?;
        self.status = BgStatus::Running;
        let needed = self.concurrency.max(1).saturating_sub(active);
        // Reserve every worker under the state lock, so simultaneous callers
        // cannot both observe the same free worker slots.
        self.active_workers.fetch_add(needed, Ordering::Relaxed);
        Ok((needed, self.active_workers.clone()))
    }

}

#[derive(serde::Serialize, serde::Deserialize)]
struct BackgroundOptions {
    ocr_enabled: bool,
    ocr_tier: String,
    ocr_rec_lang: String,
    ocr_pipeline: crate::extractors::OcrPipelineConfig,
}

fn source_key(item: &PendingIngest) -> anyhow::Result<String> {
    // JSON separates the owner and path unambiguously; SHA-256 keeps large
    // source paths out of the primary-key index. No source file must exist.
    use sha2::{Digest, Sha256};
    let identity = serde_json::to_vec(&(item.owner_id.as_deref().unwrap_or("local"), &item.path))?;
    Ok(hex::encode(Sha256::digest(identity)))
}

struct WorkerLease {
    active: Arc<AtomicUsize>,
    released: bool,
}

impl WorkerLease {
    fn release(&mut self) -> usize {
        if self.released { return 0; }
        self.released = true;
        self.active.fetch_sub(1, Ordering::Relaxed)
    }
}

impl Drop for WorkerLease {
    fn drop(&mut self) { self.release(); }
}

/// Should a re-scan skip this file entirely?
///
/// True only when the source file is untouched *and* the stored row was written
/// under the current image-indexing setting. Recording the setting state is what
/// lets enabling image indexing backfill documents that predate the toggle, and
/// only once: the re-ingested row then carries the flag.
fn skip_unchanged(
    file_mtime: Option<i64>,
    source_hash: &str,
    indexed: Option<crate::index::local_index::IndexedState>,
    images_indexing_on: bool,
) -> bool {
    let Some(state) = indexed else {
        return false;
    };
    if state.source_hash.as_deref() != Some(source_hash) {
        return false;
    }
    if state.extraction_skip { return true; }
    if state.extraction_failed { return false; }
    let Some(file_mt) = file_mtime else { return false; };
    // Moving an older file over a newer one is still a change. Equality is
    // required; a timestamp in the future must never make a source immutable.
    match state.mtime_unix {
        Some(indexed_mt) if indexed_mt == file_mt => {}
        _ => return false,
    }
    // The setting moved since this row was written: re-ingest to backfill.
    if images_indexing_on && !state.images_indexed {
        return false;
    }
    true
}

/// Hash with bounded memory. Holding a second full copy of a large PDF or
/// recording for the entire extraction needlessly multiplies peak memory.
fn hash_source(path: &std::path::Path) -> std::io::Result<String> {
    crate::index::ingest::hash_local_source(path)
}

/// Spawn worker tasks up to the configured concurrency.  Safe to call
/// repeatedly — already-running workers are counted via `active_workers`
/// so no extras are spawned.
pub fn ensure_worker(state: Arc<Mutex<BackgroundIngest>>, app: AppHandle) {
    tokio::spawn(async move {
        let plan = tokio::task::spawn_blocking({
            let state = state.clone();
            move || state.blocking_lock().start_workers()
        }).await;
        let (needed, active) = match plan {
            Ok(Ok(plan)) => plan,
            error => {
                let error = match error { Ok(Err(error)) => format!("{error:#}"), Err(error) => error.to_string(), _ => unreachable!() };
                let mut guard = state.lock().await;
                guard.status = BgStatus::Paused;
                guard.last_error = Some(format!("Could not start the saved background queue: {error}"));
                let _ = app.emit("bg-ingest:status", guard.snapshot());
                return;
            }
        };
        for _ in 0..needed {
            tokio::spawn(worker_loop(state.clone(), app.clone(), active.clone()));
        }
    });
}

/// Extraction can run concurrently for different sources. Durable claims and
/// acknowledgements run on the blocking pool while the scheduler lock is held.
async fn worker_loop(
    state: Arc<Mutex<BackgroundIngest>>,
    app: AppHandle,
    active_workers: Arc<AtomicUsize>,
) {
    use crate::AppState;
    use tauri::Manager;
    let foreground = app.state::<AppState>().foreground_active.clone();
    let mut worker_lease = WorkerLease { active: active_workers, released: false };

    loop {
        while foreground.load(Ordering::Relaxed) > 0 {
            if matches!(state.lock().await.status, BgStatus::Stopping | BgStatus::Paused) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let claim = tokio::task::spawn_blocking({
            let state = state.clone();
            move || -> anyhow::Result<Option<(PendingIngest, u64, BgStatusSnapshot)>> {
                let mut guard = state.blocking_lock();
                if matches!(guard.status, BgStatus::Stopping | BgStatus::Paused) { return Ok(None); }
                Ok(guard.claim_next()?.map(|item| (item, guard.sleep_between_ms, guard.snapshot())))
            }
        }).await;
        let (next, sleep_ms, snapshot) = match claim {
            Ok(Ok(Some(claimed))) => claimed,
            Ok(Ok(None)) => break,
            error => {
                let error = match error { Ok(Err(error)) => format!("{error:#}"), Err(error) => error.to_string(), _ => unreachable!() };
                let mut guard = state.lock().await;
                guard.last_error = Some(format!("Could not save background claim: {error}"));
                guard.status = BgStatus::Stopping;
                break;
            }
        };
        let _ = app.emit("bg-ingest:status", snapshot);
        let result = ingest_one(&next, &app).await;
        let acknowledgement = tokio::task::spawn_blocking({
            let state = state.clone();
            let item = next.clone();
            let error = result.as_ref().err().cloned();
            move || -> anyhow::Result<BgStatusSnapshot> {
                let mut guard = state.blocking_lock();
                guard.finish_item(&item, error.as_deref())?;
                Ok(guard.snapshot())
            }
        }).await;
        match acknowledgement {
            Ok(Ok(snapshot)) => { let _ = app.emit("bg-ingest:status", snapshot); }
            error => {
                let error = match error { Ok(Err(error)) => format!("{error:#}"), Err(error) => error.to_string(), _ => unreachable!() };
                let mut guard = state.lock().await;
                guard.last_error = Some(format!("Background result could not be saved; the source remains recoverable: {error}"));
                guard.status = BgStatus::Stopping;
                break;
            }
        }
        if result.is_ok() {
            let data_dir = app.state::<AppState>().data_dir.lock().await.clone();
            if let Some(data_dir) = data_dir {
                let path = next.path.to_string_lossy().into_owned();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(audit) = crate::audit::AuditLog::open_or_create(&data_dir) {
                        let _ = audit.log("ingest", None, &path, "bg_ingest");
                    }
                }).await;
            }
        }
        if sleep_ms > 0 { tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await; }
    }
    let remaining = worker_lease.release();
    let mut guard = state.lock().await;
    if remaining == 1 {
        guard.status = if guard.queue.is_empty() && guard.failed.is_empty() && guard.in_flight.is_empty() { BgStatus::Idle } else { BgStatus::Paused };
        guard.current = None;
    }
    let _ = app.emit("bg-ingest:status", guard.snapshot());
}

/// Per-path ingest. Reads the file, runs the P7.4.1 extractor,
/// computes source_hash, builds a RawDocument, and feeds the existing
/// `IngestPipeline` from `AppState.index`. Returns Ok on success, or
/// Err(message) — both sides are reported via the status snapshot.
///
/// P7.4.3 — mtime-skip: stat first, skip if index is already up-to-date.
/// P10    — extraction timeout + DRM detection + L2 fallback on failure.
async fn ingest_one(item: &PendingIngest, app: &AppHandle) -> Result<(), String> {
    use crate::index::task_failure::{epub_is_drm_protected, TaskFailureReason};
    use crate::AppState;
    use std::time::Duration;
    use tauri::Manager;

    let app_state = app.state::<AppState>();
    // P13.7 Stage F — cloud-backup auto-push hook.  Stage C shipped
    // fire-and-forget tokio::spawn; this is the durable-retry
    // upgrade: every successful ingest enqueues a `cb_manifest_push`
    // outbox entry, drained by a background timer (see lib.rs
    // setup).  On crash / network outage the outbox preserves the
    // intent across restarts.
    //
    // The decision "should we enqueue" is read here once per
    // worker_loop spawn so a Settings change mid-run picks up at
    // the next worker_loop restart.
    let (cb_auto_push_enabled, cb_thin_client_mode, skeleton_only) = {
        let g = app_state.index.lock().await;
        let push = g.config.cloud_backup_push_manifests_enabled
            && g.config.cloud_backup_url.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        let thin = !g.config.local_extraction_enabled;
        (push, thin, g.config.local_skeleton_only)
    };
    let (pipeline, local, ocr_enabled, ocr_tier_str, ocr_rec_lang_str, translate_to, audio_extraction_enabled, ingest_audio_level, image_extraction_enabled, image_indexing_enabled, ingest_image_level, ocr_pipeline_cfg, anydoc_mode) = {
        let g = app_state.index.lock().await;
        if !g.config.enabled {
            return Err("Index is disabled in settings".into());
        }
        let pipe = g
            .pipeline
            .clone()
            .ok_or_else(|| "No local ingest pipeline (remote backend?)".to_string())?;
        let local = g.local.clone();
        // P13.5 follow-up — pick up the user's index-time translation
        // target.  When None, the extractor's MT pass is skipped
        // (existing behaviour, zero overhead).  When Some("en") etc.
        // every ExtractedDocument gets the translation hook applied
        // and the translated text lands in the LanceDB
        // text_translated column.
        let translate_to = g.config.translate_to.clone();
        // P13.6 Step 5 — audio extraction master switch.  Forwarded
        // into ExtractOptions so the dispatcher's audio arm can
        // short-circuit to "skipped by Settings" when false.  bg_ingest
        // task-failure ledger then downgrades the file to L1.
        let audio_extraction_enabled = g.config.audio_extraction_enabled;
        // P13.6 Step 7c — ingest level for audio.  Serialised to the
        // canonical kebab-case string the dispatcher matches on.
        let ingest_audio_level = match g.config.ingest_audio_level {
            crate::index::IngestAudioLevel::L1 => "l1".to_string(),
            crate::index::IngestAudioLevel::L2 => "l2".to_string(),
            crate::index::IngestAudioLevel::L3 => "l3".to_string(),
        };
        // P13.7 Step 1+3 — image master switch + ingest level.
        let image_extraction_enabled = g.config.image_extraction_enabled;
        let image_indexing_enabled = g.config.image_indexing_enabled;
        let ingest_image_level = match g.config.ingest_image_level {
            crate::index::IngestImageLevel::L1 => "l1".to_string(),
            crate::index::IngestImageLevel::L2 => "l2".to_string(),
            crate::index::IngestImageLevel::L3 => "l3".to_string(),
        };
        // How far the anydoc office/e-book converter reaches. `Auto`
        // (default) only rescues formats with no native extractor, so
        // this cannot change how an already-indexable file extracts.
        let anydoc_mode = g.config.anydoc_mode;
        drop(g);
        let bg = app_state.bg_ingest.lock().await;
        let ocr_enabled = bg.ocr_enabled;
        let ocr_tier_str = bg.ocr_tier.clone();
        let ocr_rec_lang_str = bg.ocr_rec_lang.clone();
        let ocr_pipeline_cfg = bg.ocr_pipeline.clone();
        (pipe, local, ocr_enabled, ocr_tier_str, ocr_rec_lang_str, translate_to, audio_extraction_enabled, ingest_audio_level, image_extraction_enabled, image_indexing_enabled, ingest_image_level, ocr_pipeline_cfg, anydoc_mode)
    };
    let ocr_tier = match ocr_tier_str.as_str() {
        "tier1" => crate::extractors::OcrTier::Tier1,
        "tier2" => crate::extractors::OcrTier::Tier2,
        "tier3" => crate::extractors::OcrTier::Tier3,
        _       => crate::extractors::OcrTier::Auto,
    };
    let ocr_rec_lang = match ocr_rec_lang_str.as_str() {
        "latin" => crate::extractors::OcrRecLang::Latin,
        "cjk"   => crate::extractors::OcrRecLang::Cjk,
        _       => crate::extractors::OcrRecLang::Auto,
    };

    // P13.5 follow-up — when index-time translation is on, auto-resolve
    // a text-LID model (CLD3, the smallest preset) so the extractor's
    // post-dispatch LID hook can populate `doc.language` BEFORE the
    // translation hook fires.  Without a source language, MT would
    // be silently skipped (the dispatcher requires both source and
    // target to be known).
    //
    // CrispASR's `cache_ensure_file` is content-addressed (filename
    // + URL), so per-call resolution is cheap after the first download.
    // Optimising this to once-per-process is queued but not urgent.
    //
    // The cfg!() gate avoids calling the feature-stubbed resolver in
    // builds without crispasr, where it would just return an error.
    let text_lid_model = if translate_to.is_some() && cfg!(feature = "crispasr") {
        let cache_dir = {
            let dd = app_state.data_dir.lock().await;
            dd.as_ref().map(|d| d.join("models"))
        };
        match cache_dir {
            Some(cache_dir) => {
                match crate::extractors::text_lid::resolve_lid_model(
                    crate::extractors::text_lid::LidPreset::Cld3,
                    &cache_dir,
                )
                .await
                {
                    Ok(p) => Some(p),
                    Err(e) => {
                        eprintln!(
                            "[bg_ingest] couldn't auto-resolve CLD3 LID model (translate_to \
                             is set but source-lang detection will be skipped): {e:#}"
                        );
                        None
                    }
                }
            }
            None => None,
        }
    } else {
        None
    };

    let p = item.path.clone();

    // ── mtime-skip + failure-skip (P7.4.3 / P10) ──────────────────────
    let bg_meta = std::fs::metadata(&p).ok();
    let file_mtime: Option<i64> = bg_meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    // Content verification catches same-size edits within one timestamp tick
    // and files copied over an existing path with preserved timestamps.
    let source_hash = tokio::task::spawn_blocking({
        let p = p.clone();
        move || hash_source(&p)
    }).await.map_err(|e| format!("source hash task: {e}"))?
        .map_err(|e| format!("reading {}: {e}", p.display()))?;
    if let Some(local) = local.as_ref() {
        let owner_probe = item
            .owner_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::nil().to_string());
        let probe_uri = crate::index::location::FileLocation::Local {
            user_id: uuid::Uuid::parse_str(&owner_probe)
                .unwrap_or_else(|_| uuid::Uuid::nil()),
            machine_id: uuid::Uuid::nil(),
            path: p.clone(),
        }
        .to_uri();
        // Up-to-date skip: the file is untouched *and* nothing in the settings
        // changed since the row was written. Image indexing is the only such
        // switch today — turning it on must backfill documents ingested while
        // it was off, exactly once (their row then records the flag).
        let indexed_state = local
            .indexed_state_for_owner_uri(&probe_uri, &owner_probe)
            .await
            .ok()
            .flatten();
        if skip_unchanged(file_mtime, &source_hash, indexed_state, image_indexing_enabled) {
            return Ok(());
        }
        // Do not permanently blacklist a location after one failed parse:
        // its bytes or extraction capabilities may have changed since then.
    }

    // ── Shared fields used by both success and L2-fallback paths ───────
    let owner = item
        .owner_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::nil().to_string());
    let loc = crate::index::location::FileLocation::Local {
        user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()),
        machine_id: uuid::Uuid::nil(),
        path: p.clone(),
    };
    let location_uri = loc.to_uri();
    let filename = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext_from_path = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mtime_unix = file_mtime; // reuse mtime already computed above
    let file_size = bg_meta.as_ref().map(|m| m.len() as i64);
    let volume_id = crate::volume::volume_id_for_path(&p);
    let parent_dir = p.parent().and_then(|d| d.to_str()).map(|s| s.to_owned());
    // Failed attempts and successful extraction share one deterministic
    // identity, so retries replace their metadata row rather than multiply it.
    let doc_id = crate::index::source_identity::occurrence_id(&owner, &location_uri, &source_hash);

    // ── Stage W — skeleton-only fast path ──────────────────────────────
    // When `local_skeleton_only = true` we skip everything except the two
    // lightweight KV tables in skeleton_index.db.  No LanceDB, no FTS,
    // no embedder.  Fires before the thin-client branch so both flags
    // being set still only writes the skeleton.
    if skeleton_only {
        if let Some(data_dir) = app_state.data_dir.lock().await.clone() {
            if let Ok(sk) = crate::index::skeleton::SkeletonIndex::open_or_create(&data_dir) {
                let l2 = crate::index::l2_metadata::read(&p);
                if let Some(author) = item.author.as_deref().or(l2.author.as_deref()) {
                    let _ = sk.upsert_author(author);
                }
                if let Some(dir) = parent_dir.as_deref() {
                    let _ = sk.upsert_parent_dir(dir);
                }
            }
        }
        return Ok(());
    }

    // ── Stage U — thin-client fast path ────────────────────────────────
    // When `local_extraction_enabled = false` we skip all extraction and
    // write an L1-only row.  If cloud-backup push is also enabled, we
    // additionally enqueue a `cb_file_upload` outbox entry so the VPS
    // can run extraction on the actual bytes.
    if cb_thin_client_mode {
        let l2 = crate::index::l2_metadata::read(&p);
        let raw_l1 = crate::index::ingest::RawDocument {
            full_text: String::new(),
            full_text_md: String::new(),
            headings: Vec::new(),
            title: item.title.clone().or(l2.title),
            author: item.author.clone().or(l2.author),
            year: item.year.or(l2.year),
            filename: filename.clone(),
            ext: ext_from_path.clone(),
            language: item.language.clone().or(l2.language).unwrap_or_default(),
            source_hash: source_hash.clone(),
            location_uri: location_uri.clone(),
            owner_id: owner.clone(),
            tags: Vec::new(),
            mtime_unix,
            // Thin-client mode writes a skeleton with no extraction at all, so
            // it can never gain image vectors. Recording `true` keeps the
            // re-ingest guard from chasing it forever once image indexing is on.
            images_indexed: true,
            file_size,
            volume_id: volume_id.clone(),
            parent_dir: parent_dir.clone(),
            translated_text: None,
            translated_to_lang: None,
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
        };
        // Metadata-only indexing must use the metadata writer. Passing an
        // empty body through full ingest could falsely report success while
        // storing no document row at all.
        pipeline.ingest_l1(&[crate::index::ingest::L1FileEntry {
            doc_id: doc_id.clone(), source_hash: source_hash.clone(),
            location_uri: location_uri.clone(), owner_id: owner.clone(),
            filename: filename.clone(), ext: ext_from_path.clone(),
            parent_dir: parent_dir.clone().unwrap_or_default(),
            size: file_size.unwrap_or(0),
            mtime_ms: mtime_unix.unwrap_or(0).saturating_mul(1000),
            ctime_ms: 0, volume_id: volume_id.clone(),
        }]).await.map_err(|e| format!("Saving metadata for {}: {e:#}", p.display()))?;

        // Enqueue manifest push + file upload for the VPS.
        if cb_auto_push_enabled {
            let mut row = crate::sync::cloud_backup::ManifestRow::from_raw_document(&raw_l1);
            if row.collection_id.is_none() {
                if let Some(data_dir) = app_state.data_dir.lock().await.clone() {
                    if let Ok(map) = crate::sync::partition::PartitionMap::open(&data_dir) {
                        let watch_roots: Vec<std::path::PathBuf> = {
                            #[cfg(feature = "desktop")]
                            { let w = app_state.watcher.lock().await; w.list().into_iter().map(std::path::PathBuf::from).collect() }
                            #[cfg(not(feature = "desktop"))]
                            { Vec::new() }
                        };
                        let file_path = std::path::PathBuf::from(
                            crate::images::tauri_commands::location_uri_to_local_path(&row.path)
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_else(|| row.path.clone())
                        );
                        for root in &watch_roots {
                            if file_path.starts_with(root) {
                                if let Some(cid) = map.lookup(root, &file_path) {
                                    row.collection_id = Some(cid);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            if let Some(data_dir) = app_state.data_dir.lock().await.clone() {
                if let Ok(payload) = serde_json::to_string(&row) {
                    if let Ok(mgr) = crate::sync::SyncManager::open(&data_dir) {
                        let _ = mgr.enqueue("cb_manifest_push", &payload);
                        // Stage U: also enqueue the raw bytes for VPS extraction.
                        let upload_payload = serde_json::json!({
                            "sha256": source_hash,
                            "path":   p.to_string_lossy(),
                        });
                        if let Ok(s) = serde_json::to_string(&upload_payload) {
                            let _ = mgr.enqueue("cb_file_upload", &s);
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    // ── Extract with timeout (P10) ──────────────────────────────────────
    let extract_opts = crate::extractors::ExtractOptions {
        try_ocr: ocr_enabled,
        ocr_pdf_min_chars: 50,
        ocr_tier,
        ocr_rec_lang,
        // P13.5 Phase 7 + follow-up: text-LID auto-fires when
        // IndexConfig.translate_to is set (translation needs to know
        // the source language).  Resolved to CLD3 above; None
        // otherwise so users not using translation pay no LID cost.
        // A standalone "tag every doc with language" setting (LID
        // without translation) is queued — exposes its own
        // IndexConfig field once there's UI for it.
        text_lid_model: text_lid_model.clone(),
        // P13.5 follow-up — index-time translation now reads its
        // target language from IndexConfig.translate_to.  When
        // unset, the extractor's MT pass is skipped (zero
        // overhead, existing behaviour).  When set, the extractor
        // runs MT after LID and stashes the result into
        // ExtractedDocument.translated_text, which bg_ingest then
        // passes through to RawDocument → LanceDB text_translated
        // column (added by the v100 migration).  Backend is
        // hard-coded to the m2m100 default for now; exposing
        // translate_backend / translate_model in IndexConfig is a
        // follow-up.
        translate_to,
        translate_backend: None,
        translate_model: None,
        audio_extraction_enabled,
        ingest_audio_level: ingest_audio_level.clone(),
        image_extraction_enabled,
        ingest_image_level: ingest_image_level.clone(),
        ocr_pipeline: ocr_pipeline_cfg.clone(),
        anydoc: anydoc_mode,
    };
    let extract_fut = tokio::task::spawn_blocking({
        let p = p.clone();
        move || crate::extractors::extract_text_from_path_with_opts(&p, extract_opts)
    });
    let extract_result = tokio::time::timeout(
        Duration::from_secs(EXTRACTION_TIMEOUT_SECS),
        extract_fut,
    )
    .await;

    match extract_result {
        // ── Success ─────────────────────────────────────────────────────
        Ok(Ok(Ok(extracted))) => {
            let mut raw = crate::index::ingest::RawDocument {
                full_text: extracted.full_text,
                full_text_md: String::new(),
                headings: extracted.headings,
                title: item.title.clone(),
                author: item.author.clone(),
                year: item.year,
                filename,
                ext: extracted.ext,
                // Priority: explicit catalog/item metadata first, then
                // the post-dispatch text-LID detection (Phase 7).  Empty
                // string is the existing column-default for "unknown"
                // — keeps Tantivy / LanceDB happy.
                language: item
                    .language
                    .clone()
                    .or_else(|| extracted.language.clone())
                    .unwrap_or_default(),
                source_hash,
                location_uri,
                owner_id: owner,
                // v107 — lift tags the extractor pulled out of YAML
                // frontmatter / EPUB dc:subject / etc.  Empty Vec
                // when the extractor didn't find any, matching the
                // pre-v107 default.
                tags: extracted.tags.clone(),
                mtime_unix,
                // Records whether this row went through ingest with image
                // indexing on, which is what lets enabling the toggle backfill
                // documents indexed while it was off.
                images_indexed: image_indexing_enabled,
                file_size,
                volume_id,
                parent_dir,
                // P13.5 Phase 8b — pass the extractor's MT output
                // through to the LanceDB row.  bg_ingest hard-codes
                // ExtractOptions.translate_to = None today, so these
                // are always None at this call site — the wire is
                // ready when IndexConfig grows a translate_to field
                // and bg_ingest reads it (follow-up).
                translated_text: extracted.translated_text,
                translated_to_lang: extracted.translated_to_lang,
                // P13.6 Step 7 — audio L2 metadata from the symphonia
                // probe inside extractors::audio::extract.  None for
                // non-audio extractors; the audio extractor's probe
                // result rides along in ExtractedDocument.audio.
                audio_duration_seconds: extracted.audio.as_ref().and_then(|a| a.duration_seconds),
                audio_codec: extracted.audio.as_ref().and_then(|a| a.codec.clone()),
                audio_sample_rate_hz: extracted.audio.as_ref().and_then(|a| a.sample_rate_hz.map(|s| s as i32)),
                audio_channels: extracted.audio.as_ref().and_then(|a| a.channels.map(|c| c as i32)),
                audio_bitrate_kbps: extracted.audio.as_ref().and_then(|a| a.bitrate_kbps.map(|b| b as i32)),
                // P13.6 Step 9 — image L2 (EXIF) curated subset.
                image_camera_make:  extracted.image_exif.as_ref().and_then(|e| e.camera_make.clone()),
                image_camera_model: extracted.image_exif.as_ref().and_then(|e| e.camera_model.clone()),
                image_lens_model:   extracted.image_exif.as_ref().and_then(|e| e.lens_model.clone()),
                image_taken_at_unix: extracted.image_exif.as_ref().and_then(|e| e.taken_at_unix),
                image_iso:          extracted.image_exif.as_ref().and_then(|e| e.iso.map(|i| i as i32)),
                multivec_packed: None,
                multivec_n_tokens: None,
                // v106 — lift the source URL the markdown extractor
                // pulled out of YAML frontmatter (or future PDF /
                // EPUB extractors).
                url: extracted.source_url.clone(),
                // P17.5 / P17.7 — cross-modal embeddings for image/audio files.
                // Computed synchronously below; None when crispembed
                // is not compiled in or the file type doesn't apply.
                embedding_omni: None, // populated below
                embedding_vit: None,  // populated below
            };

            // P26.1 — Document-type classification at ingest
            let doctype = crate::index::doctype::classify(
                &raw.ext,
                &raw.full_text,
                raw.title.as_deref(),
                None,
            );
            raw.tags.push(doctype.as_tag());
            // ── P17.5 / P17.7 — compute cross-modal embeddings ────────────
            // Run on a blocking thread since both CrispEmbed FFI calls are
            // CPU-bound (model inference). Soft-fail: log and continue with
            // None on error — the document is still fully searchable via
            // text embedding.
            {
                let ext_lower = raw.ext.to_ascii_lowercase();
                let is_image = matches!(
                    ext_lower.as_str(),
                    "jpg" | "jpeg" | "png" | "tiff" | "tif" | "bmp" | "webp"
                    | "heic" | "heif" | "avif" | "jp2" | "j2k" | "jxl"
                );
                let is_audio = matches!(
                    ext_lower.as_str(),
                    "wav" | "mp3" | "m4a" | "flac" | "ogg" | "opus" | "aac"
                    | "wma" | "aiff" | "aif" | "ape" | "wv"
                    | "mp4" | "mkv" | "avi" | "mov" | "webm" | "ts" | "mts"
                    | "m2ts" | "flv" | "wmv" | "3gp"
                );

                // ViT + Omni image embeddings — fire concurrently when
                // both are available (independent models, no shared state).
                let vit_available = is_image && crate::images::vit_embed::is_vit_available();
                let omni_available = crate::index::omni_embed::is_omni_available();

                if is_image && image_indexing_enabled && (vit_available || omni_available) {
                    let vit_handle = if vit_available {
                        let p2 = p.clone();
                        Some(tokio::task::spawn_blocking(move || {
                            crate::images::vit_embed::embed_image(&p2)
                        }))
                    } else {
                        None
                    };
                    let omni_handle = if omni_available {
                        let p2 = p.clone();
                        Some(tokio::task::spawn_blocking(move || {
                            crate::index::omni_embed::encode_image_omni(&p2)
                        }))
                    } else {
                        None
                    };
                    // Await both — they're already running on the blocking pool.
                    if let Some(h) = vit_handle {
                        match h.await {
                            Ok(Ok(emb)) => { raw.embedding_vit = Some(emb); }
                            Ok(Err(e)) => { eprintln!("[bg_ingest] vit embed failed for {}: {e:#}", p.display()); }
                            Err(e) => { eprintln!("[bg_ingest] vit embed join error: {e}"); }
                        }
                    }
                    if let Some(h) = omni_handle {
                        match h.await {
                            Ok(Ok(emb)) => { raw.embedding_omni = Some(emb); }
                            Ok(Err(e)) => { eprintln!("[bg_ingest] omni image embed failed for {}: {e:#}", p.display()); }
                            Err(e) => { eprintln!("[bg_ingest] omni image embed join error: {e}"); }
                        }
                    }
                }

                // Omni audio embedding (2048-D BidirLM-Omni)
                if is_audio && omni_available {
                    if let Some(ref pcm) = extracted.audio_pcm {
                        let pcm_clone = pcm.clone();
                        match tokio::task::spawn_blocking(move || {
                            crate::index::omni_embed::encode_audio_omni(&pcm_clone)
                        }).await {
                            Ok(Ok(emb)) => { raw.embedding_omni = Some(emb); }
                            Ok(Err(e)) => { eprintln!("[bg_ingest] omni audio embed failed for {}: {e:#}", p.display()); }
                            Err(e) => { eprintln!("[bg_ingest] omni audio embed join error: {e}"); }
                        }
                    }
                }
            }

            let verified_hash = tokio::task::spawn_blocking({
                let p = p.clone();
                move || hash_source(&p)
            }).await.map_err(|e| format!("source verification task: {e}"))?
                .map_err(|e| format!("verifying {}: {e}", p.display()))?;
            if verified_hash != raw.source_hash {
                return Err(format!("{} changed during extraction; the previous index is retained. Retry the file.", p.display()));
            }
            if raw.full_text.trim().is_empty()
                && raw.embedding_vit.is_none() && raw.embedding_omni.is_none() {
                let message = "No searchable text was extracted. Metadata is retained; enable OCR or adjust extraction settings and retry.";
                pipeline.ingest_l2_row(
                    doc_id, raw.location_uri, raw.owner_id, raw.filename,
                    raw.ext, raw.source_hash, raw.mtime_unix, raw.file_size,
                    raw.parent_dir, raw.volume_id, raw.title, raw.author, raw.year,
                    Some(raw.language), None, &TaskFailureReason::EmptyText, message,
                ).await.map_err(|e| format!("{message} Could not save the failure record: {e:#}"))?;
                return Err(format!("{}: {message}", p.display()));
            }

            // P13.7 Stage F + N — capture the wire-shape snapshot
            // BEFORE ingest_document consumes `raw`.  If the auto-
            // partition map (Stage N) has an entry for this file's
            // path, overwrite the snapshot's `collection_id` with
            // the map's choice so related files land on the same
            // VPS shard.  Manual `collection:<id>` tags still win
            // (`from_raw_document` reads them first).
            let cb_row = if cb_auto_push_enabled {
                let mut row = crate::sync::cloud_backup::ManifestRow::from_raw_document(&raw);
                // Only consult the map when no manual tag was set.
                if row.collection_id.is_none() {
                    if let Some(data_dir) = app_state.data_dir.lock().await.clone() {
                        if let Ok(map) = crate::sync::partition::PartitionMap::open(&data_dir) {
                            // The map is keyed by (root_path, file_path);
                            // we don't carry the root through bg_ingest,
                            // so probe each watched root as the prefix.
                            // Linear scan is fine: watch roots are O(1-10).
                            let watch_roots: Vec<std::path::PathBuf> = {
                                #[cfg(feature = "desktop")]
                                { let w = app_state.watcher.lock().await; w.list().into_iter().map(std::path::PathBuf::from).collect() }
                                #[cfg(not(feature = "desktop"))]
                                { Vec::new() }
                            };
                            let file_path = std::path::PathBuf::from(
                                crate::images::tauri_commands::location_uri_to_local_path(&row.path)
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| row.path.clone())
                            );
                            for root in &watch_roots {
                                if file_path.starts_with(root) {
                                    if let Some(cid) = map.lookup(root, &file_path) {
                                        row.collection_id = Some(cid);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Some(row)
            } else { None };
            let result = pipeline
                .ingest_verified_document(raw, p.clone())
                .await
                .map_err(|e| e.to_string());
            if result.is_ok() {
                if let Some(row) = cb_row {
                    if let Some(data_dir) = app_state.data_dir.lock().await.clone() {
                        // Outbox enqueue: serialise the row JSON and
                        // store under op="cb_manifest_push".  The
                        // background drain task (lib.rs setup) picks
                        // it up.  Errors here log only — local
                        // write already succeeded.
                        let payload = match serde_json::to_string(&row) {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!(
                                    "[bg_ingest] cb outbox serialise failed for {}: {e}",
                                    row.sha256
                                );
                                return result.map(|_| ());
                            }
                        };
                        match crate::sync::SyncManager::open(&data_dir) {
                            Ok(mgr) => {
                                if let Err(e) = mgr.enqueue("cb_manifest_push", &payload) {
                                    eprintln!(
                                        "[bg_ingest] cb outbox enqueue failed for {}: {e}",
                                        row.sha256
                                    );
                                }
                            }
                            Err(e) => eprintln!("[bg_ingest] SyncManager open failed: {e}"),
                        }
                    }
                }
            }
            result.map(|_| ())
        }

        // ── Extraction error (anyhow) ────────────────────────────────────
        Ok(Ok(Err(extract_err))) => {
            let err_msg = extract_err.to_string();
            let reason = if ext_from_path == "epub" && epub_is_drm_protected(&p) {
                TaskFailureReason::Drm
            } else {
                TaskFailureReason::classify(&err_msg)
            };
            let l2 = crate::index::l2_metadata::read(&p);
            pipeline
                .ingest_l2_row(
                    doc_id,
                    location_uri,
                    owner,
                    filename,
                    ext_from_path,
                    source_hash,
                    mtime_unix,
                    file_size,
                    parent_dir,
                    volume_id,
                    item.title.clone().or(l2.title),
                    item.author.clone().or(l2.author),
                    item.year.or(l2.year),
                    item.language.clone().or(l2.language),
                    l2.page_count,
                    &reason,
                    &err_msg,
                )
                .await.map_err(|e| format!("{err_msg}; could not save failure metadata: {e:#}"))?;
            Err(format!("{}: {err_msg}", p.display()))
        }

        // ── spawn_blocking panicked ──────────────────────────────────────
        Ok(Err(join_err)) => {
            let err_msg = join_err.to_string();
            let l2 = crate::index::l2_metadata::read(&p);
            pipeline
                .ingest_l2_row(
                    doc_id,
                    location_uri,
                    owner,
                    filename,
                    ext_from_path,
                    source_hash,
                    mtime_unix,
                    file_size,
                    parent_dir,
                    volume_id,
                    item.title.clone().or(l2.title),
                    item.author.clone().or(l2.author),
                    item.year.or(l2.year),
                    item.language.clone().or(l2.language),
                    l2.page_count,
                    &TaskFailureReason::Other,
                    &err_msg,
                )
                .await.map_err(|e| format!("{err_msg}; could not save failure metadata: {e:#}"))?;
            Err(format!("{}: {err_msg}", p.display()))
        }

        // ── Timeout ──────────────────────────────────────────────────────
        Err(_elapsed) => {
            let err_msg = format!(
                "extraction timed out after {}s",
                EXTRACTION_TIMEOUT_SECS
            );
            let l2 = crate::index::l2_metadata::read(&p);
            pipeline
                .ingest_l2_row(
                    doc_id,
                    location_uri,
                    owner,
                    filename,
                    ext_from_path,
                    source_hash,
                    mtime_unix,
                    file_size,
                    parent_dir,
                    volume_id,
                    item.title.clone().or(l2.title),
                    item.author.clone().or(l2.author),
                    item.year.or(l2.year),
                    item.language.clone().or(l2.language),
                    l2.page_count,
                    &TaskFailureReason::Timeout,
                    &err_msg,
                )
                .await.map_err(|e| format!("{err_msg}; could not save failure metadata: {e:#}"))?;
            // Timeout is retryable — count as errored so the user can
            // see which files didn't complete and retry them later.
            Err(err_msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_scheduler() -> (BackgroundIngest, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut scheduler = BackgroundIngest::new();
        scheduler.attach_storage(dir.path()).unwrap();
        (scheduler, dir)
    }

    fn source(path: &str) -> PendingIngest {
        PendingIngest { path: path.into(), owner_id: None, title: None, author: None, year: None, language: None }
    }

    #[test]
    fn enqueue_appends_in_order() {
        let (mut s, _dir) = stored_scheduler();
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
        s.enqueue(items).unwrap();
        assert_eq!(s.snapshot().pending, 2);
        assert_eq!(s.snapshot().status, BgStatus::Idle);
    }

    #[test]
    fn pause_only_works_when_running() {
        let (mut s, _dir) = stored_scheduler();
        s.pause().unwrap();
        // Idle → pause is a no-op; status stays Idle.
        assert_eq!(s.snapshot().status, BgStatus::Idle);
        s.status = BgStatus::Running;
        s.pause().unwrap();
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
        let (mut s, _dir) = stored_scheduler();
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
        s.clear().unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.done, 0);
        assert_eq!(snap.errored, 0);
        assert_eq!(snap.pending, 0);
        assert!(snap.last_error.is_none());
    }

    #[test]
    fn default_ocr_settings_are_off() {
        let s = BackgroundIngest::new();
        assert!(!s.ocr_enabled);
        assert_eq!(s.ocr_tier,     "auto");
        assert_eq!(s.ocr_rec_lang, "auto");
    }

    #[test]
    fn cancel_is_a_no_op_when_idle() {
        let (mut s, _dir) = stored_scheduler();
        s.cancel().unwrap();
        // Idle stays idle — cancel only takes effect on a running worker.
        assert_eq!(s.snapshot().status, BgStatus::Idle);
    }

    #[test]
    fn resume_only_works_when_paused() {
        let (mut s, _dir) = stored_scheduler();
        s.resume().unwrap();
        assert_eq!(s.snapshot().status, BgStatus::Idle);
        s.status = BgStatus::Paused;
        s.resume().unwrap();
        assert_eq!(s.snapshot().status, BgStatus::Running);
    }

    #[test]
    fn snapshot_is_consistent_with_internal_state() {
        let (mut s, _dir) = stored_scheduler();
        s.done = 7;
        s.errored = 3;
        s.current = Some("/path/to/active.pdf".to_owned());
        s.last_error = Some("oh no".to_owned());
        s.queue.push_back(PendingIngest {
            path: PathBuf::from("/q"),
            owner_id: None, title: None, author: None, year: None, language: None,
        });
        let snap = s.snapshot();
        assert_eq!(snap.done, 7);
        assert_eq!(snap.errored, 3);
        assert_eq!(snap.pending, 1);
        assert_eq!(snap.current.as_deref(), Some("/path/to/active.pdf"));
        assert_eq!(snap.last_error.as_deref(), Some("oh no"));
    }

    #[test]
    fn pending_ingest_serde_round_trip() {
        let item = PendingIngest {
            path: PathBuf::from("/data/foo bar.pdf"),
            owner_id: Some("u1".to_owned()),
            title: Some("Title".to_owned()),
            author: Some("Doe, John".to_owned()),
            year: Some(2024),
            language: Some("en".to_owned()),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: PendingIngest = serde_json::from_str(&json).unwrap();
        assert_eq!(item.path,     back.path);
        assert_eq!(item.owner_id, back.owner_id);
        assert_eq!(item.title,    back.title);
        assert_eq!(item.year,     back.year);
    }

    #[test]
    fn extraction_timeout_const_is_sensible() {
        // Sanity: 300s = 5min, the documented value. Catching a typo would
        // cause silent data loss (every file flagged as Timeout).
        assert!(EXTRACTION_TIMEOUT_SECS >= 60,  "timeout too short");
        assert!(EXTRACTION_TIMEOUT_SECS <= 3600,"timeout too long");
        assert_eq!(EXTRACTION_TIMEOUT_SECS, 300);
    }

    #[test]
    fn scheduler_reopens_pending_active_and_failed_sources_with_saved_options() {
        let (mut scheduler, dir) = stored_scheduler();
        scheduler.ocr_enabled = true;
        scheduler.ocr_tier = "tier2".into();
        scheduler.ocr_pipeline.layout = true;
        scheduler.persist_settings().unwrap();
        scheduler.enqueue(vec![source("/done"), source("/error"), source("/active"), source("/pending")]).unwrap();
        let done = scheduler.claim_next().unwrap().unwrap();
        scheduler.finish_item(&done, None).unwrap();
        let failed = scheduler.claim_next().unwrap().unwrap();
        scheduler.finish_item(&failed, Some("OCR model unavailable")).unwrap();
        let active = scheduler.claim_next().unwrap().unwrap();
        assert_eq!(active.path, PathBuf::from("/active"));
        drop(scheduler);
        let mut reopened = BackgroundIngest::new();
        reopened.attach_storage(dir.path()).unwrap();
        let state = reopened.snapshot();
        assert_eq!(state.status, BgStatus::Paused);
        assert_eq!((state.pending, state.done, state.errored), (2, 1, 1));
        assert!(reopened.ocr_enabled);
        assert_eq!(reopened.ocr_tier, "tier2");
        assert!(reopened.ocr_pipeline.layout);
        reopened.resume().unwrap();
        assert_eq!(reopened.snapshot().pending, 3);
        assert_eq!(reopened.snapshot().errored, 0);
        assert!(!reopened.queue.iter().any(|item| item.path == PathBuf::from("/done")));
    }

    #[test]
    fn scheduler_serializes_same_source_and_coalesces_followups() {
        let (mut scheduler, _dir) = stored_scheduler();
        let first = source("/same");
        assert_eq!(scheduler.enqueue(vec![first.clone(), first.clone()]).unwrap(), 1);
        let (workers, active) = scheduler.start_workers().unwrap();
        assert_eq!(workers, 1);
        assert_eq!(scheduler.start_workers().unwrap().0, 0);
        let current = scheduler.claim_next().unwrap().unwrap();
        let mut followup = first.clone(); followup.title = Some("updated title".into());
        assert_eq!(scheduler.enqueue(vec![followup.clone(), followup]).unwrap(), 1);
        assert!(scheduler.claim_next().unwrap().is_none());
        assert!(scheduler.clear().is_err());
        scheduler.finish_item(&current, None).unwrap();
        let next = scheduler.claim_next().unwrap().unwrap();
        assert_eq!(next.title.as_deref(), Some("updated title"));
        scheduler.finish_item(&next, None).unwrap();
        let lease = WorkerLease { active: active.clone(), released: false }; drop(lease);
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert_eq!(scheduler.snapshot().done, 2);
    }

    #[test]
    fn scheduler_rejects_unattached_storage_and_recovers_dropped_worker() {
        let mut scheduler = BackgroundIngest::new();
        assert!(scheduler.enqueue(vec![source("/not-saved")]).is_err());
        assert_eq!(scheduler.snapshot().pending, 0);
        let dir = tempfile::tempdir().unwrap();
        scheduler.attach_storage(dir.path()).unwrap();
        scheduler.enqueue(vec![source("/interrupted")]).unwrap();
        let (_, active) = scheduler.start_workers().unwrap();
        scheduler.claim_next().unwrap().unwrap();
        drop(WorkerLease { active, released: false });
        scheduler.attach_storage(dir.path()).unwrap();
        assert_eq!(scheduler.snapshot().status, BgStatus::Paused);
        assert_eq!(scheduler.snapshot().pending, 1);
        assert!(scheduler.in_flight.is_empty());
    }

    // ── Re-ingest guard (mtime + image-indexing backfill) ────────────────

    use crate::index::local_index::IndexedState;

    fn state(mtime: Option<i64>, images: bool) -> Option<IndexedState> {
        Some(IndexedState { mtime_unix: mtime, images_indexed: images, source_hash: Some("hash".into()), extraction_failed: false, extraction_skip: false })
    }

    #[test]
    fn skip_requires_the_file_to_be_untouched() {
        // Replacing a source with an older version is still a change.
        assert!(!skip_unchanged(Some(100), "hash", state(Some(120), true), false));
        // Same timestamp is still up to date (sub-second edits are ignored).
        assert!(skip_unchanged(Some(100), "hash", state(Some(100), true), false));
        // File changed after ingest → re-ingest.
        assert!(!skip_unchanged(Some(200), "hash", state(Some(120), true), false));
    }

    #[test]
    fn skip_never_happens_without_an_index_record() {
        assert!(!skip_unchanged(Some(100), "hash", None, false));
        assert!(!skip_unchanged(Some(100), "hash", None, true));
        // A row with no recorded mtime cannot prove it is current.
        assert!(!skip_unchanged(Some(100), "hash", state(None, true), false));
        // ...and no file mtime means there is nothing to compare.
        assert!(!skip_unchanged(None, "hash", state(Some(100), true), false));
    }

    #[test]
    fn enabling_image_indexing_backfills_old_rows_once() {
        // Row written while the feature was off, file untouched: re-ingest.
        assert!(!skip_unchanged(Some(100), "hash", state(Some(100), false), true));
        // Row written with the feature on: skip, so no reindex loop.
        assert!(skip_unchanged(Some(100), "hash", state(Some(100), true), true));
        // Feature off again: the old row is fine as-is.
        assert!(skip_unchanged(Some(100), "hash", state(Some(100), false), false));
    }

    #[test]
    fn content_changes_and_failed_attempts_are_never_skipped() {
        assert!(!skip_unchanged(Some(100), "different-hash", state(Some(100), true), false));
        let mut failed = state(Some(100), true).unwrap();
        failed.extraction_failed = true;
        assert!(!skip_unchanged(Some(100), "hash", Some(failed), false));
    }

    #[test]
    fn streaming_hash_detects_same_length_replacements() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"first").unwrap();
        let before = hash_source(file.path()).unwrap();
        std::fs::write(file.path(), b"other").unwrap();
        let after = hash_source(file.path()).unwrap();
        assert_ne!(before, after);
        assert_eq!(before.len(), 64);
    }
}
