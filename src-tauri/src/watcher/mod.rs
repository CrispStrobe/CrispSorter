//! Folder watcher — cross-platform fs-event bridge backing the
//! auto-import feature in the Settings UI.
//!
//! `notify` handles the platform-specific event source (FSEvents on
//! macOS, inotify on Linux, `ReadDirectoryChangesW` on Windows). For
//! each create/rename event whose target is a regular file with a
//! known extension, we emit a Tauri `folder-watch:added` event with
//! the absolute path. The frontend listens and pushes the path into
//! `batchManager`.
//!
//! ## Auto-process modes (P5)
//!
//! Each watched folder has a `WatchMode`:
//! - **Off** — detect and emit events only (legacy behaviour).
//! - **Analyse** — auto-queue for extraction + indexing (no file moves).
//! - **Sort** — auto-queue for full batch pipeline (extraction → LLM →
//!   sort-path → move/copy).
//!
//! When mode ≠ Off, detected files are placed into a debounced
//! auto-dispatch queue. After a 5-second batch window, the queue is
//! flushed and emitted as a `folder-watch:auto-process` event with
//! the list of paths + mode. The frontend (or a backend handler)
//! picks them up for processing.
//!
//! Safety caps prevent runaway costs:
//! - **Hourly file cap** (default 100): at most N files per folder per
//!   rolling hour.
//! - **Daily cost cap** (default 500 files globally): hard stop across
//!   all folders for one calendar day.

use anyhow::{Context, Result};
use notify::{
    event::{CreateKind, EventKind, ModifyKind, RenameMode},
    RecommendedWatcher, RecursiveMode, Watcher as _,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

const DEDUP_WINDOW: Duration = Duration::from_secs(2);

/// Auto-dispatch batch window — files detected within this window are
/// grouped into a single batch before processing.
const AUTO_DISPATCH_DELAY: Duration = Duration::from_secs(5);

/// Extensions worth importing — same set the rest of the app
/// recognises. Lowercased; the matcher folds before comparing.
const ALLOWED_EXTS: &[&str] = &[
    "pdf", "epub", "djvu", "txt", "md", "rtf", "doc", "docx", "odt",
    // Image formats — included when auto-process can OCR/embed them.
    "jpg", "jpeg", "png", "tiff", "tif", "webp", "heic", "bmp",
    // Audio/video — included for transcription + omni embedding.
    "mp3", "mp4", "m4a", "wav", "flac", "ogg", "opus", "webm", "mkv",
    "avi", "mov",
];

// ── Types ─────────────────────────────────────────────────────────────────────

/// Per-folder watch mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WatchMode {
    /// Detect only — emit events, no auto-processing.
    #[default]
    Off,
    /// Auto-queue for extraction + indexing (no file moves).
    Analyse,
    /// Auto-queue for full batch pipeline (extract → LLM → sort → move).
    Sort,
}

/// Tauri event payload — kept stable so the frontend listener can
/// stay simple. The field name `path` matches what the rest of the
/// codebase uses for absolute file paths.
#[derive(Debug, Clone, Serialize)]
pub struct AddedEvent {
    pub path: String,
}

/// Payload for the auto-process batch event.
#[derive(Debug, Clone, Serialize)]
pub struct AutoProcessEvent {
    pub paths: Vec<String>,
    pub mode: WatchMode,
    pub folder: String,
}

/// Status event emitted after queue changes — frontend renders as
/// a status chip or tray badge.
#[derive(Debug, Clone, Serialize)]
pub struct WatchStatusEvent {
    pub daily_processed: u32,
    pub daily_cap: u32,
    pub daily_tokens_used: u64,
    pub daily_token_budget: u64,
    pub pending_files: usize,
    pub dead_letter_count: usize,
}

/// Per-folder watcher entry.
struct WatchEntry {
    _watcher: RecommendedWatcher,
    mode: WatchMode,
}

/// A file that failed during auto-processing.
#[derive(Debug, Clone, Serialize)]
pub struct DeadLetter {
    pub path: String,
    pub folder: String,
    pub error: String,
    pub timestamp: u64,
}

