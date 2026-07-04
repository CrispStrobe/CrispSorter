//! Tauri commands for the cloud drive registry (P11 Pillar 5).

use tauri::State;
use crate::AppState;
use super::{DriveConfig, DriveRegistry, DriveType};

/// List all configured drives.
#[tauri::command]
pub async fn drive_list(state: State<'_, AppState>) -> Result<Vec<DriveConfig>, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    DriveRegistry::open(&data_dir)
        .map(|r| r.drives)
        .map_err(|e| e.to_string())
}

/// Add or update a drive entry.
///
/// `kind` must be one of: "local", "filen", "internxt", "sftp", "webdav".
/// `path` is the root path (for local/sftp/filen: OS mount-point or CLI
/// root; for `webdav`: base URL like `https://host/dav/`).
/// `username` / `password` are only used for `webdav`.
#[tauri::command]
pub async fn drive_create(
    state: State<'_, AppState>,
    label: String,
    kind: String,
    path: String,
    username: Option<String>,
    password: Option<String>,
    insecure_tls: Option<bool>,
) -> Result<DriveConfig, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let drive_type = match kind.as_str() {
        "filen"    => DriveType::Filen,
        "internxt" => DriveType::Internxt,
        "sftp"     => DriveType::Sftp,
        "webdav"   => DriveType::WebDav,
        _          => DriveType::Local,
    };
    let config = DriveConfig {
        id:    uuid::Uuid::new_v4().to_string(),
        label, kind: drive_type, path, username, password, insecure_tls,
        access_token: None, refresh_token: None, client_id: None, client_secret: None,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    reg.add(config.clone()).map_err(|e| e.to_string())?;
    Ok(config)
}

/// Remove a drive by id.  Returns `true` if it was found and removed.
#[tauri::command]
pub async fn drive_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    reg.remove(&id).map_err(|e| e.to_string())
}

/// Update an existing drive's label / kind / path / auth in place.
/// Preserves the `id` so existing `crisp+drive://<id>/...` URIs in the
/// LanceDB index continue to resolve to the (now-edited) drive.  Errors
/// if no drive with the given id exists.
///
/// `kind` accepts the same values as `drive_create`.
#[tauri::command]
pub async fn drive_update(
    state: State<'_, AppState>,
    id: String,
    label: String,
    kind: String,
    path: String,
    username: Option<String>,
    password: Option<String>,
    insecure_tls: Option<bool>,
) -> Result<DriveConfig, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let drive_type = match kind.as_str() {
        "filen"    => DriveType::Filen,
        "internxt" => DriveType::Internxt,
        "sftp"     => DriveType::Sftp,
        "webdav"   => DriveType::WebDav,
        _          => DriveType::Local,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|d| d.id == id) {
        return Err(format!("drive '{id}' not found"));
    }
    let updated = DriveConfig {
        id, label, kind: drive_type, path, username, password, insecure_tls,
        access_token: None, refresh_token: None, client_id: None, client_secret: None,
    };
    // `add` dedupes by id (replaces) — exactly the semantics we want.
    reg.add(updated.clone()).map_err(|e| e.to_string())?;
    Ok(updated)
}

/// List directory entries on a drive.
#[tauri::command]
pub async fn drive_list_dir(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<Vec<super::DirEntry>, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    drive.list_dir(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

/// Stat a file or directory on a drive.
#[tauri::command]
pub async fn drive_stat(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<super::FileStat, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    drive.stat(std::path::Path::new(&path)).map_err(|e| e.to_string())
}
