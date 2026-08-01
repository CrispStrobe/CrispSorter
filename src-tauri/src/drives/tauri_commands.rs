//! Tauri commands for the cloud drive registry (P11 Pillar 5).

use super::{DriveConfig, DriveRegistry, DriveType};
use crate::AppState;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::State;

#[cfg(feature = "fuse")]
static FUSE_MOUNTS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
pub struct DriveCredentialsStatus {
    pub has_username: bool,
    pub has_password: bool,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub has_client_id: bool,
    pub has_session: bool,
}

/// Mount a registered drive as a read-only FUSE filesystem for indexing.
/// The FUSE event loop owns a dedicated thread and therefore never blocks IPC.
#[tauri::command]
pub async fn drive_mount(
    state: State<'_, AppState>,
    drive_id: String,
    mount_point: String,
) -> Result<(), String> {
    #[cfg(not(feature = "fuse"))]
    {
        let _ = (state, drive_id, mount_point);
        return Err("FUSE support is not enabled in this build".into());
    }
    #[cfg(feature = "fuse")]
    {
        let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
        let registry = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
        let config = registry.drives.iter().find(|drive| drive.id == drive_id)
            .ok_or_else(|| format!("drive '{drive_id}' not found"))?.clone();
        let mount_point = PathBuf::from(mount_point);
        if !mount_point.is_absolute() {
            return Err("FUSE mount point must be an absolute path".into());
        }
        let mounts = FUSE_MOUNTS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut active = mounts.lock().map_err(|_| "FUSE mount registry poisoned")?;
        if active.contains_key(&drive_id) {
            return Err(format!("drive '{drive_id}' is already mounted"));
        }
        let drive: std::sync::Arc<dyn super::CloudDrive> =
            std::sync::Arc::from(DriveRegistry::instantiate(&config));
        let id = drive_id.clone();
        let point = mount_point.clone();
        active.insert(drive_id.clone(), mount_point);
        drop(active);
        let spawned = std::thread::Builder::new().name(format!("fuse-{id}")).spawn(move || {
            if let Err(error) = super::fuse_mount::fs::mount_blocking(drive, &point) {
                eprintln!("FUSE mount {id} failed: {error:#}");
            }
            if let Some(mounts) = FUSE_MOUNTS.get() {
                if let Ok(mut active) = mounts.lock() { active.remove(&id); }
            }
        });
        if let Err(error) = spawned {
            if let Ok(mut active) = mounts.lock() { active.remove(&drive_id); }
            return Err(format!("starting FUSE thread: {error}"));
        }
        Ok(())
    }
}

/// Unmount a drive previously started with [`drive_mount`].
#[tauri::command]
pub async fn drive_unmount(drive_id: String) -> Result<(), String> {
    #[cfg(not(feature = "fuse"))]
    {
        let _ = drive_id;
        return Err("FUSE support is not enabled in this build".into());
    }
    #[cfg(feature = "fuse")]
    {
        let mounts = FUSE_MOUNTS.get().ok_or("drive is not mounted")?;
        let mount_point = mounts.lock().map_err(|_| "FUSE mount registry poisoned")?
            .get(&drive_id).cloned().ok_or("drive is not mounted")?;
        let path = mount_point.to_string_lossy().into_owned();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("FUSE unmount is unsupported on this platform".into());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            #[cfg(target_os = "linux")]
            let output = std::process::Command::new("fusermount3")
                .arg("-u").arg(&path).output()
                .or_else(|_| std::process::Command::new("umount").arg(&path).output());
            #[cfg(target_os = "macos")]
            let output = std::process::Command::new("umount")
                .arg(&path).output();
            let output = output.map_err(|e| format!("unmounting {path}: {e}"))?;
            if !output.status.success() {
                return Err(format!("unmounting {path} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()));
            }
        }
        Ok(())
    }
}

/// Return the process-local FUSE mounts and their lifecycle state.
#[tauri::command]
pub async fn drive_mount_status() -> Result<Vec<super::fuse_mount::FuseMountStatus>, String> {
    #[cfg(not(feature = "fuse"))]
    {
        return Err("FUSE support is not enabled in this build".into());
    }
    #[cfg(feature = "fuse")]
    {
        let mounts = FUSE_MOUNTS.get_or_init(|| Mutex::new(HashMap::new()));
        let active = mounts.lock().map_err(|_| "FUSE mount registry poisoned")?;
        Ok(active.iter().map(|(drive_id, mount_point)| super::fuse_mount::FuseMountStatus {
            drive_id: drive_id.clone(),
            mount_point: mount_point.clone(),
            active: true,
            cached_bytes: 0,
            cached_files: 0,
        }).collect())
    }
}

/// Start a public-client OAuth flow. The returned URL may be opened by the
/// system browser; tokens are exchanged by the loopback callback thread and
/// never returned through IPC.
#[tauri::command]
pub async fn drive_oauth_start(
    state: State<'_, AppState>,
    drive_id: String,
    provider: String,
    client_id: String,
) -> Result<super::oauth::StartResult, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let expected = match config.kind {
        DriveType::GoogleDrive => "google",
        DriveType::OneDrive => "microsoft",
        _ => return Err("OAuth login requires Google Drive or OneDrive".into()),
    };
    if provider != expected {
        return Err("OAuth provider does not match drive type".into());
    }
    super::oauth::start(
        drive_id,
        super::oauth::Provider::parse(&provider).map_err(|e| e.to_string())?,
        client_id,
    )
    .map_err(|e| e.to_string())
}