/// Rate-limiting counters — dual: file-count + token-cost.
struct RateLimits {
    /// Per-folder: (folder_path, hour_start) → count.
    hourly: HashMap<PathBuf, (Instant, u32)>,
    /// Global daily file count (day_start, count).
    daily: (Instant, u32),
    /// Global daily token cost (day_start, tokens_used).
    /// Estimated at ~750 tokens/page for text, ~1500 for OCR'd images.
    daily_tokens: (Instant, u64),
}

impl RateLimits {
    fn new() -> Self {
        Self {
            hourly: HashMap::new(),
            daily: (Instant::now(), 0),
            daily_tokens: (Instant::now(), 0),
        }
    }

    /// Check if a file from `folder` is within caps. If yes, increment
    /// file count and return true. If no, return false.
    /// Token budget is a soft cap checked via `is_token_budget_exceeded`.
    fn try_increment(
        &mut self,
        folder: &Path,
        hourly_cap: u32,
        daily_cap: u32,
        daily_token_budget: u64,
    ) -> bool {
        let now = Instant::now();

        // Daily cap — 24h rolling window (file count).
        if now.duration_since(self.daily.0) > Duration::from_secs(86400) {
            self.daily = (now, 0);
            self.daily_tokens = (now, 0);
        }
        if self.daily.1 >= daily_cap {
            return false;
        }
        // Token budget check (soft — based on already-consumed tokens).
        if daily_token_budget > 0 && self.daily_tokens.1 >= daily_token_budget {
            return false;
        }

        // Hourly per-folder cap.
        let entry = self.hourly.entry(folder.to_path_buf()).or_insert((now, 0));
        if now.duration_since(entry.0) > Duration::from_secs(3600) {
            *entry = (now, 0);
        }
        if entry.1 >= hourly_cap {
            return false;
        }

        entry.1 += 1;
        self.daily.1 += 1;
        true
    }

    /// Record actual tokens consumed after processing a file.
    fn record_tokens(&mut self, tokens: u64) {
        let now = Instant::now();
        if now.duration_since(self.daily_tokens.0) > Duration::from_secs(86400) {
            self.daily_tokens = (now, 0);
        }
        self.daily_tokens.1 += tokens;
    }

    /// Estimate tokens for a file based on size (heuristic: ~1 token per 4 bytes).
    fn estimate_tokens_for_file(path: &Path) -> u64 {
        std::fs::metadata(path)
            .map(|m| m.len() / 4)
            .unwrap_or(1000)
    }
}

/// State held in `AppState`. Holds one `RecommendedWatcher` per watched
/// folder, keyed by the canonical path.
pub struct WatcherState {
    watchers: HashMap<PathBuf, WatchEntry>,
    /// Per-path debounce map (last-emit instant).
    last_emit: Arc<Mutex<HashMap<PathBuf, Instant>>>,
    /// Auto-dispatch queue: pending files grouped by folder.
    auto_queue: Arc<Mutex<HashMap<PathBuf, Vec<PathBuf>>>>,
    /// Rate limiter.
    rate_limits: Arc<Mutex<RateLimits>>,
    /// Dead-letter list: files that failed auto-processing.
    dead_letters: Arc<Mutex<Vec<DeadLetter>>>,
    /// Hourly file cap per folder (configurable).
    pub hourly_cap: u32,
    /// Daily file cap globally (configurable).
    pub daily_cap: u32,
    /// Daily token budget (0 = unlimited). Estimated at ~1 token/4 bytes.
    pub daily_token_budget: u64,
}

impl WatcherState {
    pub fn new() -> Self {
        Self {
            watchers: HashMap::new(),
            last_emit: Arc::new(Mutex::new(HashMap::new())),
            auto_queue: Arc::new(Mutex::new(HashMap::new())),
            rate_limits: Arc::new(Mutex::new(RateLimits::new())),
            dead_letters: Arc::new(Mutex::new(Vec::new())),
            hourly_cap: 100,
            daily_cap: 500,
            daily_token_budget: 2_000_000, // ~2M tokens/day default
        }
    }

