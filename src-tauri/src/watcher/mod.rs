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
//! ## Design notes
//!
//! - **Multi-folder.** State holds `HashMap<PathBuf, RecommendedWatcher>`
//!   keyed by canonical path. The event payload carries the file path
//!   so consumers don't need to know which watcher fired. Adding the
//!   same folder twice is idempotent (existing watcher is preserved).
//! - **Debouncing.** A given file often produces multiple events on
//!   atomic-save patterns (write to temp, rename into place). We keep
//!   a small per-path "recently emitted" map and skip dupes within a
//!   2-second window.
//! - **Extension filter.** Only paths matching the same extensions
//!   the rest of the app indexes get emitted (`pdf`, `epub`, `docx`,
//!   `txt`, `md`). Editor swap files (`.tmp`, `.swp`, dot-prefixed)
//!   are dropped.
//! - **No auto-process.** The watcher only *adds* to the batch. The
//!   user still presses Start. A future "auto-process" toggle could
//!   trigger `processAll` after each add, but auto-moving files is
//!   too risky to land here.

use anyhow::{Context, Result};
use notify::{
    event::{CreateKind, EventKind, ModifyKind, RenameMode},
    RecommendedWatcher, RecursiveMode, Watcher as _,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

const DEDUP_WINDOW: Duration = Duration::from_secs(2);

/// Extensions worth importing — same set the rest of the app
/// recognises. Lowercased; the matcher folds before comparing.
const ALLOWED_EXTS: &[&str] = &[
    "pdf", "epub", "djvu", "txt", "md", "rtf", "doc", "docx", "odt",
    // Common image formats are excluded — the batch processor doesn't
    // OCR everything by default, so importing JPEGs from a watched
    // folder would create noisy "poor extraction" rows.
];

/// Tauri event payload — kept stable so the frontend listener can
/// stay simple. The field name `path` matches what the rest of the
/// codebase uses for absolute file paths.
#[derive(Debug, Clone, Serialize)]
pub struct AddedEvent {
    pub path: String,
}

/// State held in `AppState`. Holds one `RecommendedWatcher` per watched
/// folder, keyed by the canonical path. Adding the same folder twice
/// is idempotent — `start` returns `Ok(())` without touching the
/// existing watcher. Dropping a watcher (via `stop_one`) stops fs
/// events for that folder; the others keep emitting.
pub struct WatcherState {
    /// Map of canonical path → `RecommendedWatcher`. Owning each
    /// watcher keeps the platform handle alive — drop the entry to
    /// stop events for that folder.
    watchers: HashMap<PathBuf, RecommendedWatcher>,
    /// Per-path debounce map (last-emit instant). Shared across all
    /// active watchers — debounce is keyed on file path, not on which
    /// watcher saw the event, so a single map is correct.
    last_emit: Arc<Mutex<HashMap<PathBuf, Instant>>>,
}

impl WatcherState {
    pub fn new() -> Self {
        Self {
            watchers: HashMap::new(),
            last_emit: Arc::new(Mutex::new(HashMap::new())),
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
}

impl Default for WatcherState {
    fn default() -> Self {
        Self::new()
    }
}

/// Start watching `folder` recursively. Idempotent: if the canonical
/// path is already in the watch set, returns `Ok(())` without spawning
/// a duplicate watcher. Errors out if the path doesn't exist, isn't a
/// directory, or fails to register with the platform.
pub fn start(state: &mut WatcherState, app: AppHandle, folder: PathBuf) -> Result<()> {
    let folder = folder
        .canonicalize()
        .with_context(|| format!("watch path does not resolve: {}", folder.display()))?;
    if !folder.is_dir() {
        anyhow::bail!("watch path is not a directory: {}", folder.display());
    }
    if state.watchers.contains_key(&folder) {
        return Ok(());
    }

    let dedup = state.last_emit.clone();
    let app_for_handler = app.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) => handle_event(event, &app_for_handler, &dedup),
            Err(e) => eprintln!("[watch] notify error: {e}"),
        }
    })
    .context("create fs watcher")?;

    watcher
        .watch(&folder, RecursiveMode::Recursive)
        .with_context(|| format!("watch start failed for {}", folder.display()))?;

    state.watchers.insert(folder, watcher);
    Ok(())
}

/// Stop watching a single folder. Looks up by canonical path; missing
/// entries are silently ignored (idempotent). Returns whether anything
/// was actually removed.
pub fn stop_one(state: &mut WatcherState, folder: &Path) -> bool {
    let canonical = match folder.canonicalize() {
        Ok(p) => p,
        // Path may have been deleted since we started watching it.
        // Fall back to a literal lookup so the user can still remove
        // a stale entry from the list.
        Err(_) => folder.to_path_buf(),
    };
    state.watchers.remove(&canonical).is_some()
        || state.watchers.remove(folder).is_some()
}

/// Stop all active watchers.
pub fn stop_all(state: &mut WatcherState) {
    state.watchers.clear();
}

fn handle_event(
    event: notify::Event,
    app: &AppHandle,
    dedup: &Arc<Mutex<HashMap<PathBuf, Instant>>>,
) {
    if !is_relevant_kind(&event.kind) {
        return;
    }
    for path in event.paths {
        if !is_eligible_path(&path) {
            continue;
        }
        // Debounce in a blocking-poll style — we're inside a notify
        // worker thread, not a tokio task. `try_lock` would race;
        // `blocking_lock` blocks the worker briefly which is fine.
        let dedup_clone = dedup.clone();
        let app_clone = app.clone();
        let path_clone = path.clone();
        tokio::spawn(async move {
            let mut map = dedup_clone.lock().await;
            let now = Instant::now();
            if let Some(prev) = map.get(&path_clone) {
                if now.duration_since(*prev) < DEDUP_WINDOW {
                    return;
                }
            }
            map.insert(path_clone.clone(), now);
            // Garbage-collect: keep the map small even on busy folders.
            map.retain(|_, t| now.duration_since(*t) < DEDUP_WINDOW * 8);
            drop(map);

            let payload = AddedEvent {
                path: path_clone.to_string_lossy().into_owned(),
            };
            if let Err(e) = app_clone.emit("folder-watch:added", payload) {
                eprintln!("[watch] emit failed: {e}");
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
    // Skip dotfiles, swap/lock/temp files. These are common during
    // editor saves and downloads-in-progress.
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
    // Skip directories (CreateKind::Any can fire for them on some
    // platforms) and zero-byte placeholders.
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
        assert!(!is_eligible_path(Path::new("/x/photo.jpg")));
        assert!(!is_eligible_path(Path::new("/x/script.sh")));
    }

    // --- is_eligible_path edge cases ---

    #[test]
    fn part_extension_rejected() {
        assert!(!is_eligible_path(Path::new("/x/document.part")));
    }

    #[test]
    fn no_extension_rejected() {
        assert!(!is_eligible_path(Path::new("/tmp/Makefile")));
    }

    // --- is_relevant_kind ---

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
}