/// Refresh an OAuth access token in the OS keychain without returning it over IPC.
#[tauri::command]
pub async fn drive_oauth_refresh(
    state: State<'_, AppState>, drive_id: String,
) -> Result<(), String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = reg.drives.iter().find(|d| d.id == drive_id).ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let provider = match config.kind {
        DriveType::GoogleDrive => super::oauth::Provider::Google,
        DriveType::OneDrive => super::oauth::Provider::Microsoft,
        _ => return Err("OAuth refresh requires Google Drive or OneDrive".into()),
    };
    super::oauth::refresh(provider, &drive_id).map_err(|e| e.to_string())
}

/// Revoke/clear provider OAuth credentials. Microsoft is cleared locally
/// because its platform has no token-revocation API.
#[tauri::command]
pub async fn drive_oauth_revoke(
    state: State<'_, AppState>, drive_id: String,
) -> Result<(), String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = reg.drives.iter().find(|d| d.id == drive_id).ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let provider = match config.kind {
        DriveType::GoogleDrive => super::oauth::Provider::Google,
        DriveType::OneDrive => super::oauth::Provider::Microsoft,
        _ => return Err("OAuth revoke requires Google Drive or OneDrive".into()),
    };
    super::oauth::revoke(provider, &drive_id).map_err(|e| e.to_string())
}