    /// Currently watched folders, sorted for deterministic UI ordering.
    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .watchers
            .keys()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// Get mode for a specific folder. Returns None if not watched.
    pub fn get_mode(&self, folder: &Path) -> Option<WatchMode> {
        self.watchers.get(folder).map(|e| e.mode)
    }

    /// Set mode for a watched folder. Returns Err if folder is not watched.
    pub fn set_mode(&mut self, folder: &Path, mode: WatchMode) -> Result<()> {
        let canonical = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
        let entry = self
            .watchers
            .get_mut(&canonical)
            .ok_or_else(|| anyhow::anyhow!("folder not in watch set: {}", canonical.display()))?;
        entry.mode = mode;
        Ok(())
    }

    /// List folders with their modes.
    pub fn list_with_modes(&self) -> Vec<(String, WatchMode)> {
        let mut v: Vec<(String, WatchMode)> = self
            .watchers
            .iter()
            .map(|(p, e)| (p.to_string_lossy().into_owned(), e.mode))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Get queue status — pending files per folder + rate limit state.
    pub async fn queue_status(&self) -> QueueStatus {
        let queue = self.auto_queue.lock().await;
        let limits = self.rate_limits.lock().await;
        let dead = self.dead_letters.lock().await;
        let pending: HashMap<String, usize> = queue
            .iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.len()))
            .collect();
        QueueStatus {
            pending_by_folder: pending,
            daily_processed: limits.daily.1,
            daily_cap: self.daily_cap,
            daily_tokens_used: limits.daily_tokens.1,
            daily_token_budget: self.daily_token_budget,
            dead_letter_count: dead.len(),
        }
    }

    /// Record tokens consumed after processing a file.
    pub async fn record_tokens_used(&self, tokens: u64) {
        let mut limits = self.rate_limits.lock().await;
        limits.record_tokens(tokens);
    }

    /// Report a file as failed (dead letter).
    pub async fn report_failure(&self, path: &str, folder: &str, error: &str) {
        let mut dead = self.dead_letters.lock().await;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        dead.push(DeadLetter {
            path: path.to_owned(),
            folder: folder.to_owned(),
            error: error.to_owned(),
            timestamp: ts,
        });
        // Keep at most 500 entries.
        if dead.len() > 500 {
            let excess = dead.len() - 500;
            dead.drain(0..excess);
        }
    }

    /// Get dead letters (most recent first).
    pub async fn dead_letters(&self) -> Vec<DeadLetter> {
        let dead = self.dead_letters.lock().await;
        let mut v = dead.clone();
        v.reverse();
        v
    }

    /// Dismiss a dead letter by path.
    pub async fn dismiss_dead_letter(&self, path: &str) {
        let mut dead = self.dead_letters.lock().await;
        dead.retain(|d| d.path != path);
    }

    /// Retry a dead letter — removes from dead list and returns the path
    /// for re-queuing.
    pub async fn retry_dead_letter(&self, path: &str) -> Option<DeadLetter> {
        let mut dead = self.dead_letters.lock().await;
        if let Some(pos) = dead.iter().position(|d| d.path == path) {
            Some(dead.remove(pos))
        } else {
            None
        }
    }
}

/// Queue status returned by `watch_queue_status`.
#[derive(Debug, Clone, Serialize)]
pub struct QueueStatus {
    pub pending_by_folder: HashMap<String, usize>,
    pub daily_processed: u32,
    pub daily_cap: u32,
    pub daily_tokens_used: u64,
    pub daily_token_budget: u64,
    pub dead_letter_count: usize,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self::new()
    }
}

