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

/// Max seconds the extractor may run before we classify the file as
/// `TaskFailureReason::Timeout` and fall through to the L2 path.
/// Five minutes covers slow PDFs; OCR-heavy scans may still time out
/// (that's intentional — they get a retryable Timeout badge).
const EXTRACTION_TIMEOUT_SECS: u64 = 300;

/// Mutable state held inside `AppState`. The worker takes the inner
/// lock briefly per iteration; tauri commands take it briefly per call.
pub struct BackgroundIngest {
    queue: VecDeque<PendingIngest>,
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
            concurrency: 1,
            active_workers: Arc::new(AtomicUsize::new(0)),
            sleep_between_ms: 50,
            ocr_enabled: false,
            ocr_tier: "auto".to_owned(),
            ocr_rec_lang: "auto".to_owned(),
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

/// Spawn worker tasks up to the configured concurrency.  Safe to call
/// repeatedly — already-running workers are counted via `active_workers`
/// so no extras are spawned.
pub fn ensure_worker(state: Arc<Mutex<BackgroundIngest>>, app: AppHandle) {
    tokio::spawn(async move {
        let (target, active, already_running) = {
            let mut g = state.lock().await;
            let running = g.active_workers.load(Ordering::Relaxed);
            let target = g.concurrency.max(1);
            let need = target.saturating_sub(running);
            if need == 0 {
                return;
            }
            g.status = BgStatus::Running;
            (target, g.active_workers.clone(), running > 0)
        };
        let _ = (target, already_running); // suppress unused warnings
        let to_spawn = {
            let g = state.lock().await;
            g.concurrency.max(1).saturating_sub(active.load(Ordering::Relaxed))
        };
        for _ in 0..to_spawn {
            active.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(worker_loop(state.clone(), app.clone(), active.clone()));
        }
    });
}

/// Each worker task runs this loop. Multiple concurrent instances share the
/// `state` mutex — pops are serialised by the lock, processing runs in
/// parallel.  Each worker decrements `active_workers` on exit so
/// `ensure_worker` can respawn exactly the right number.
async fn worker_loop(
    state: Arc<Mutex<BackgroundIngest>>,
    app: AppHandle,
    active_workers: Arc<AtomicUsize>,
) {
    use crate::AppState;
    use tauri::Manager;
    let foreground = app.state::<AppState>().foreground_active.clone();

    loop {
        // ── QoS: yield to foreground (P7.4.4) ───────────────────────────
        while foreground.load(Ordering::Relaxed) > 0 {
            if matches!(state.lock().await.status, BgStatus::Stopping) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // ── Take next item under the lock; release immediately. ─────────
        let (next, sleep_ms) = {
            let mut g = state.lock().await;
            if matches!(g.status, BgStatus::Stopping) {
                let remaining = active_workers.fetch_sub(1, Ordering::Relaxed);
                if remaining == 1 {
                    // Last worker: clean up shared state.
                    g.status = BgStatus::Idle;
                    g.current = None;
                }
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
                    (item, g.sleep_between_ms)
                }
                None => {
                    // Queue empty — this worker exits.
                    let remaining = active_workers.fetch_sub(1, Ordering::Relaxed);
                    if remaining == 1 {
                        g.status = BgStatus::Idle;
                        g.current = None;
                    }
                    let _ = app.emit("bg-ingest:status", g.snapshot());
                    return;
                }
            }
        };

        // ── Do the actual ingest off-lock. ──────────────────────────────
        let result = ingest_one(&next, &app).await;

        // ── Update counters under the lock. ─────────────────────────────
        {
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
        }
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
/// P7.4.3 — mtime-skip: stat first, skip if index is already up-to-date.
/// P10    — extraction timeout + DRM detection + L2 fallback on failure.
async fn ingest_one(item: &PendingIngest, app: &AppHandle) -> Result<(), String> {
    use crate::index::task_failure::{epub_is_drm_protected, TaskFailureReason};
    use crate::AppState;
    use sha2::{Digest, Sha256};
    use std::time::Duration;
    use tauri::Manager;

    let app_state = app.state::<AppState>();
    let (pipeline, local, ocr_enabled, ocr_tier_str, ocr_rec_lang_str, translate_to) = {
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
        drop(g);
        let bg = app_state.bg_ingest.lock().await;
        let ocr_enabled = bg.ocr_enabled;
        let ocr_tier_str = bg.ocr_tier.clone();
        let ocr_rec_lang_str = bg.ocr_rec_lang.clone();
        (pipe, local, ocr_enabled, ocr_tier_str, ocr_rec_lang_str, translate_to)
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
    let file_mtime: Option<i64> = std::fs::metadata(&p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
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
        // mtime-skip: already up-to-date.
        if let (Some(file_mt), Ok(Some(indexed_mt))) =
            (file_mtime, local.indexed_mtime_for_uri(&probe_uri).await)
        {
            if indexed_mt >= file_mt {
                return Ok(());
            }
        }
        // Failure-skip: non-retryable reason already stored — don't waste
        // extraction time on DRM EPUBs, corrupt files, or unsupported types.
        if let Ok(Some(reason)) = local.extraction_failure_reason_for_uri(&probe_uri).await {
            match reason.as_str() {
                "drm" | "corrupt" | "unsupported" | "password" => return Ok(()),
                _ => {} // Timeout / Other — still worth retrying.
            }
        }
    }

    // ── Read bytes + hash ───────────────────────────────────────────────
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
    let bg_meta = std::fs::metadata(&p).ok();
    let mtime_unix = bg_meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let file_size = bg_meta.map(|m| m.len() as i64);
    let volume_id = crate::volume::volume_id_for_path(&p);
    let parent_dir = p.parent().and_then(|d| d.to_str()).map(|s| s.to_owned());
    let doc_id = uuid::Uuid::new_v4().to_string();

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
            let raw = crate::index::ingest::RawDocument {
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
                tags: Vec::new(),
                mtime_unix,
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
            };
            pipeline
                .ingest_document(raw)
                .await
                .map_err(|e| e.to_string())
                .map(|_| ())
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
            let _ = pipeline
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
                .await;
            Ok(())
        }

        // ── spawn_blocking panicked ──────────────────────────────────────
        Ok(Err(join_err)) => {
            let err_msg = join_err.to_string();
            let l2 = crate::index::l2_metadata::read(&p);
            let _ = pipeline
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
                .await;
            Ok(())
        }

        // ── Timeout ──────────────────────────────────────────────────────
        Err(_elapsed) => {
            let err_msg = format!(
                "extraction timed out after {}s",
                EXTRACTION_TIMEOUT_SECS
            );
            let l2 = crate::index::l2_metadata::read(&p);
            let _ = pipeline
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
                .await;
            // Timeout is retryable — count as errored so the user can
            // see which files didn't complete and retry them later.
            Err(err_msg)
        }
    }
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

    #[test]
    fn default_ocr_settings_are_off() {
        let s = BackgroundIngest::new();
        assert!(!s.ocr_enabled);
        assert_eq!(s.ocr_tier,     "auto");
        assert_eq!(s.ocr_rec_lang, "auto");
    }

    #[test]
    fn cancel_is_a_no_op_when_idle() {
        let mut s = BackgroundIngest::new();
        s.cancel();
        // Idle stays idle — cancel only takes effect on a running worker.
        assert_eq!(s.snapshot().status, BgStatus::Idle);
    }

    #[test]
    fn resume_only_works_when_paused() {
        let mut s = BackgroundIngest::new();
        s.resume();
        assert_eq!(s.snapshot().status, BgStatus::Idle);
        s.status = BgStatus::Paused;
        s.resume();
        assert_eq!(s.snapshot().status, BgStatus::Running);
    }

    #[test]
    fn snapshot_is_consistent_with_internal_state() {
        let mut s = BackgroundIngest::new();
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
}