/// List all configured drives.
#[tauri::command]
pub async fn drive_list(state: State<'_, AppState>) -> Result<Vec<DriveConfig>, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
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
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let drive_type = match kind.as_str() {
        "filen" => DriveType::Filen,
        "internxt" => DriveType::Internxt,
        "sftp" => DriveType::Sftp,
        "webdav" => DriveType::WebDav,
        "onedrive" => DriveType::OneDrive,
        "google_drive" => DriveType::GoogleDrive,
        _ => DriveType::Local,
    };
    let config = DriveConfig {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        kind: drive_type.clone(),
        path,
        username: None,
        password: None,
        insecure_tls,
        access_token: None,
        refresh_token: None,
        client_id: None,
        client_secret: None,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    reg.add(config.clone()).map_err(|e| e.to_string())?;
    if drive_type == DriveType::WebDav {
        super::secret::set_credentials(
            &config.id,
            &super::secret::DriveCredentials {
                username,
                password,
                ..Default::default()
            },
        )
        .map_err(|e| format!("storing drive credentials failed: {e:#}"))?;
    }
    Ok(config)
}

/// Remove a drive by id.  Returns `true` if it was found and removed.
#[tauri::command]
pub async fn drive_delete(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let removed = reg.remove(&id).map_err(|e| e.to_string())?;
    if removed {
        super::secret::delete_credentials(&id).map_err(|e| e.to_string())?;
        super::secret::delete_session(&id).map_err(|e| e.to_string())?;
    }
    Ok(removed)
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
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let drive_type = match kind.as_str() {
        "filen" => DriveType::Filen,
        "internxt" => DriveType::Internxt,
        "sftp" => DriveType::Sftp,
        "webdav" => DriveType::WebDav,
        "onedrive" => DriveType::OneDrive,
        "google_drive" => DriveType::GoogleDrive,
        _ => DriveType::Local,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|d| d.id == id) {
        return Err(format!("drive '{id}' not found"));
    }
    let updated = DriveConfig {
        id,
        label,
        kind: drive_type.clone(),
        path,
        username: None,
        password: None,
        insecure_tls,
        access_token: None,
        refresh_token: None,
        client_id: None,
        client_secret: None,
    };
    // `add` dedupes by id (replaces) — exactly the semantics we want.
    reg.add(updated.clone()).map_err(|e| e.to_string())?;
    if drive_type == DriveType::WebDav && (username.is_some() || password.is_some()) {
        super::secret::set_credentials(
            &updated.id,
            &super::secret::DriveCredentials {
                username,
                password,
                ..Default::default()
            },
        )
        .map_err(|e| format!("storing drive credentials failed: {e:#}"))?;
    }
    Ok(updated)
}

/// Return credential presence only. Secret values never cross the Tauri IPC boundary.
#[tauri::command]
pub async fn drive_credentials_status(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<DriveCredentialsStatus, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|drive| drive.id == drive_id) {
        return Err(format!("drive '{drive_id}' not found"));
    }
    let c = super::secret::get_credentials(&drive_id)
        .map_err(|e| format!("reading drive credential status failed: {e:#}"))?
        .unwrap_or_default();
    let has_session = super::secret::get_session(&drive_id)
        .map_err(|e| format!("reading drive session status failed: {e:#}"))?
        .is_some();
    Ok(DriveCredentialsStatus {
        has_username: c.username.is_some(),
        has_password: c.password.is_some(),
        has_access_token: c.access_token.is_some(),
        has_refresh_token: c.refresh_token.is_some(),
        has_client_id: c.client_id.is_some(),
        has_session,
    })
}

/// Disconnect a provider without deleting its non-secret drive metadata.
#[tauri::command]
pub async fn drive_disconnect(state: State<'_, AppState>, drive_id: String) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|drive| drive.id == drive_id) {
        return Err(format!("drive '{drive_id}' not found"));
    }
    super::secret::delete_credentials(&drive_id).map_err(|e| e.to_string())?;
    super::secret::delete_session(&drive_id).map_err(|e| e.to_string())
}

/// List directory entries on a drive.
#[tauri::command]
pub async fn drive_list_dir(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<Vec<super::DirEntry>, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    drive
        .list_dir(std::path::Path::new(&path))
        .map_err(|e| e.to_string())
}

/// Stat a file or directory on a drive.
#[tauri::command]
pub async fn drive_stat(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<super::FileStat, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    drive
        .stat(std::path::Path::new(&path))
        .map_err(|e| e.to_string())
}

/// Return the safe operation set for a registered drive.
#[tauri::command]
pub async fn drive_capabilities(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<super::DriveCapabilities, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    Ok(DriveRegistry::instantiate(cfg).probed_capabilities())
}

/// Create a directory on a drive when its capability set permits it.
#[tauri::command]
pub async fn drive_create_dir(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().create_dir {
        return Err(format!(
            "{} does not support create_dir",
            drive.drive_type().label()
        ));
    }
    drive
        .create_dir(std::path::Path::new(&path))
        .map_err(|e| e.to_string())
}

/// Move or rename a path within a drive.
#[tauri::command]
pub async fn drive_move_path(
    state: State<'_, AppState>,
    drive_id: String,
    source: String,
    destination: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().move_path {
        return Err(format!(
            "{} does not support move_path",
            drive.drive_type().label()
        ));
    }
    drive
        .move_path(
            std::path::Path::new(&source),
            std::path::Path::new(&destination),
        )
        .map_err(|e| e.to_string())
}

