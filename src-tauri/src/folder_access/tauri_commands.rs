//! Tauri surface for folder permissions.
//!
//! The flow the frontend drives:
//!
//! 1. A sort fails with `NOT_PERMITTED` naming a folder.
//! 2. `folder_access_should_ask` — has the user already granted it, or
//!    told us to stop asking?
//! 3. If yes, the frontend opens a *folder picker* at that path. Only the
//!    picker can grant a sandboxed app access; Rust cannot conjure it.
//! 4. `folder_access_grant` with what the user picked — creates and
//!    stores the security-scoped bookmark while the pick is still live.
//! 5. Retry the failed items.
//!
//! Step 3 has to be the frontend's job: the grant comes from the user
//! driving the system's own dialog, so the dialog must be the one the
//! plugin opens.

use super::{decline, forget, grant, held_paths, load_store, probe, should_ask, Writability};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStatus {
    pub path: PathBuf,
    pub writability: Writability,
    /// `true` when picking the folder would plausibly fix it.
    pub can_request: bool,
    /// `false` when a grant already covers it or the user declined.
    pub should_ask: bool,
}

/// Probe a destination folder.
#[tauri::command]
pub async fn folder_access_probe(path: PathBuf) -> Result<FolderStatus, String> {
    let w = probe(&path);
    let needs = matches!(w, Writability::NeedsPermission { .. });
    Ok(FolderStatus {
        can_request: needs,
        should_ask: needs && should_ask(&path),
        writability: w,
        path,
    })
}

/// Has the user already answered for this folder, one way or the other?
#[tauri::command]
pub async fn folder_access_should_ask(path: PathBuf) -> Result<bool, String> {
    Ok(should_ask(&path))
}

/// Persist the folder the user just picked, and hold it open.
///
/// `path` must be what the picker returned. Passing anything else stores
/// a bookmark for a folder the app was never granted, which resolves to
/// nothing useful later.
#[tauri::command]
pub async fn folder_access_grant(path: PathBuf) -> Result<FolderStatus, String> {
    grant(&path)?;
    let w = probe(&path);
    Ok(FolderStatus {
        can_request: false,
        should_ask: false,
        writability: w,
        path,
    })
}

/// Stop asking about this folder. The "unless the user chooses not to"
/// half of the contract.
#[tauri::command]
pub async fn folder_access_decline(path: PathBuf) -> Result<(), String> {
    decline(&path)
}

/// Drop a stored grant.
#[tauri::command]
pub async fn folder_access_forget(path: PathBuf) -> Result<(), String> {
    forget(&path)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantsView {
    /// Folders with a stored grant.
    pub granted: Vec<PathBuf>,
    /// Folders held open in this process. Shorter than `granted` when a
    /// bookmark went stale.
    pub active: Vec<PathBuf>,
    /// Folders the user asked not to be prompted about.
    pub declined: Vec<PathBuf>,
    /// `true` where persisting a grant is possible at all.
    pub supported: bool,
}

/// Everything a settings pane needs to show and revoke grants.
#[tauri::command]
pub async fn folder_access_list() -> Result<GrantsView, String> {
    let store = load_store(&super::data_dir());
    Ok(GrantsView {
        granted: store.grants.iter().map(|g| g.path.clone()).collect(),
        active: held_paths(),
        declined: store.declined,
        supported: cfg!(target_os = "macos"),
    })
}