/// Start watching `folder` recursively. Idempotent: if the canonical
/// path is already in the watch set, returns `Ok(())` without spawning
/// a duplicate watcher.
pub fn start(
    state: &mut WatcherState,
    app: AppHandle,
    folder: PathBuf,
    mode: WatchMode,
    initial_scan: bool,
) -> Result<()> {
    let folder = folder
        .canonicalize()
        .with_context(|| format!("watch path does not resolve: {}", folder.display()))?;
    if !folder.is_dir() {
        anyhow::bail!("watch path is not a directory: {}", folder.display());
    }
    if state.watchers.contains_key(&folder) {
        // Update mode if re-registering with a different mode.
        if let Some(entry) = state.watchers.get_mut(&folder) {
            entry.mode = mode;
        }
        return Ok(());
    }

    let dedup = state.last_emit.clone();
    let auto_queue = state.auto_queue.clone();
    let rate_limits = state.rate_limits.clone();
    let hourly_cap = state.hourly_cap;
    let daily_cap = state.daily_cap;
    let daily_token_budget = state.daily_token_budget;
    let app_for_handler = app.clone();
    let folder_for_handler = folder.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) => handle_event(
                event,
                &app_for_handler,
                &dedup,
                &auto_queue,
                &rate_limits,
                &folder_for_handler,
                mode,
                hourly_cap,
                daily_cap,
                daily_token_budget,
            ),
            Err(e) => eprintln!("[watch] notify error: {e}"),
        }
    })
    .context("create fs watcher")?;

    watcher
        .watch(&folder, RecursiveMode::Recursive)
        .with_context(|| format!("watch start failed for {}", folder.display()))?;

    state.watchers.insert(folder.clone(), WatchEntry { _watcher: watcher, mode });

    // Optional initial scan.
    if initial_scan && mode != WatchMode::Off {
        let app_scan = app.clone();
        let folder_scan = folder.clone();
        let queue_scan = state.auto_queue.clone();
        let limits_scan = state.rate_limits.clone();
        tokio::spawn(async move {
            if let Err(e) = run_initial_scan(
                &app_scan,
                &folder_scan,
                mode,
                &queue_scan,
                &limits_scan,
                hourly_cap,
                daily_cap,
                daily_token_budget,
            )
            .await
            {
                eprintln!("[watch] initial scan failed for {}: {e:#}", folder_scan.display());
            }
        });
    }

    Ok(())
}

/// Stop watching a single folder.
pub fn stop_one(state: &mut WatcherState, folder: &Path) -> bool {
    let canonical = match folder.canonicalize() {
        Ok(p) => p,
        Err(_) => folder.to_path_buf(),
    };
    state.watchers.remove(&canonical).is_some()
        || state.watchers.remove(folder).is_some()
}

/// Stop all active watchers.
pub fn stop_all(state: &mut WatcherState) {
    state.watchers.clear();
}

// ── Initial scan ─────────────────────���────────────────────────────────────────

async fn run_initial_scan(
    app: &AppHandle,
    folder: &Path,
    mode: WatchMode,
    queue: &Arc<Mutex<HashMap<PathBuf, Vec<PathBuf>>>>,
    limits: &Arc<Mutex<RateLimits>>,
    hourly_cap: u32,
    daily_cap: u32,
    daily_token_budget: u64,
) -> Result<()> {
    let mut batch: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(folder)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path().to_path_buf();
        if !is_eligible_path(&path) {
            continue;
        }
        // Rate check.
        let mut lim = limits.lock().await;
        if !lim.try_increment(folder, hourly_cap, daily_cap, daily_token_budget) {
            eprintln!(
                "[watch] initial scan hit rate limit for {}",
                folder.display()
            );
            break;
        }
        drop(lim);
        batch.push(path);
    }

    if !batch.is_empty() {
        // Emit added events for each file.
        for p in &batch {
            let payload = AddedEvent {
                path: p.to_string_lossy().into_owned(),
            };
            let _ = app.emit("folder-watch:added", payload);
        }
        // Queue for auto-process.
        let paths_str: Vec<String> = batch.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        {
            let mut q = queue.lock().await;
            q.entry(folder.to_path_buf())
                .or_default()
                .extend(batch);
        }
        let payload = AutoProcessEvent {
            paths: paths_str,
            mode,
            folder: folder.to_string_lossy().into_owned(),
        };
        let _ = app.emit("folder-watch:auto-process", payload);
    }
    Ok(())
}

