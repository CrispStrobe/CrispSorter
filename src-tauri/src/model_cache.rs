//! The one CrispEmbed model cache directory shared by the app, the CLI and
//! the OCR worker subprocess.
//!
//! Resolved once, at process start, before any thread exists:
//!   1. `CRISPEMBED_CACHE_DIR` when the user set it,
//!   2. otherwise CrispEmbed's own per-user default (`~/.cache/crispembed`).
//!
//! [`init_process`] exports the result as `CRISPEMBED_CACHE_DIR` exactly once,
//! while the process is still single-threaded, so the native library, the OCR
//! worker child and this module agree. Nothing else may set the variable: a
//! later `set_var` races every thread reading the environment (the former
//! Settings download did that).
//!
//! Earlier releases also downloaded registry models into the app's model
//! directory (`<app data>/models`, `CRISPSORTER_MODEL_CACHE_DIR`, or the
//! configured index model folder). Those directories stay *readable*
//! ([`add_legacy_dir`]) so installed models keep working; new downloads only
//! go to [`dir`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

const ENV: &str = "CRISPEMBED_CACHE_DIR";

static CANONICAL: OnceLock<PathBuf> = OnceLock::new();
static LEGACY: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());
static OFFLINE: AtomicBool = AtomicBool::new(false);

fn from_env() -> Option<PathBuf> {
    std::env::var_os(ENV)
        .map(|value| value.to_string_lossy().trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// CrispEmbed's built-in default, mirrored from `model_mgr.cpp::cache_dir()`.
fn native_default() -> PathBuf {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME")
    };
    let home = home.filter(|h| !h.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".cache").join("crispembed")
}

fn resolve() -> PathBuf { from_env().unwrap_or_else(native_default) }

/// Call first thing in `main`, before any thread is spawned. Idempotent.
pub fn init_process() {
    let dir = CANONICAL.get_or_init(resolve).clone();
    if std::env::var_os(ENV).map(PathBuf::from).as_deref() != Some(dir.as_path()) {
        // SAFETY: called from `main` before the Tauri runtime, the CLI, or any
        // worker thread starts, so no other thread can read the environment.
        unsafe { std::env::set_var(ENV, &dir) };
    }
    if let Ok(value) = std::env::var("CRISPSORTER_MODEL_CACHE_DIR") {
        if !value.trim().is_empty() { add_legacy_dir(PathBuf::from(value.trim())); }
    }
    // A child process (the isolated OCR worker) inherits every location the
    // parent searched, so a model the plan reports installed is usable there.
    if let Some(value) = std::env::var_os(SEARCH_PATH_ENV) {
        for path in std::env::split_paths(&value) { add_legacy_dir(path); }
    }
}

/// Environment variable carrying [`search_dirs`] to OCR child processes.
pub const SEARCH_PATH_ENV: &str = "CRISPSORTER_MODEL_SEARCH_PATH";

/// [`search_dirs`] encoded for [`SEARCH_PATH_ENV`] (platform path-list syntax).
pub fn search_path_value() -> std::ffi::OsString {
    std::env::join_paths(search_dirs()).unwrap_or_default()
}

/// Environment for an OCR child process: the canonical cache, every search
/// location and the offline switch. Apply with `Command::envs`.
pub fn child_env() -> Vec<(&'static str, std::ffi::OsString)> {
    let mut env = vec![(ENV, dir().into_os_string()), (SEARCH_PATH_ENV, search_path_value())];
    if offline() { env.push(("CRISPEMBED_OFFLINE", "1".into())); }
    env
}

/// The canonical cache directory. Falls back to resolving lazily (without
/// touching the environment) in tests and library callers.
pub fn dir() -> PathBuf { CANONICAL.get_or_init(resolve).clone() }

/// Register an older download location that must still be searched.
pub fn add_legacy_dir(path: PathBuf) {
    if path.as_os_str().is_empty() || path == dir() { return; }
    if let Ok(mut dirs) = LEGACY.write() {
        if !dirs.contains(&path) { dirs.push(path); }
    }
}

/// Canonical directory first, then legacy directories in registration order.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![dir()];
    if let Ok(legacy) = LEGACY.read() {
        for path in legacy.iter() { if !dirs.contains(path) { dirs.push(path.clone()); } }
    }
    dirs
}