/// Copy a file or directory within a drive.
#[tauri::command]
pub async fn drive_copy_path(
    state: State<'_, AppState>,
    drive_id: String,
    source: String,
    destination: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().copy {
        return Err(format!(
            "{} does not support copy",
            drive.drive_type().label()
        ));
    }
    drive
        .copy_path(
            std::path::Path::new(&source),
            std::path::Path::new(&destination),
        )
        .map_err(|e| e.to_string())
}

/// Delete or trash a file/directory within a registered drive.
///
/// This is intentionally separate from `drive_delete`, which removes the
/// drive registration itself.
#[tauri::command]
pub async fn drive_delete_path(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().delete {
        return Err(format!("{} does not support delete", drive.drive_type().label()));
    }
    drive
        .delete(std::path::Path::new(&path))
        .map_err(|e| e.to_string())
}

/// Generate a public share link for a file on a registered drive.
/// Providers without a public-link implementation return a clear error
/// rather than silently returning an unusable local URL.
#[tauri::command]
pub async fn drive_share_link(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<String, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    drive
        .share_link(std::path::Path::new(&path))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "{} does not support public share links",
                drive.drive_type().label()
            )
        })
}

/// List provider-managed versions for a file.
#[tauri::command]
pub async fn drive_list_versions(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<Vec<super::FileVersion>, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().versions {
        return Err(format!(
            "{} does not support file versions",
            drive.drive_type().label()
        ));
    }
    drive
        .list_versions(std::path::Path::new(&path))
        .map_err(|e| e.to_string())
}

/// Restore a provider-managed file version.
#[tauri::command]
pub async fn drive_restore_version(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
    version_id: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().versions {
        return Err(format!(
            "{} does not support file version restore",
            drive.drive_type().label()
        ));
    }
    drive
        .restore_version(std::path::Path::new(&path), &version_id)
        .map_err(|e| e.to_string())
}

/// Read a file through the shared transfer queue.
#[tauri::command]
pub async fn drive_read_file(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<Vec<u8>, String> {
    use std::sync::Arc;

    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive: Arc<dyn super::CloudDrive> = Arc::from(DriveRegistry::instantiate(cfg));
    let path = std::path::PathBuf::from(path);
    let path_for_transfer = path.clone();
    let transfer = state
        .transfer_queue
        .clone()
        .submit_download(drive_id, path, None, move |_| {
            drive.read_file(&path_for_transfer)
        });
    match transfer.handle.await {
        Ok(Ok(data)) => Ok(data),
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(format!("transfer queue task failed: {error}")),
    }
}

/// Write a file through the shared transfer queue.
#[tauri::command]
pub async fn drive_write_file(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
    data: Vec<u8>,
) -> Result<(), String> {
    use std::sync::Arc;

    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive: Arc<dyn super::CloudDrive> = Arc::from(DriveRegistry::instantiate(cfg));
    let path = std::path::PathBuf::from(path);
    let retry_data = data.clone();
    let retry_path = path.clone();
    let retry_drive_id = drive_id.clone();
    let transfer =
        state
            .transfer_queue
            .clone()
            .submit_upload(drive_id, path, data, move |path, data| {
                drive.write_file(path, data)
            });
    match transfer.handle.await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            let queued = crate::sync::tauri_commands::queue_failed_drive_upload(
                &data_dir,
                &retry_drive_id,
                &retry_path,
                &retry_data,
                &error,
            );
            match queued {
                Ok(id) => Err(format!("{} (queued offline operation {id})", error)),
                Err(queue_error) => Err(format!("{} (offline queue failed: {queue_error})", error)),
            }
        }
        Err(error) => Err(format!("transfer queue task failed: {error}")),
    }
}