// ── Event handling ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn handle_event(
    event: notify::Event,
    app: &AppHandle,
    dedup: &Arc<Mutex<HashMap<PathBuf, Instant>>>,
    auto_queue: &Arc<Mutex<HashMap<PathBuf, Vec<PathBuf>>>>,
    rate_limits: &Arc<Mutex<RateLimits>>,
    watched_folder: &Path,
    mode: WatchMode,
    hourly_cap: u32,
    daily_cap: u32,
    daily_token_budget: u64,
) {
    if !is_relevant_kind(&event.kind) {
        return;
    }
    for path in event.paths {
        if !is_eligible_path(&path) {
            continue;
        }
        let dedup_clone = dedup.clone();
        let app_clone = app.clone();
        let path_clone = path.clone();
        let auto_queue_clone = auto_queue.clone();
        let rate_limits_clone = rate_limits.clone();
        let folder_clone = watched_folder.to_path_buf();
        tokio::spawn(async move {
            // Debounce.
            {
                let mut map = dedup_clone.lock().await;
                let now = Instant::now();
                if let Some(prev) = map.get(&path_clone) {
                    if now.duration_since(*prev) < DEDUP_WINDOW {
                        return;
                    }
                }
                map.insert(path_clone.clone(), now);
                map.retain(|_, t| now.duration_since(*t) < DEDUP_WINDOW * 8);
            }

            // Always emit the detection event (backwards compatible).
            let payload = AddedEvent {
                path: path_clone.to_string_lossy().into_owned(),
            };
            if let Err(e) = app_clone.emit("folder-watch:added", payload) {
                eprintln!("[watch] emit failed: {e}");
            }

            // Auto-process path if mode is not Off.
            if mode != WatchMode::Off {
                // Rate limit check.
                let mut lim = rate_limits_clone.lock().await;
                if !lim.try_increment(&folder_clone, hourly_cap, daily_cap, daily_token_budget) {
                    eprintln!(
                        "[watch] rate limit hit for {} — skipping auto-process",
                        folder_clone.display()
                    );
                    return;
                }
                drop(lim);

                // Add to queue.
                {
                    let mut q = auto_queue_clone.lock().await;
                    q.entry(folder_clone.clone()).or_default().push(path_clone.clone());
                }

                // Schedule a delayed flush. We spawn a task that waits
                // the batch window, then flushes whatever is in the queue
                // for this folder.
                let app_flush = app_clone.clone();
                let queue_flush = auto_queue_clone.clone();
                let rate_flush = rate_limits_clone.clone();
                let folder_flush = folder_clone.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(AUTO_DISPATCH_DELAY).await;
                    let mut q = queue_flush.lock().await;
                    let batch = q.remove(&folder_flush).unwrap_or_default();
                    let total_pending: usize = q.values().map(|v| v.len()).sum();
                    drop(q);
                    if batch.is_empty() {
                        return;
                    }
                    let paths: Vec<String> =
                        batch.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    let payload = AutoProcessEvent {
                        paths,
                        mode,
                        folder: folder_flush.to_string_lossy().into_owned(),
                    };
                    if let Err(e) = app_flush.emit("folder-watch:auto-process", payload) {
                        eprintln!("[watch] auto-process emit failed: {e}");
                    }
                    // Emit status event so the frontend can update badges.
                    let lim = rate_flush.lock().await;
                    let status = WatchStatusEvent {
                        daily_processed: lim.daily.1,
                        daily_cap,
                        daily_tokens_used: lim.daily_tokens.1,
                        daily_token_budget,
                        pending_files: total_pending,
                        dead_letter_count: 0, // not accessible here; frontend polls
                    };
                    drop(lim);
                    let _ = app_flush.emit("folder-watch:status", status);
                });
            }
        });
    }
}

fn is_relevant_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(CreateKind::File)
            | EventKind::Create(CreateKind::Any)
            | EventKind::Modify(ModifyKind::Name(RenameMode::To))
            | EventKind::Modify(ModifyKind::Name(RenameMode::Both))
    )
}

