//! Asking for, and remembering, permission to write to a folder.
//!
//! The shipped App Store build is sandboxed with
//! `files.user-selected.read-write` and nothing broader, so it can write
//! exactly where the user has pointed a file dialog. Sorting into a
//! destination root they never picked fails with `EPERM` — and a 545-item
//! sort produced 512 of those, each reported as "not writable at target",
//! which sent people to check permission bits that were never involved.
//!
//! So: [`probe`] says whether a directory is writable *now*, and
//! distinguishes "the OS refused" from "the folder refused".
//! [`grant`] turns a folder the user has just picked into a persisted
//! security-scoped bookmark, and [`restore_all`] reopens those at
//! startup. [`decline`] records a folder the user does not want to be
//! asked about again, because "ask me every time" is its own kind of
//! broken.
//!
//! Precedence, when the user has both granted and declined a folder: the
//! grant wins. A grant is something they actively did; a decline only
//! means "stop interrupting me".

pub mod bookmarks;
pub mod tauri_commands;

use bookmarks::Registry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Basename of the grant store under the app data dir.
pub const STORE_FILE: &str = "folder_grants.json";

/// What [`probe`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Writability {
    /// Writable right now; nothing to ask.
    Writable,
    /// The OS refused (`EPERM`). Asking the user to pick this folder is
    /// what fixes it — not chmod.
    NeedsPermission { reason: String },
    /// A genuine filesystem problem: read-only mount, missing parent,
    /// permission bits. Picking the folder will not help.
    Unwritable { reason: String },
}

impl Writability {
    pub fn is_writable(&self) -> bool {
        matches!(self, Writability::Writable)
    }
}

/// Can we create `dir` and write into it, right now?
///
/// Probes by actually trying, because nothing else is conclusive: mode
/// bits say nothing about the sandbox, and the sandbox says nothing about
/// a read-only mount. The probe file is removed immediately.
pub fn probe(dir: &Path) -> Writability {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return classify(dir, &e);
    }
    // Existing *and* creatable are different questions from writable: a
    // directory can exist inside a granted scope yet sit on a read-only
    // volume. A create-delete round trip answers the one that matters.
    let probe = dir.join(".crispsorter-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Writability::Writable
        }
        Err(e) => classify(dir, &e),
    }
}

fn classify(dir: &Path, e: &std::io::Error) -> Writability {
    // `EPERM` (1) is not `EACCES` (13). Permission *bits* produce the
    // latter; the former means something stronger refused — on a
    // sandboxed macOS build, the sandbox.
    #[cfg(unix)]
    if e.raw_os_error() == Some(1) {
        return Writability::NeedsPermission {
            reason: format!(
                "macOS has not granted CrispSorter access to {}",
                dir.display()
            ),
        };
    }
    Writability::Unwritable {
        reason: format!("{}: {}", dir.display(), e),
    }
}

// ── The persisted store ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub path: PathBuf,
    /// Base64 security-scoped bookmark. Absent off macOS, where the
    /// grant is recorded for bookkeeping but buys nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Folders the user asked not to be prompted about again.
    #[serde(default)]
    pub declined: Vec<PathBuf>,
}

fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STORE_FILE)
}

/// Read the store. A missing or corrupt file reads as empty — losing the
/// grants is recoverable by asking again, whereas refusing to start is
/// not.
pub fn load_store(data_dir: &Path) -> Store {
    std::fs::read_to_string(store_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_store(data_dir: &Path, store: &Store) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("creating {}: {e}", data_dir.display()))?;
    let body = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    std::fs::write(store_path(data_dir), body)
        .map_err(|e| format!("writing {}: {e}", store_path(data_dir).display()))
}

// ── Process state ───────────────────────────────────────────────────

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

pub(crate) fn data_dir() -> PathBuf {
    DATA_DIR
        .get()
        .cloned()
        .unwrap_or_else(crate::secrets::vault::default_app_data_dir)
}

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

/// Reopen every stored grant. Call once at startup, before anything
/// tries to write.
///
/// Returns the paths that could not be reopened — stale bookmarks, or
/// folders that have since gone. Those are worth surfacing: the user
/// granted them once and would otherwise watch them fail silently.
pub fn restore_all() -> Vec<(PathBuf, String)> {
    let dir = data_dir();
    let store = load_store(&dir);
    let mut failed = Vec::new();
    let mut reg = match registry().lock() {
        Ok(r) => r,
        Err(_) => return failed,
    };
    for g in &store.grants {
        let Some(blob) = g.bookmark.as_ref() else {
            continue;
        };
        let bytes = match base64_decode(blob) {
            Some(b) => b,
            None => {
                failed.push((g.path.clone(), "stored permission is unreadable".into()));
                continue;
            }
        };
        match bookmarks::resolve(&bytes) {
            Ok(access) => {
                if access.stale {
                    // Resolution succeeded but points somewhere else now.
                    // Holding it anyway would make a moved folder look
                    // granted while writes land in the wrong place.
                    failed.push((g.path.clone(), bookmarks::BookmarkError::Stale.to_string()));
                    continue;
                }
                reg.hold(access);
            }
            Err(bookmarks::BookmarkError::Unsupported) => {}
            Err(e) => failed.push((g.path.clone(), e.to_string())),
        }
    }
    failed
}