/// `--offline` (CLI) or the app's offline switch, plus the native variables.
pub fn set_offline(offline: bool) { OFFLINE.store(offline, Ordering::SeqCst); }

pub fn offline() -> bool {
    OFFLINE.load(Ordering::SeqCst) || ["CRISPEMBED_OFFLINE", "HF_HUB_OFFLINE"].iter().any(|key| {
        std::env::var(key).ok().is_some_and(|value| {
            matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        })
    })
}

/// A non-empty regular file (symlinks to regular files are accepted: users
/// commonly link a cache to an external drive).
pub fn usable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// The directory that holds every file of a registry model, if any does.
/// `files[0]` is the primary file; the rest are companions that must sit in
/// the same directory (vision towers are found next to the model).
pub fn locate_files(files: &[&str]) -> Option<PathBuf> {
    let (primary, companions) = files.split_first()?;
    search_dirs().into_iter().find(|dir| {
        usable_file(&dir.join(primary)) && companions.iter().all(|name| usable_file(&dir.join(name)))
    }).map(|dir| dir.join(primary))
}

/// Registry filenames of an exact model name: primary first, then companions.
#[cfg(feature = "crispembed")]
pub fn registry_files(name: &str) -> Option<Vec<String>> {
    if let Ok(Some(info)) = crispembed::CrispEmbed::model_download_info(name) {
        let mut files = vec![info.primary.filename];
        files.extend(info.companions.into_iter().map(|file| file.filename));
        return Some(files);
    }
    crispembed::CrispEmbed::list_models().into_iter()
        .find(|model| model.name == name || model.filename == name)
        .map(|model| vec![model.filename])
}

/// The installed path of a registry model, searching every cache directory.
#[cfg(feature = "crispembed")]
pub fn locate_registry(name: &str) -> Option<PathBuf> {
    let files = registry_files(name)?;
    locate_files(&files.iter().map(String::as_str).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companions_must_share_the_primary_directory() {
        let legacy = tempfile::tempdir().unwrap();
        add_legacy_dir(legacy.path().to_path_buf());
        let primary = format!("model-cache-test-{}.gguf", uuid::Uuid::new_v4());
        let companion = format!("mmproj-{primary}");
        std::fs::write(legacy.path().join(&primary), b"GGUF").unwrap();
        assert!(locate_files(&[&primary, &companion]).is_none(), "a missing companion is not installed");
        std::fs::write(legacy.path().join(&companion), b"GGUF").unwrap();
        assert_eq!(locate_files(&[&primary, &companion]), Some(legacy.path().join(&primary)));
        // Empty files (an interrupted native download) are not installs.
        std::fs::write(legacy.path().join(&primary), b"").unwrap();
        assert!(locate_files(&[&primary]).is_none());
    }

    #[test]
    fn child_processes_receive_every_search_location() {
        let legacy = tempfile::tempdir().unwrap();
        add_legacy_dir(legacy.path().to_path_buf());
        let env = child_env();
        let (_, list) = env.iter().find(|(key, _)| *key == SEARCH_PATH_ENV).expect("search path exported");
        let paths: Vec<PathBuf> = std::env::split_paths(list).collect();
        assert_eq!(paths[0], dir(), "the canonical cache stays first");
        assert!(paths.contains(&legacy.path().to_path_buf()));
        assert_eq!(env.iter().find(|(key, _)| *key == ENV).map(|(_, v)| PathBuf::from(v)), Some(dir()));
    }

    #[test]
    fn canonical_directory_is_searched_first_and_registered_once() {
        let first = search_dirs();
        assert_eq!(first[0], dir());
        add_legacy_dir(dir());
        assert_eq!(search_dirs().iter().filter(|d| **d == dir()).count(), 1);
    }
}