fn is_eligible_path(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    if name.starts_with('.') || name.ends_with('~') {
        return false;
    }
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_ascii_lowercase(),
        None => return false,
    };
    if matches!(ext.as_str(), "tmp" | "swp" | "swx" | "part" | "crdownload") {
        return false;
    }
    if !ALLOWED_EXTS.iter().any(|allowed| ext == *allowed) {
        return false;
    }
    matches!(std::fs::metadata(path), Ok(m) if m.is_file() && m.len() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, EventKind, ModifyKind, RenameMode};

    #[test]
    fn dotfiles_rejected() {
        assert!(!is_eligible_path(Path::new("/x/.DS_Store")));
        assert!(!is_eligible_path(Path::new("/x/.hidden.pdf")));
    }

    #[test]
    fn editor_swap_rejected() {
        assert!(!is_eligible_path(Path::new("/x/document.tmp")));
        assert!(!is_eligible_path(Path::new("/x/document.crdownload")));
        assert!(!is_eligible_path(Path::new("/x/document.swp")));
        assert!(!is_eligible_path(Path::new("/x/document.txt~")));
    }

    #[test]
    fn unknown_extension_rejected() {
        assert!(!is_eligible_path(Path::new("/x/script.sh")));
    }

    #[test]
    fn part_extension_rejected() {
        assert!(!is_eligible_path(Path::new("/x/document.part")));
    }

    #[test]
    fn no_extension_rejected() {
        assert!(!is_eligible_path(Path::new("/tmp/Makefile")));
    }

    #[test]
    fn is_relevant_kind_create_file() {
        assert!(is_relevant_kind(&EventKind::Create(CreateKind::File)));
    }

    #[test]
    fn is_relevant_kind_rename_to() {
        assert!(is_relevant_kind(&EventKind::Modify(ModifyKind::Name(
            RenameMode::To
        ))));
    }

    #[test]
    fn is_relevant_kind_data_write_false() {
        assert!(!is_relevant_kind(&EventKind::Modify(ModifyKind::Data(
            DataChange::Any
        ))));
    }

    #[test]
    fn watch_mode_default_is_off() {
        assert_eq!(WatchMode::default(), WatchMode::Off);
    }

    #[test]
    fn watch_mode_serde_roundtrip() {
        let modes = vec![WatchMode::Off, WatchMode::Analyse, WatchMode::Sort];
        for m in modes {
            let json = serde_json::to_string(&m).unwrap();
            let back: WatchMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn rate_limits_basic() {
        let mut rl = RateLimits::new();
        let folder = PathBuf::from("/test");
        // Should allow up to cap.
        for _ in 0..5 {
            assert!(rl.try_increment(&folder, 5, 100, 0));
        }
        // Should reject after cap.
        assert!(!rl.try_increment(&folder, 5, 100, 0));
    }

    #[test]
    fn rate_limits_daily_cap() {
        let mut rl = RateLimits::new();
        let f1 = PathBuf::from("/a");
        let f2 = PathBuf::from("/b");
        // Fill daily cap (3) across two folders.
        assert!(rl.try_increment(&f1, 100, 3, 0));
        assert!(rl.try_increment(&f2, 100, 3, 0));
        assert!(rl.try_increment(&f1, 100, 3, 0));
        // Daily cap hit.
        assert!(!rl.try_increment(&f2, 100, 3, 0));
    }

    #[test]
    fn rate_limits_token_budget() {
        let mut rl = RateLimits::new();
        let folder = PathBuf::from("/test");
        // With token budget of 1000, should allow files initially.
        assert!(rl.try_increment(&folder, 100, 500, 1000));
        // Record 900 tokens consumed.
        rl.record_tokens(900);
        // Still under budget.
        assert!(rl.try_increment(&folder, 100, 500, 1000));
        // Record another 200 → total 1100, over budget.
        rl.record_tokens(200);
        // Should now be rejected by token budget.
        assert!(!rl.try_increment(&folder, 100, 500, 1000));
    }

    #[test]
    fn token_budget_zero_means_unlimited() {
        let mut rl = RateLimits::new();
        let folder = PathBuf::from("/test");
        rl.record_tokens(999_999_999);
        // Budget of 0 = unlimited — should still allow.
        assert!(rl.try_increment(&folder, 100, 500, 0));
    }
}