/// Record a folder the user has just picked, and hold it open.
///
/// Must run while the pick is still live — the bookmark can only be
/// created while access is held.
pub fn grant(dir: &Path) -> Result<(), String> {
    let data = data_dir();
    let mut store = load_store(&data);

    let blob = match bookmarks::create(dir) {
        Ok(bytes) => {
            match bookmarks::resolve(&bytes) {
                Ok(access) => {
                    if let Ok(mut reg) = registry().lock() {
                        reg.hold(access);
                    }
                }
                // Creation worked, reopening did not. Keep the bookmark:
                // the live pick still grants access for this run, and the
                // next launch gets another chance.
                Err(e) => eprintln!("folder_access: saved but could not reopen {}: {e}", dir.display()),
            }
            Some(base64_encode(&bytes))
        }
        // Off macOS, and on an un-entitled build, there is nothing to
        // persist. Still record the grant so the UI stops asking.
        Err(bookmarks::BookmarkError::Unsupported) => None,
        Err(e) => return Err(e.to_string()),
    };

    store.grants.retain(|g| g.path != dir);
    store.grants.push(Grant {
        path: dir.to_path_buf(),
        bookmark: blob,
        created_unix: now_unix(),
    });
    // Granting is a deliberate act; it clears any earlier "don't ask".
    store.declined.retain(|p| p != dir);
    save_store(&data, &store)
}

/// Record that the user does not want to be asked about `dir` again.
pub fn decline(dir: &Path) -> Result<(), String> {
    let data = data_dir();
    let mut store = load_store(&data);
    if !store.declined.iter().any(|p| p == dir) {
        store.declined.push(dir.to_path_buf());
    }
    save_store(&data, &store)
}

/// Drop a stored grant. Access already open stays open until the process
/// ends — macOS gives no way to revoke it, and pretending otherwise
/// would be a lie.
pub fn forget(dir: &Path) -> Result<(), String> {
    let data = data_dir();
    let mut store = load_store(&data);
    store.grants.retain(|g| g.path != dir);
    save_store(&data, &store)
}

/// Should the user be prompted about `dir`?
///
/// No, if a held grant already covers it, or if they declined it (or any
/// parent of it — declining a tree means the whole tree).
pub fn should_ask(dir: &Path) -> bool {
    if let Ok(reg) = registry().lock() {
        if reg.covers(dir) {
            return false;
        }
    }
    let store = load_store(&data_dir());
    !store.declined.iter().any(|d| dir.starts_with(d))
}

/// Folders held open in this process.
pub fn held_paths() -> Vec<PathBuf> {
    registry()
        .lock()
        .map(|r| r.paths())
        .unwrap_or_default()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writable_dir_probes_writable_and_leaves_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("Sorted/Author/2023");
        assert_eq!(probe(&target), Writability::Writable);
        // The probe file must not survive — it would show up in the user's
        // sorted library.
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn eperm_asks_for_permission_and_eacces_does_not() {
        // The whole distinction the sandbox failure turned on.
        let p = Path::new("/nope");
        assert!(matches!(
            classify(p, &std::io::Error::from_raw_os_error(1)),
            Writability::NeedsPermission { .. }
        ));
        assert!(matches!(
            classify(p, &std::io::Error::from_raw_os_error(13)),
            Writability::Unwritable { .. }
        ));
    }

    #[test]
    fn the_store_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let s = Store {
            grants: vec![Grant {
                path: PathBuf::from("/a/b"),
                bookmark: Some("Ym9va21hcms=".into()),
                created_unix: 42,
            }],
            declined: vec![PathBuf::from("/c")],
        };
        save_store(d.path(), &s).unwrap();
        let back = load_store(d.path());
        assert_eq!(back.grants.len(), 1);
        assert_eq!(back.grants[0].path, PathBuf::from("/a/b"));
        assert_eq!(back.declined, vec![PathBuf::from("/c")]);
    }

    #[test]
    fn a_corrupt_store_reads_as_empty_rather_than_failing() {
        // Losing grants means asking again; refusing to start does not.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(store_path(d.path()), b"{{{").unwrap();
        let s = load_store(d.path());
        assert!(s.grants.is_empty() && s.declined.is_empty());
    }

    #[test]
    fn base64_round_trips_bookmark_bytes() {
        let raw: Vec<u8> = (0u8..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&raw)).unwrap(), raw);
    }
}