/// Attempt a true CrispCloud delta upload for a local file to a WebDAV
/// Nextcloud/ownCloud drive. None means the optional server app is absent and
/// the caller must use the normal full-file upload path.
#[tauri::command]
pub async fn drive_delta_upload(
    state: State<'_, AppState>,
    drive_id: String,
    local_path: String,
    remote_path: String,
) -> Result<Option<super::webdav::DeltaTransferResult>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|drive| drive.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    if cfg.kind != DriveType::WebDav {
        return Err("delta upload currently requires a WebDAV Nextcloud/ownCloud drive".into());
    }
    let credentials = super::secret::get_credentials(&drive_id)
        .map_err(|e| e.to_string())?.unwrap_or_default();
    let label = cfg.label.clone();
    let base_url = cfg.path.clone();
    let insecure_tls = cfg.insecure_tls.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        let drive = super::webdav::WebDavDrive::new(
            label, base_url, credentials.username, credentials.password, insecure_tls,
        );
        drive.delta_upload_file(
            std::path::Path::new(&local_path),
            std::path::Path::new(&remote_path),
        ).map_err(|e| e.to_string())
    }).await.map_err(|e| format!("delta upload task failed: {e}"))?
}

/// Attempt a true CrispCloud delta download into an existing local file.
#[tauri::command]
pub async fn drive_delta_download(
    state: State<'_, AppState>,
    drive_id: String,
    remote_path: String,
    local_path: String,
) -> Result<Option<super::webdav::DeltaTransferResult>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|drive| drive.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    if cfg.kind != DriveType::WebDav {
        return Err("delta download currently requires a WebDAV Nextcloud/ownCloud drive".into());
    }
    let credentials = super::secret::get_credentials(&drive_id)
        .map_err(|e| e.to_string())?.unwrap_or_default();
    let label = cfg.label.clone();
    let base_url = cfg.path.clone();
    let insecure_tls = cfg.insecure_tls.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        let drive = super::webdav::WebDavDrive::new(
            label, base_url, credentials.username, credentials.password, insecure_tls,
        );
        drive.delta_download_file(
            std::path::Path::new(&remote_path),
            std::path::Path::new(&local_path),
        ).map_err(|e| e.to_string())
    }).await.map_err(|e| format!("delta download task failed: {e}"))?
}

