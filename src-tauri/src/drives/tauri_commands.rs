//! Tauri commands for the cloud drive registry (P11 Pillar 5).

use super::{DriveConfig, DriveRegistry, DriveType};
use crate::AppState;
use tauri::State;

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
        _ => DriveType::Local,
    };
    let config = DriveConfig {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        kind: drive_type,
        path,
        username,
        password,
        insecure_tls,
        access_token: None,
        refresh_token: None,
        client_id: None,
        client_secret: None,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    reg.add(config.clone()).map_err(|e| e.to_string())?;
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
        _ => DriveType::Local,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|d| d.id == id) {
        return Err(format!("drive '{id}' not found"));
    }
    let updated = DriveConfig {
        id,
        label,
        kind: drive_type,
        path,
        username,
        password,
        insecure_tls,
        access_token: None,
        refresh_token: None,
        client_id: None,
        client_secret: None,
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
    Ok(DriveRegistry::instantiate(cfg).capabilities())
}

/// Create a directory on a drive when its capability set permits it.
#[tauri::command]
pub async fn drive_create_dir(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<(), String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().create_dir {
        return Err(format!("{} does not support create_dir", drive.drive_type().label()));
    }
    drive.create_dir(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

/// Move or rename a path within a drive.
#[tauri::command]
pub async fn drive_move_path(
    state: State<'_, AppState>,
    drive_id: String,
    source: String,
    destination: String,
) -> Result<(), String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().move_path {
        return Err(format!("{} does not support move_path", drive.drive_type().label()));
    }
    drive.move_path(std::path::Path::new(&source), std::path::Path::new(&destination))
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
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let cfg = reg.drives.iter().find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?;
    let drive = DriveRegistry::instantiate(cfg);
    if !drive.capabilities().copy {
        return Err(format!("{} does not support copy", drive.drive_type().label()));
    }
    drive.copy_path(std::path::Path::new(&source), std::path::Path::new(&destination))
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
        .ok_or_else(|| format!("{} does not support public share links", drive.drive_type().label()))
}

/// Read a file through the shared transfer queue.
#[tauri::command]
pub async fn drive_read_file(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<Vec<u8>, String> {
    use crate::sync::transfer_queue::TransferQueue;
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
    let transfer = TransferQueue::new().submit_download(
        drive_id,
        path,
        None,
        move |_| drive.read_file(&path_for_transfer),
    );
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
    use crate::sync::transfer_queue::TransferQueue;
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
    let transfer = TransferQueue::new().submit_upload(drive_id, path, data, move |path, data| {
        drive.write_file(path, data)
    });
    match transfer.handle.await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(format!("transfer queue task failed: {error}")),
    }
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
        let session = crisp_filen::FilenNativeClient::login(
            &url,
            &email,
            &password,
            tfa_code.as_deref(),
        )
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