/// Resume a native-provider upload from its durable provider state file.
/// Providers without a stable encryption/session resume contract fail before
/// any bytes are read.
#[tauri::command]
pub async fn drive_upload_resumable(
    state: State<'_, AppState>,
    drive_id: String,
    local_path: String,
    remote_path: String,
    state_path: String,
    workers: Option<usize>,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?
        .clone();
    let drive = DriveRegistry::instantiate(&cfg);
    let workers = workers.unwrap_or(1).clamp(1, 10);
    tokio::task::spawn_blocking(move || {
        drive.upload_file_resumable(
            std::path::Path::new(&local_path),
            std::path::Path::new(&remote_path),
            std::path::Path::new(&state_path),
            workers,
        )
    })
    .await
    .map_err(|e| format!("resumable upload task failed: {e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn drive_download_resumable(
    state: State<'_, AppState>,
    drive_id: String,
    remote_path: String,
    local_path: String,
    state_path: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg
        .drives
        .iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?
        .clone();
    let drive = DriveRegistry::instantiate(&cfg);
    tokio::task::spawn_blocking(move || {
        drive.download_file_resumable(
            std::path::Path::new(&remote_path),
            std::path::Path::new(&local_path),
            std::path::Path::new(&state_path),
        )
    })
    .await
    .map_err(|e| format!("resumable download task failed: {e}"))?
    .map_err(|e| e.to_string())
}

/// Log a native Internxt drive in without persisting the password or
/// mnemonic. The resulting session is stored under the registered drive id
/// in the OS keychain. This command is available in all builds so the UI can
/// report a clear feature error when native support is not compiled in.
#[tauri::command]
pub async fn drive_native_login(
    state: State<'_, AppState>,
    drive_id: String,
    email: String,
    password: String,
    tfa_code: Option<String>,
    drive_api_url: Option<String>,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = reg
        .drives
        .iter()
        .find(|drive| drive.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    if config.kind != DriveType::Internxt {
        return Err("native Internxt login requires an Internxt drive".to_owned());
    }

    #[cfg(feature = "drive-internxt-native")]
    {
        let api_url = drive_api_url
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| super::internxt_native::DEFAULT_DRIVE_API_URL.to_owned());
        let session = super::internxt_native::InternxtNativeClient::login_without_keys(
            &api_url,
            &email,
            &password,
            tfa_code.as_deref(),
        )
        .map_err(|error| format!("native Internxt login failed: {error:#}"))?;
        let serialized = session
            .encode()
            .map_err(|error| format!("serializing native Internxt session failed: {error:#}"))?;
        super::secret::set_session(&drive_id, &serialized)
            .map_err(|error| format!("storing native Internxt session failed: {error:#}"))?;
        Ok(())
    }
    #[cfg(not(feature = "drive-internxt-native"))]
    {
        let _ = (email, password, tfa_code, drive_api_url);
        Err("native Internxt support is not enabled in this build".to_owned())
    }
}

/// Log in a native Filen drive and store its encrypted session in the OS
/// keychain.  The password is never written to DriveConfig/drives.json.
#[tauri::command]
pub async fn drive_filen_native_login(
    state: State<'_, AppState>,
    drive_id: String,
    email: String,
    password: String,
    tfa_code: Option<String>,
    gateway_url: Option<String>,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = reg
        .drives
        .iter()
        .find(|drive| drive.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    if config.kind != DriveType::Filen {
        return Err("native Filen login requires a Filen drive".to_owned());
    }
    #[cfg(feature = "drive-filen-native")]
    {
        let url = gateway_url
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| crisp_filen::DEFAULT_GATEWAY_URL.to_owned());
        let session =
            crisp_filen::FilenNativeClient::login(&url, &email, &password, tfa_code.as_deref())
                .map_err(|e| format!("native Filen login failed: {e:#}"))?;
        let serialized = session
            .encode()
            .map_err(|e| format!("serializing native Filen session failed: {e:#}"))?;
        super::secret::set_session(&drive_id, &serialized)
            .map_err(|e| format!("storing native Filen session failed: {e:#}"))
    }
    #[cfg(not(feature = "drive-filen-native"))]
    {
        let _ = (email, password, tfa_code, gateway_url);
        Err("native Filen support is not enabled in this build".to_owned())
    }
}

/// Remove the native Internxt session from the OS keychain.
#[tauri::command]
pub async fn drive_native_logout(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|drive| drive.id == drive_id) {
        return Err(format!("drive '{drive_id}' not found"));
    }
    super::secret::delete_session(&drive_id).map_err(|error| error.to_string())
}

/// Refresh the bearer tokens for a keychain-backed native Internxt session.
#[tauri::command]
pub async fn drive_native_refresh(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<(), String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = reg
        .drives
        .iter()
        .find(|drive| drive.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    if config.kind != DriveType::Internxt {
        return Err("native Internxt refresh requires an Internxt drive".to_owned());
    }

    #[cfg(feature = "drive-internxt-native")]
    {
        let serialized = super::secret::get_session(&drive_id)
            .map_err(|error| format!("reading native Internxt session failed: {error:#}"))?
            .ok_or_else(|| "no native Internxt session is stored".to_owned())?;
        let session = super::internxt_native::InternxtSession::decode(&serialized)
            .map_err(|error| format!("parsing native Internxt session failed: {error:#}"))?;
        let client = super::internxt_native::InternxtNativeClient::new(
            &session.drive_api_url,
            session.active_token(),
        )
        .map_err(|error| format!("creating native Internxt client failed: {error:#}"))?;
        let refreshed = client
            .refresh_session(&session)
            .map_err(|error| format!("refreshing native Internxt session failed: {error:#}"))?;
        let serialized = refreshed
            .encode()
            .map_err(|error| format!("serializing native Internxt session failed: {error:#}"))?;
        super::secret::set_session(&drive_id, &serialized)
            .map_err(|error| format!("storing native Internxt session failed: {error:#}"))
    }
    #[cfg(not(feature = "drive-internxt-native"))]
    {
        Err("native Internxt support is not enabled in this build".to_owned())
    }
}
