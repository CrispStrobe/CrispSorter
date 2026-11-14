//! Tauri commands for the cloud drive registry (P11 Pillar 5).

use super::{DriveConfig, DriveRegistry, DriveType};
use crate::operation_log::OperationLog;
use crate::AppState;
use serde_json::{json, Value};
use crate::sync::conflict::ConflictPolicy;
use serde::Serialize;
#[cfg(feature = "fuse")]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(feature = "fuse")]
use std::sync::{Mutex, OnceLock};
use tauri::State;

#[cfg(feature = "fuse")]
static FUSE_MOUNTS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

/// Reserve a mount only after async provider setup has finished. Checking and
/// inserting under one lock prevents concurrent requests from mounting twice.
#[cfg(feature = "fuse")]
fn reserve_fuse_mount(
    mounts: &Mutex<HashMap<String, PathBuf>>,
    drive_id: &str,
    mount_point: PathBuf,
) -> Result<(), String> {
    let mut active = mounts.lock().map_err(|_| "FUSE mount registry poisoned")?;
    match active.entry(drive_id.to_owned()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(mount_point);
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(_) => {
            Err(format!("drive '{drive_id}' is already mounted"))
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DriveCredentialsStatus {
    pub has_username: bool,
    pub has_password: bool,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub has_client_id: bool,
    pub has_session: bool,
}

async fn configured_proxy(state: &AppState) -> Result<crate::sync::proxy::ProxyConfig, String> {
    let index_config = state.index.lock().await.config.clone();
    let password = tokio::task::spawn_blocking(crate::sync::proxy_secret::get)
        .await
        .map_err(|_| "Proxy credential worker failed".to_owned())?
        .map_err(|e| e.to_string())?;
    Ok(crate::sync::proxy::ProxyConfig {
        url: index_config.proxy_url,
        username: index_config.proxy_username,
        password,
    })
}

async fn instantiate_registered(
    state: &AppState,
    config: &DriveConfig,
) -> Result<Box<dyn super::CloudDrive>, String> {
    let proxy = configured_proxy(state).await?;
    DriveRegistry::instantiate_with_proxy(config, &proxy).map_err(|e| e.to_string())
}

/// Closed provider identifier for operation receipts. Labels, URLs, paths and
/// account names never enter the app log; only this enum value does.
pub(crate) fn backend_label(kind: &DriveType) -> &'static str {
    match kind {
        DriveType::Local => "local",
        DriveType::Smb => "smb",
        DriveType::Filen => "filen",
        DriveType::Internxt => "internxt",
        DriveType::Sftp => "sftp",
        DriveType::WebDav => "webdav",
        DriveType::OneDrive => "onedrive",
        DriveType::GoogleDrive => "google_drive",
    }
}

fn drive_type_from_kind(kind: &str) -> DriveType {
    match kind {
        "filen" => DriveType::Filen,
        "internxt" => DriveType::Internxt,
        "sftp" => DriveType::Sftp,
        "smb" => DriveType::Smb,
        "webdav" => DriveType::WebDav,
        "onedrive" => DriveType::OneDrive,
        "google_drive" => DriveType::GoogleDrive,
        _ => DriveType::Local,
    }
}

/// Record exactly one terminal receipt for a command result. The error string
/// is returned unchanged to the caller; only the fixed `code` is logged.
pub(crate) fn finish_operation<T>(
    log: &OperationLog,
    result: &Result<T, String>,
    code: &'static str,
    receipt: impl FnOnce(&T) -> Value,
) {
    match result {
        Ok(value) => log.succeed(receipt(value)),
        Err(_) => log.fail(code),
    }
}

async fn registered_config(state: &AppState, drive_id: &str) -> Result<DriveConfig, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_owned())?;
    let reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    reg.drives
        .into_iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))
}

/// Resolve a registered drive once and start its receipt with the provider
/// kind. A failed lookup is itself a visible, content-free failure.
async fn drive_operation(
    state: &AppState,
    operation: &'static str,
    drive_id: &str,
) -> Result<(OperationLog, DriveConfig), String> {
    match registered_config(state, drive_id).await {
        Ok(config) => {
            let log = OperationLog::start(operation, json!({"backend": backend_label(&config.kind)}));
            Ok((log, config))
        }
        Err(error) => {
            let log = OperationLog::start(operation, Value::Null);
            log.fail("drive_lookup_failed");
            Err(error)
        }
    }
}

/// Decide what a file-manager mutation should do when its destination exists.
/// The source is the incoming version for copy/move purposes; this makes the
/// persisted sync policy explicit instead of allowing a provider's overwrite
/// default to decide silently.
#[derive(Debug, PartialEq, Eq)]
enum MutationResolution {
    /// Proceed with the supplied destination.
    UseDestination,
    /// Preserve the existing destination and report a successful no-op.
    Skip,
    /// Keep both by using this new destination path.
    KeepBoth(PathBuf),
}

fn mutation_destination(
    source_mtime: Option<i64>,
    destination_mtime: Option<i64>,
    destination: &Path,
    policy: ConflictPolicy,
) -> Result<MutationResolution, String> {
    match policy {
        ConflictPolicy::RemoteWins => Ok(MutationResolution::UseDestination),
        ConflictPolicy::LocalWins => Ok(MutationResolution::Skip),
        ConflictPolicy::Manual => Err(
            "destination already exists; manual conflict review is required before mutation"
                .to_owned(),
        ),
        ConflictPolicy::NewestWins => {
            let source_is_newer = match (source_mtime, destination_mtime) {
                (Some(source), Some(existing)) if source != 0 && existing != 0 => source > existing,
                // Missing metadata is not evidence that the destination is
                // older. Preserve it until an explicit policy resolves this.
                _ => false,
            };
            Ok(if source_is_newer {
                MutationResolution::UseDestination
            } else {
                MutationResolution::Skip
            })
        }
        ConflictPolicy::KeepBoth => {
            let name = destination
                .file_name()
                .ok_or_else(|| "destination has no filename for keep-both".to_owned())?
                .to_string_lossy();
            let (stem, extension) = match destination.extension() {
                Some(extension) => (
                    name.trim_end_matches(&format!(".{}", extension.to_string_lossy()))
                        .to_owned(),
                    format!(".{}", extension.to_string_lossy()),
                ),
                None => (name.into_owned(), String::new()),
            };
            Ok(MutationResolution::KeepBoth(
                destination
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(format!("{stem} (copy){extension}")),
            ))
        }
    }
}

async fn configured_conflict_policy(state: &AppState) -> ConflictPolicy {
    state.index.lock().await.config.conflict_policy
}

/// Preserve typed local lookup failures rather than authorizing an overwrite.
/// Remote providers do not expose portable absence errors. Keep their legacy
/// behavior here; transport-level io errors cannot identify a missing path.
fn mutation_path_exists(drive: &dyn super::CloudDrive, path: &Path) -> Result<bool, String> {
    match drive.stat(path) {
        Ok(_) => Ok(true),
        Err(error) => {
            if drive.drive_type() == DriveType::Local {
                if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
                    if io_error.kind() != std::io::ErrorKind::NotFound {
                        return Err(error.to_string());
                    }
                }
            }
            Ok(false)
        }
    }
}

/// Resolve an existing destination and return the effective mutation path.
/// Local typed NotFound and ambiguous provider errors retain legacy behavior.
/// This does not protect against dangling symlinks or changes after lookup.
fn resolve_mutation_path(
    drive: &dyn super::CloudDrive,
    source: &Path,
    destination: &Path,
    policy: ConflictPolicy,
) -> Result<Option<PathBuf>, String> {
    if source == destination || !mutation_path_exists(drive, destination)? {
        return Ok(Some(destination.to_owned()));
    }
    let source_mtime = drive.stat(source).map_err(|e| e.to_string())?.mtime_unix;
    let destination_mtime = drive
        .stat(destination)
        .map_err(|e| e.to_string())?
        .mtime_unix;
    match mutation_destination(source_mtime, destination_mtime, destination, policy)? {
        MutationResolution::UseDestination => Ok(Some(destination.to_owned())),
        MutationResolution::Skip => Ok(None),
        MutationResolution::KeepBoth(mut candidate) => {
            for index in 2..=1000 {
                if !mutation_path_exists(drive, &candidate)? {
                    return Ok(Some(candidate));
                }
                let name = candidate
                    .file_name()
                    .ok_or_else(|| "keep-both candidate has no filename".to_owned())?
                    .to_string_lossy();
                candidate.set_file_name(format!("{name} {index}"));
            }
            Err("could not find an unused keep-both destination".to_owned())
        }
    }
}

/// Mount a registered drive as a read-only FUSE filesystem for indexing.
/// The FUSE event loop owns a dedicated thread and therefore never blocks IPC.
#[tauri::command]
pub async fn drive_mount(
    state: State<'_, AppState>,
    drive_id: String,
    mount_point: String,
    cache_max_bytes: Option<u64>,
) -> Result<(), String> {
    // Covers setup up to the FUSE thread start; the mount worker itself
    // records `storage.fuse_mount` with the kernel-mount outcome.
    let log = OperationLog::start("storage.drive.mount", json!({"surface": "native"}));
    let result = drive_mount_inner(&state, drive_id, mount_point, cache_max_bytes).await;
    finish_operation(&log, &result, "drive_mount_failed", |_| Value::Null);
    result
}

async fn drive_mount_inner(
    state: &AppState,
    drive_id: String,
    mount_point: String,
    cache_max_bytes: Option<u64>,
) -> Result<(), String> {
    #[cfg(not(feature = "fuse"))]
    {
        let _ = (state, drive_id, mount_point, cache_max_bytes);
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
        // Provider setup awaits app state; no standard mutex guard may cross
        // that boundary in this Send command future.
        let drive: std::sync::Arc<dyn super::CloudDrive> =
            std::sync::Arc::from(instantiate_registered(&state, &config).await?);
        let id = drive_id.clone();
        let point = mount_point.clone();
        let cache_max_bytes = cache_max_bytes.unwrap_or(super::fuse_mount::fs::DEFAULT_CACHE_MAX_BYTES);
        let mounts = FUSE_MOUNTS.get_or_init(|| Mutex::new(HashMap::new()));
        reserve_fuse_mount(mounts, &drive_id, mount_point)?;
        let spawned = std::thread::Builder::new().name(format!("fuse-{id}")).spawn(move || {
            // The worker emits content-free lifecycle receipts to the app log.
            // Thread creation alone does not mean that the kernel mount worked.
            let _ = super::fuse_mount::fs::mount_blocking_with_cache(drive, &point, cache_max_bytes);
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
    let log = OperationLog::start("storage.drive.unmount", json!({"surface": "native"}));
    let result = drive_unmount_inner(drive_id).await;
    finish_operation(&log, &result, "drive_unmount_failed", |_| Value::Null);
    result
}

async fn drive_unmount_inner(drive_id: String) -> Result<(), String> {
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
        // Unmounting is `fusermount3` / `umount` and nothing else — there is
        // no in-process equivalent. That is fine: FUSE cannot work inside an
        // App Sandbox regardless (it needs a kernel extension and a mount
        // the container cannot see), so a build without `sidecars` has no
        // mounts to unmount either. PLAN P36.2 records this as dropped
        // rather than ported.
        #[cfg(not(all(feature = "sidecars", any(target_os = "linux", target_os = "macos"))))]
        return Err("FUSE unmount is unsupported on this platform".into());
        #[cfg(all(feature = "sidecars", any(target_os = "linux", target_os = "macos")))]
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
    let (log, config) = drive_operation(&state, "storage.drive.oauth_start", &drive_id).await?;
    let result = async {
        let expected = match config.kind {
            DriveType::GoogleDrive => "google",
            DriveType::OneDrive => "microsoft",
            _ => return Err("OAuth login requires Google Drive or OneDrive".into()),
        };
        if provider != expected {
            return Err("OAuth provider does not match drive type".into());
        }
        let proxy = configured_proxy(&state).await?;
        let provider = super::oauth::Provider::parse(&provider).map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || {
            super::oauth::start_with_proxy(drive_id, provider, client_id, &proxy)
        })
        .await
        .map_err(|_| "OAuth start worker failed".to_owned())?
        .map_err(|e| e.to_string())
    }
    .await;
    // Completion here means the loopback listener is waiting; the token
    // exchange records its own `storage.drive.oauth_callback` receipt.
    finish_operation(&log, &result, "oauth_start_failed", |_| Value::Null);
    result
}

/// Refresh an OAuth access token in the OS keychain without returning it over IPC.
#[tauri::command]
pub async fn drive_oauth_refresh(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<(), String> {
    let (log, config) = drive_operation(&state, "storage.drive.oauth_refresh", &drive_id).await?;
    let result = async {
        let provider = match config.kind {
            DriveType::GoogleDrive => super::oauth::Provider::Google,
            DriveType::OneDrive => super::oauth::Provider::Microsoft,
            _ => return Err("OAuth refresh requires Google Drive or OneDrive".into()),
        };
        let proxy = configured_proxy(&state).await?;
        tokio::task::spawn_blocking(move || {
            super::oauth::refresh_with_proxy(provider, &drive_id, &proxy)
        })
        .await
        .map_err(|_| "OAuth refresh worker failed".to_owned())?
        .map_err(|e| e.to_string())
    }
    .await;
    finish_operation(&log, &result, "oauth_refresh_failed", |_| Value::Null);
    result
}

/// Revoke/clear provider OAuth credentials. Microsoft is cleared locally
/// because its platform has no token-revocation API.
#[tauri::command]
pub async fn drive_oauth_revoke(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<(), String> {
    let (log, config) = drive_operation(&state, "storage.drive.oauth_revoke", &drive_id).await?;
    let result = async {
        let provider = match config.kind {
            DriveType::GoogleDrive => super::oauth::Provider::Google,
            DriveType::OneDrive => super::oauth::Provider::Microsoft,
            _ => return Err("OAuth revoke requires Google Drive or OneDrive".into()),
        };
        let proxy = if provider == super::oauth::Provider::Google {
            configured_proxy(&state).await?
        } else {
            crate::sync::proxy::ProxyConfig::default()
        };
        tokio::task::spawn_blocking(move || {
            super::oauth::revoke_with_proxy(provider, &drive_id, &proxy)
        })
        .await
        .map_err(|_| "OAuth revoke worker failed".to_owned())?
        .map_err(|e| e.to_string())
    }
    .await;
    finish_operation(&log, &result, "oauth_revoke_failed", |_| Value::Null);
    result
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
    smb: Option<crisp_smb_source::Settings>,
) -> Result<DriveConfig, String> {
    let drive_type = drive_type_from_kind(&kind);
    let log = OperationLog::start("storage.drive.create",
        json!({"backend": backend_label(&drive_type)}));
    let result = drive_create_inner(&state, drive_type, label, path, username, password, insecure_tls, smb, &log).await;
    finish_operation(&log, &result, "drive_create_failed", |_| Value::Null);
    result
}

#[allow(clippy::too_many_arguments)]
async fn drive_create_inner(
    state: &AppState,
    drive_type: DriveType,
    label: String,
    path: String,
    username: Option<String>,
    password: Option<String>,
    insecure_tls: Option<bool>,
    smb: Option<crisp_smb_source::Settings>,
    log: &OperationLog,
) -> Result<DriveConfig, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let config = DriveConfig {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        kind: drive_type.clone(),
        path,
        username: None,
        password: None,
        insecure_tls,
        smb,
        access_token: None,
        refresh_token: None,
        client_id: None,
        client_secret: None,
    };
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if drive_type == DriveType::Smb {
        log.step("smb_settings", Value::Null);
        return reg.save_smb(config, username, password).map_err(|e| e.to_string());
    }
    log.step("registry", Value::Null);
    reg.add(config.clone()).map_err(|e| e.to_string())?;
    if drive_type == DriveType::WebDav {
        log.step("credentials", Value::Null);
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
    let log = OperationLog::start("storage.drive.remove", Value::Null);
    let result = drive_delete_inner(&state, id, &log).await;
    finish_operation(&log, &result, "drive_remove_failed", |removed| json!({"count": u64::from(*removed)}));
    result
}

async fn drive_delete_inner(state: &AppState, id: String, log: &OperationLog) -> Result<bool, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if let Some(config) = reg.drives.iter().find(|d| d.id == id) {
        log.step("registry", json!({"backend": backend_label(&config.kind)}));
    }
    let removed = reg.remove(&id).map_err(|e| e.to_string())?;
    if removed {
        log.step("credentials", Value::Null);
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
    smb: Option<crisp_smb_source::Settings>,
    expected: Option<DriveConfig>,
) -> Result<DriveConfig, String> {
    let drive_type = drive_type_from_kind(&kind);
    let log = OperationLog::start("storage.drive.update",
        json!({"backend": backend_label(&drive_type)}));
    let result = drive_update_inner(
        &state, id, label, drive_type, path, username, password, insecure_tls, smb, expected, &log,
    ).await;
    finish_operation(&log, &result, "drive_update_failed", |_| Value::Null);
    result
}

#[allow(clippy::too_many_arguments)]
async fn drive_update_inner(
    state: &AppState,
    id: String,
    label: String,
    drive_type: DriveType,
    path: String,
    username: Option<String>,
    password: Option<String>,
    insecure_tls: Option<bool>,
    smb: Option<crisp_smb_source::Settings>,
    expected: Option<DriveConfig>,
    log: &OperationLog,
) -> Result<DriveConfig, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let mut reg = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    if !reg.drives.iter().any(|d| d.id == id) {
        return Err(format!("drive '{id}' not found"));
    }
    if let Some(snapshot) = expected {
        if snapshot.id != id {
            return Err("Drive snapshot does not match".into());
        }
        reg.require_snapshot(&snapshot).map_err(|e| e.to_string())?;
    } else if drive_type == DriveType::Smb {
        return Err("Reload the SMB drive before saving its settings".into());
    }
    let updated = DriveConfig {
        id,
        label,
        kind: drive_type.clone(),
        path,
        username: None,
        password: None,
        insecure_tls,
        smb,
        access_token: None,
        refresh_token: None,
        client_id: None,
        client_secret: None,
    };
    // `add` dedupes by id (replaces) — exactly the semantics we want.
    if drive_type == DriveType::Smb {
        log.step("smb_settings", Value::Null);
        return reg.save_smb(updated, username, password).map_err(|e| e.to_string());
    }
    log.step("registry", Value::Null);
    reg.add(updated.clone()).map_err(|e| e.to_string())?;
    if drive_type == DriveType::WebDav && (username.is_some() || password.is_some()) {
        log.step("credentials", Value::Null);
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
    let (log, _config) = drive_operation(&state, "storage.drive.disconnect", &drive_id).await?;
    let result = super::secret::delete_credentials(&drive_id)
        .map_err(|e| e.to_string())
        .and_then(|()| super::secret::delete_session(&drive_id).map_err(|e| e.to_string()));
    finish_operation(&log, &result, "drive_disconnect_failed", |_| Value::Null);
    result
}

/// List directory entries on a drive.
#[tauri::command]
pub async fn drive_list_dir(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<Vec<super::DirEntry>, String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.list", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
        log.step("list", Value::Null);
        drive
            .list_dir(std::path::Path::new(&path))
            .map_err(|e| e.to_string())
    }
    .await;
    finish_operation(&log, &result, "drive_list_failed", |entries| json!({"items": entries.len()}));
    result
}

fn drive_search_path_matches(path: &Path, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_lowercase().contains(query))
        .unwrap_or(false)
}

/// Search provider-visible paths by filename. This deliberately does not
/// claim remote full-text support: providers expose listing, not content
/// indexing. Results are bounded to keep a slow/large drive responsive.
#[tauri::command]
pub async fn drive_search(
    state: State<'_, AppState>,
    drive_id: String,
    root: String,
    query: String,
    max_depth: Option<usize>,
    max_results: Option<usize>,
    content: Option<bool>,
) -> Result<Vec<super::WalkEntry>, String> {
    drive_search_report(state, drive_id, root, query, max_depth, max_results, content)
        .await
        .map(|report| report.results)
}

/// Search metadata is additive: the original `drive_search` array contract
/// remains available to existing IPC consumers.
#[derive(Serialize)]
pub struct DriveSearchReport {
    pub results: Vec<super::WalkEntry>,
    pub scanned_entries: usize,
    pub scan_limit: usize,
    pub scan_limit_reached: bool,
    /// The result cap was filled; more matches may exist. Determining whether
    /// another content match exists would require additional provider reads.
    pub results_limit_reached: bool,
}

#[tauri::command]
pub async fn drive_search_report(
    state: State<'_, AppState>,
    drive_id: String,
    root: String,
    query: String,
    max_depth: Option<usize>,
    max_results: Option<usize>,
    content: Option<bool>,
) -> Result<DriveSearchReport, String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(DriveSearchReport {
            results: Vec::new(), scanned_entries: 0,
            scan_limit: max_results.unwrap_or(100).clamp(1, 1000).saturating_mul(100),
            scan_limit_reached: false, results_limit_reached: false,
        });
    }
    let (log, cfg) = drive_operation(&state, "storage.drive.search", &drive_id).await?;
    let content = content.unwrap_or(false);
    // The query and matched names stay out of the log; only counts do.
    let result = drive_search_inner(&state, &cfg, root, query, max_depth, max_results, content, &log).await;
    finish_operation(&log, &result, "drive_search_failed", |report| json!({
        "results": report.results.len(),
        "mode": if content { "search.content" } else { "search.name" },
    }));
    result
}

#[allow(clippy::too_many_arguments)]
async fn drive_search_inner(
    state: &AppState,
    cfg: &DriveConfig,
    root: String,
    query: String,
    max_depth: Option<usize>,
    max_results: Option<usize>,
    content: bool,
    log: &OperationLog,
) -> Result<DriveSearchReport, String> {
    let drive = instantiate_registered(state, cfg).await?;
    let capabilities = drive.capabilities();
    if !capabilities.list {
        return Err(format!("{} does not support listing", drive.drive_type().label()));
    }
    if content && !capabilities.read {
        return Err(format!("{} does not support content reads", drive.drive_type().label()));
    }
    let mut skipped = 0u64;
    let mut errors = |_path: &Path, _error: anyhow::Error| { skipped += 1; };
    let depth = max_depth.unwrap_or(8).min(32);
    let limit = max_results.unwrap_or(100).clamp(1, 1000);
    let scan_limit = limit.saturating_mul(100).max(limit);
    let scan = super::walk_with_scan_budget(&*drive, Path::new(&root), Some(depth), Some(scan_limit), &mut errors);
    log.step("walked", json!({
        "items": scan.entries.len(), "warnings": skipped,
        "scanned_entries": scan.scanned_entries, "scan_limit": scan_limit,
        "scan_limit_reached": scan.limit_reached,
    }));
    let results: Vec<_> = scan.entries
        .into_iter()
        .filter(|entry| {
            if drive_search_path_matches(&entry.path, &query) {
                return true;
            }
            if !content || entry.is_dir || entry.size.unwrap_or(u64::MAX) > super::MAX_CONTENT_SEARCH_BYTES {
                return false;
            }
            super::read_file_bounded(drive.as_ref(), &entry.path, super::MAX_CONTENT_SEARCH_BYTES)
                .ok()
                .map(|bytes| super::content_search_matches(&bytes, &query))
                .unwrap_or(false)
        })
        .take(limit)
        .collect();
    // Keep the existing short-circuiting content search: probing for one more
    // match could read every remaining scanned file when this cap is exact.
    let results_limit_reached = results.len() == limit;
    Ok(DriveSearchReport {
        results, scanned_entries: scan.scanned_entries, scan_limit,
        scan_limit_reached: scan.limit_reached, results_limit_reached,
    })
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
    let drive = instantiate_registered(&state, cfg).await?;
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
    Ok(instantiate_registered(&state, cfg).await?.probed_capabilities())
}

/// Create a directory on a drive when its capability set permits it.
#[tauri::command]
pub async fn drive_create_dir(
    state: State<'_, AppState>,
    drive_id: String,
    path: String,
) -> Result<(), String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.mkdir", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
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
    .await;
    finish_operation(&log, &result, "drive_mkdir_failed", |_| Value::Null);
    result
}

/// Shared move/copy receipt: which conflict resolution was applied, never
/// the source or destination names.
fn mutation_mode(applied: Option<&Path>, requested: &Path) -> &'static str {
    match applied {
        None => "skip",
        Some(path) if path == requested => "drives.apply",
        Some(_) => "drives.keep_both",
    }
}

/// Move or rename a path within a drive.
#[tauri::command]
pub async fn drive_move_path(
    state: State<'_, AppState>,
    drive_id: String,
    source: String,
    destination: String,
    conflict_policy: Option<ConflictPolicy>,
) -> Result<(), String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.move", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.capabilities().move_path {
            return Err(format!(
                "{} does not support move_path",
                drive.drive_type().label()
            ));
        }
        let source = Path::new(&source);
        let destination = Path::new(&destination);
        let policy = conflict_policy.unwrap_or(configured_conflict_policy(&state).await);
        let applied = resolve_mutation_path(&*drive, source, destination, policy)?;
        let mode = mutation_mode(applied.as_deref(), destination);
        if let Some(destination) = applied {
            drive
                .move_path(source, &destination)
                .map_err(|e| e.to_string())?;
        }
        Ok(mode)
    }
    .await;
    finish_operation(&log, &result, "drive_move_failed", |mode| json!({"mode": mode}));
    result.map(|_| ())
}

/// Copy a file or directory within a drive.
#[tauri::command]
pub async fn drive_copy_path(
    state: State<'_, AppState>,
    drive_id: String,
    source: String,
    destination: String,
    conflict_policy: Option<ConflictPolicy>,
) -> Result<(), String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.copy", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.capabilities().copy {
            return Err(format!(
                "{} does not support copy",
                drive.drive_type().label()
            ));
        }
        let source = Path::new(&source);
        let destination = Path::new(&destination);
        let policy = conflict_policy.unwrap_or(configured_conflict_policy(&state).await);
        let applied = resolve_mutation_path(&*drive, source, destination, policy)?;
        let mode = mutation_mode(applied.as_deref(), destination);
        if let Some(destination) = applied {
            drive
                .copy_path(source, &destination)
                .map_err(|e| e.to_string())?;
        }
        Ok(mode)
    }
    .await;
    finish_operation(&log, &result, "drive_copy_failed", |mode| json!({"mode": mode}));
    result.map(|_| ())
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
    let (log, cfg) = drive_operation(&state, "storage.drive.delete", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.capabilities().delete {
            return Err(format!("{} does not support delete", drive.drive_type().label()));
        }
        drive
            .delete(std::path::Path::new(&path))
            .map_err(|e| e.to_string())
    }
    .await;
    finish_operation(&log, &result, "drive_delete_failed", |_| Value::Null);
    result
}

async fn trash_receipt_store(state: &AppState) -> Result<super::trash_receipts::ReceiptStore, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    super::trash_receipts::ReceiptStore::open(&data_dir).map_err(|e| e.to_string())
}

/// Duplicate trash writes the private identity journal before any remote mutation.
#[tauri::command]
pub async fn drive_trash_with_receipt(state: State<'_, AppState>, drive_id: String, group_id: String, path: String)
    -> Result<super::trash_receipts::ReceiptSummary, String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.receipt_trash", &drive_id).await?;
    let result = async {
        let store = trash_receipt_store(&state).await?;
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.capabilities().trash_receipts { return Err("Provider does not support durable object-bound trash".into()); }
        let provider = drive.receipt_trash().ok_or("Provider has no object-bound trash adapter")?;
        store.trash(provider, &drive_id, &group_id, Path::new(&path)).map_err(|e| e.to_string())
    }.await;
    finish_operation(&log, &result, "receipt_trash_failed", |_| Value::Null);
    result
}

#[tauri::command]
pub async fn drive_trash_receipts(state: State<'_, AppState>, group_id: String)
    -> Result<Vec<super::trash_receipts::ReceiptSummary>, String> {
    trash_receipt_store(&state).await?.list(&group_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn drive_restore_trash_receipt(state: State<'_, AppState>, drive_id: String, receipt_id: String)
    -> Result<super::trash_receipts::ReceiptSummary, String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.receipt_restore", &drive_id).await?;
    let result = async {
        let store = trash_receipt_store(&state).await?;
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.capabilities().trash_receipts { return Err("Provider object-bound trash is not qualified at the pinned SDK revision".into()); }
        let provider = drive.receipt_trash().ok_or("Provider has no object-bound trash adapter")?;
        store.restore(provider, &drive_id, &receipt_id).map_err(|e| e.to_string())
    }.await;
    finish_operation(&log, &result, "receipt_restore_failed", |_| Value::Null);
    result
}

/// Reconcile remote state by UUID without retrying a delete or restore.
#[tauri::command]
pub async fn drive_recheck_trash_receipt(state: State<'_, AppState>, drive_id: String, receipt_id: String)
    -> Result<super::trash_receipts::ReceiptSummary, String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.receipt_recheck", &drive_id).await?;
    let result = async {
        let store = trash_receipt_store(&state).await?;
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.capabilities().trash_receipts { return Err("Provider object-bound trash is not qualified at the pinned SDK revision".into()); }
        let provider = drive.receipt_trash().ok_or("Provider has no object-bound trash adapter")?;
        store.reconcile(provider, &drive_id, &receipt_id).map_err(|e| e.to_string())
    }.await;
    finish_operation(&log, &result, "receipt_recheck_failed", |_| Value::Null);
    result
}

/// Restore an item from a provider's recoverable trash.
#[tauri::command]
pub async fn drive_restore_deleted(
    state: State<'_, AppState>,
    drive_id: String,
    trash_path: String,
    destination: Option<String>,
) -> Result<(), String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.restore_deleted", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
        if !drive.probed_capabilities().reversible_trash {
            return Err(format!(
                "{} does not support restoring deleted items",
                drive.drive_type().label()
            ));
        }
        let destination = destination.as_deref().map(std::path::Path::new);
        drive
            .restore_deleted(std::path::Path::new(&trash_path), destination)
            .map_err(|e| e.to_string())
    }
    .await;
    finish_operation(&log, &result, "drive_restore_deleted_failed", |_| Value::Null);
    result
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
    // The link itself is a bearer URL and is never logged.
    let (log, cfg) = drive_operation(&state, "storage.drive.share_link", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
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
    .await;
    finish_operation(&log, &result, "drive_share_link_failed", |_| Value::Null);
    result
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
    let drive = instantiate_registered(&state, cfg).await?;
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
    let (log, cfg) = drive_operation(&state, "storage.drive.restore_version", &drive_id).await?;
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
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
    .await;
    finish_operation(&log, &result, "drive_restore_version_failed", |_| Value::Null);
    result
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
    let drive: Arc<dyn super::CloudDrive> = Arc::from(instantiate_registered(&state, cfg).await?);
    let path = std::path::PathBuf::from(path);
    let transfer = state.transfer_queue.clone().submit_drive_download_bounded(
        drive_id, drive, path, super::MAX_PREVIEW_READ_BYTES,
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
    let drive: Arc<dyn super::CloudDrive> = Arc::from(instantiate_registered(&state, cfg).await?);
    let path = std::path::PathBuf::from(path);
    let retry_data = data.clone();
    let retry_path = path.clone();
    let retry_drive_id = drive_id.clone();
    let transfer = state
        .transfer_queue
        .clone()
        .submit_drive_upload(drive_id, drive, path, data);
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
    let (log, cfg) = drive_operation(&state, "storage.drive.delta_upload", &drive_id).await?;
    let result = drive_delta_upload_inner(drive_id, &cfg, local_path, remote_path).await;
    finish_operation(&log, &result, "drive_delta_upload_failed", delta_receipt);
    result
}

/// `accepted=false` means the server has no delta endpoint and the caller
/// falls back to a full upload/download.
fn delta_receipt(result: &Option<super::webdav::DeltaTransferResult>) -> Value {
    match result {
        Some(delta) => json!({
            "accepted": true,
            "completed": delta.changed_blocks,
            "total": delta.total_blocks,
            "bytes": delta.transferred_bytes,
            "input_bytes": delta.total_bytes,
        }),
        None => json!({"accepted": false}),
    }
}

async fn drive_delta_upload_inner(
    drive_id: String,
    cfg: &DriveConfig,
    local_path: String,
    remote_path: String,
) -> Result<Option<super::webdav::DeltaTransferResult>, String> {
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
    let (log, cfg) = drive_operation(&state, "storage.drive.delta_download", &drive_id).await?;
    let result = drive_delta_download_inner(drive_id, &cfg, remote_path, local_path).await;
    finish_operation(&log, &result, "drive_delta_download_failed", delta_receipt);
    result
}

async fn drive_delta_download_inner(
    drive_id: String,
    cfg: &DriveConfig,
    remote_path: String,
    local_path: String,
) -> Result<Option<super::webdav::DeltaTransferResult>, String> {
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
    let (log, cfg) = drive_operation(&state, "storage.drive.upload_resumable", &drive_id).await?;
    let workers = workers.unwrap_or(1).clamp(1, 10);
    log.step("transfer", json!({"mode": "upload", "count": workers}));
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
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
    .await;
    finish_operation(&log, &result, "drive_upload_resumable_failed", |_| Value::Null);
    result
}

#[tauri::command]
pub async fn drive_download_resumable(
    state: State<'_, AppState>,
    drive_id: String,
    remote_path: String,
    local_path: String,
    state_path: String,
) -> Result<(), String> {
    let (log, cfg) = drive_operation(&state, "storage.drive.download_resumable", &drive_id).await?;
    log.step("transfer", json!({"mode": "download"}));
    let result = async {
        let drive = instantiate_registered(&state, &cfg).await?;
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
    .await;
    finish_operation(&log, &result, "drive_download_resumable_failed", |_| Value::Null);
    result
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
    // Account, password, 2FA code and endpoint never enter the receipt.
    let (log, config) = drive_operation(&state, "storage.drive.login", &drive_id).await?;
    let result = (|| -> Result<(), String> {
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
    })();
    finish_operation(&log, &result, "drive_login_failed", |_| Value::Null);
    result
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
    let (log, config) = drive_operation(&state, "storage.drive.login", &drive_id).await?;
    let result = (|| -> Result<(), String> {
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
    })();
    finish_operation(&log, &result, "drive_login_failed", |_| Value::Null);
    result
}

/// Remove the native Internxt session from the OS keychain.
#[tauri::command]
pub async fn drive_native_logout(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<(), String> {
    let (log, _config) = drive_operation(&state, "storage.drive.logout", &drive_id).await?;
    let result = super::secret::delete_session(&drive_id).map_err(|error| error.to_string());
    finish_operation(&log, &result, "drive_logout_failed", |_| Value::Null);
    result
}

/// Refresh the bearer tokens for a keychain-backed native Internxt session.
#[tauri::command]
pub async fn drive_native_refresh(
    state: State<'_, AppState>,
    drive_id: String,
) -> Result<(), String> {
    let (log, config) = drive_operation(&state, "storage.drive.session_refresh", &drive_id).await?;
    let result: Result<(), String> = async {
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
        let index_config = state.index.lock().await.config.clone();
        let proxy = super::internxt_native::HttpConfig {
            proxy_url: index_config.proxy_url,
            proxy_username: index_config.proxy_username,
            proxy_password: crate::sync::proxy_secret::get()
                .map_err(|error| format!("reading proxy password failed: {error}"))?,
        };
        let client = super::internxt_native::InternxtNativeClient::new_with_config_and_http(
            &session.drive_api_url,
            session.active_token(),
            super::internxt_native::TransferConfig::default(),
            &proxy,
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
    .await;
    finish_operation(&log, &result, "drive_session_refresh_failed", |_| Value::Null);
    result
}

/// Which drive kinds have a native (no-subprocess) client compiled into this
/// build.
///
/// The UI needs this *before* a drive exists, so it cannot use
/// `drive_capabilities`, which resolves a registered `drive_id`. The picker
/// uses it to decide whether to offer the kind at all.
///
/// Since the Python-CLI drives were retired there is no longer a
/// "native or `cli.py`?" question to answer: `desktop` implies both native
/// features, so on desktop these are always `true`, and a build without them
/// has no Filen/Internxt transport whatsoever rather than a slower one. The
/// command stays because mobile builds still answer `false` and the picker
/// must not offer a kind it cannot instantiate.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct NativeDriveSupport {
    pub filen: bool,
    pub internxt: bool,
}

#[tauri::command]
pub fn drive_native_support() -> NativeDriveSupport {
    NativeDriveSupport {
        filen: cfg!(feature = "drive-filen-native"),
        internxt: cfg!(feature = "drive-internxt-native"),
    }
}

#[cfg(test)]
mod operation_receipt_tests {
    use super::*;

    #[test]
    fn every_drive_kind_survives_the_backend_allowlist() {
        for kind in [DriveType::Local, DriveType::Smb, DriveType::Filen, DriveType::Internxt,
            DriveType::Sftp, DriveType::WebDav, DriveType::OneDrive, DriveType::GoogleDrive] {
            let context = crate::operation_log::safe_context(json!({"backend": backend_label(&kind)}));
            assert_eq!(context.get("backend").and_then(Value::as_str), Some(backend_label(&kind)));
        }
    }

    #[test]
    fn command_receipts_keep_fixed_codes_and_drop_private_errors() {
        let ((), events) = crate::operation_log::capture(|| {
            let log = OperationLog::start("storage.drive.move", json!({"backend": "webdav"}));
            let result: Result<&'static str, String> =
                Err("moving /private/report.pdf on https://user:secret@host failed".into());
            finish_operation(&log, &result, "drive_move_failed", |mode| json!({"mode": mode}));
            let log = OperationLog::start("storage.drive.copy", json!({"backend": "onedrive"}));
            let applied = PathBuf::from("/private/report (copy).pdf");
            let result: Result<&'static str, String> =
                Ok(mutation_mode(Some(applied.as_path()), Path::new("/private/report.pdf")));
            finish_operation(&log, &result, "drive_copy_failed", |mode| json!({"mode": mode}));
        });
        let terminals: Vec<_> = events.iter()
            .filter(|e| ["completed", "failed", "cancelled"].contains(&e.phase.as_str())).collect();
        assert_eq!(terminals.len(), 2);
        assert_eq!(terminals[0].phase, "failed");
        assert_eq!(terminals[0].code.as_deref(), Some("drive_move_failed"));
        assert_eq!(terminals[1].phase, "completed");
        assert_eq!(terminals[1].context.get("mode"), Some(&json!("drives.keep_both")));
        assert_eq!(terminals[1].context.get("backend"), Some(&json!("onedrive")));
        let serialized = serde_json::to_string(&events).unwrap();
        for private in ["private", "secret", "report", "host"] {
            assert!(!serialized.contains(private), "leaked {private}");
        }
    }

    #[test]
    fn skipped_mutations_report_skip() {
        assert_eq!(mutation_mode(None, Path::new("a")), "skip");
        assert_eq!(mutation_mode(Some(Path::new("a")), Path::new("a")), "drives.apply");
    }
}

#[cfg(test)]
mod native_support_tests {
    use super::*;

    #[test]
    fn native_support_reports_the_compiled_features() {
        let support = drive_native_support();
        assert_eq!(support.filen, cfg!(feature = "drive-filen-native"));
        assert_eq!(support.internxt, cfg!(feature = "drive-internxt-native"));
    }
}

#[cfg(all(test, feature = "fuse"))]
mod fuse_mount_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_mount_reservations_keep_exactly_one_destination() {
        let mounts = Arc::new(Mutex::new(HashMap::new()));
        let ready = Arc::new(Barrier::new(3));
        let workers: Vec<_> = ["/mnt/first", "/mnt/second"]
            .into_iter()
            .map(|point| {
                let mounts = Arc::clone(&mounts);
                let ready = Arc::clone(&ready);
                std::thread::spawn(move || {
                    let point = PathBuf::from(point);
                    ready.wait();
                    let result = reserve_fuse_mount(&mounts, "same-drive", point.clone());
                    (point, result)
                })
            })
            .collect();
        ready.wait();
        let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
        let winners: Vec<_> = results.iter().filter(|(_, result)| result.is_ok()).collect();
        assert_eq!(winners.len(), 1);
        let failed = results.iter().find_map(|(_, result)| result.as_ref().err()).unwrap();
        assert!(failed.contains("already mounted"));
        let active = mounts.lock().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active.get("same-drive"), Some(&winners[0].0));
    }
}

#[cfg(test)]
mod mutation_conflict_tests {
    use super::*;

    #[test]
    fn newest_only_allows_a_strictly_newer_source() {
        assert_eq!(
            mutation_destination(
                Some(20),
                Some(10),
                Path::new("/remote/report.pdf"),
                ConflictPolicy::NewestWins,
            )
            .unwrap(),
            MutationResolution::UseDestination
        );
        assert_eq!(
            mutation_destination(
                Some(10),
                Some(20),
                Path::new("/remote/report.pdf"),
                ConflictPolicy::NewestWins,
            )
            .unwrap(),
            MutationResolution::Skip
        );
    }

    #[test]
    fn newest_preserves_destination_when_timestamps_are_missing_or_equal() {
        for (source, destination) in [(Some(20), None), (None, Some(20)), (None, None), (Some(20), Some(20)), (Some(0), Some(20)), (Some(20), Some(0)), (Some(0), Some(0))] {
            assert_eq!(mutation_destination(source, destination, Path::new("report.txt"), ConflictPolicy::NewestWins).unwrap(), MutationResolution::Skip);
            // The user's explicit overwrite policy remains an override.
            assert_eq!(mutation_destination(source, destination, Path::new("report.txt"), ConflictPolicy::RemoteWins).unwrap(), MutationResolution::UseDestination);
        }
    }

    #[test]
    fn keep_both_adds_copy_suffix_without_overwrite() {
        assert_eq!(
            mutation_destination(
                Some(10),
                Some(20),
                Path::new("/remote/report.pdf"),
                ConflictPolicy::KeepBoth,
            )
            .unwrap(),
            MutationResolution::KeepBoth(PathBuf::from("/remote/report (copy).pdf"))
        );
    }

    #[test]
    fn manual_conflicts_are_rejected_and_local_wins_skips() {
        assert!(mutation_destination(
            Some(10),
            Some(20),
            Path::new("report.txt"),
            ConflictPolicy::Manual,
        )
        .is_err());
        assert_eq!(
            mutation_destination(
                Some(30),
                Some(20),
                Path::new("report.txt"),
                ConflictPolicy::LocalWins,
            )
            .unwrap(),
            MutationResolution::Skip
        );
    }
}

#[cfg(test)]
mod credential_status_tests {
    use super::*;

    #[test]
    fn credential_status_is_presence_only() {
        let status = DriveCredentialsStatus {
            has_username: true,
            has_password: true,
            has_access_token: true,
            has_refresh_token: true,
            has_client_id: true,
            has_session: true,
        };
        let json = serde_json::to_string(&status).expect("status serializes");
        for secret in ["password", "access_token", "refresh_token", "token"] {
            assert!(!json.contains(&format!("\"{secret}\"")));
        }
        for field in [
            "has_username",
            "has_password",
            "has_access_token",
            "has_refresh_token",
            "has_client_id",
            "has_session",
        ] {
            assert!(json.contains(field), "missing presence field {field}");
        }
    }
}

#[cfg(test)]
mod drive_search_tests {
    use super::drive_search_path_matches;
    use std::path::Path;

    #[test]
    fn matches_only_the_filename_case_insensitively() {
        assert!(drive_search_path_matches(Path::new("/docs/Report.PDF"), "report"));
        assert!(!drive_search_path_matches(Path::new("/docs/report.pdf"), "docs"));
        assert!(!drive_search_path_matches(Path::new("/docs/报告.pdf"), "report"));
    }

    #[test]
    fn empty_or_non_utf8_names_do_not_match() {
        assert!(!drive_search_path_matches(Path::new("/docs/readme.md"), ""));
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let raw = OsString::from_vec(vec![b'/', 0xff, b'.', b't', b'x', b't']);
            let path = Path::new(&raw);
            assert!(!drive_search_path_matches(path, "txt"));
        }
    }
}

#[cfg(test)]
mod destination_lookup_tests {
    use super::*;
    use crate::drives::{CloudDrive, LocalDrive, DriveType, DirEntry, FileStat};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct LookupFault {
        local: LocalDrive,
        error: Option<std::io::ErrorKind>,
        mutations: AtomicUsize,
    }
    impl CloudDrive for LookupFault {
        fn label(&self) -> &str { "lookup-fault" }
        fn drive_type(&self) -> DriveType { DriveType::Local }
        fn list_dir(&self, path: &Path) -> anyhow::Result<Vec<DirEntry>> { self.local.list_dir(path) }
        fn read_file(&self, path: &Path) -> anyhow::Result<Vec<u8>> { self.local.read_file(path) }
        fn write_file(&self, path: &Path, data: &[u8]) -> anyhow::Result<()> { self.local.write_file(path, data) }
        fn delete(&self, path: &Path) -> anyhow::Result<()> { self.local.delete(path) }
        fn stat(&self, path: &Path) -> anyhow::Result<FileStat> {
            if path == Path::new("destination") {
                return match self.error {
                    Some(kind) => Err(std::io::Error::new(kind, "lookup failed").into()),
                    None => Err(anyhow::anyhow!("HTTP lookup failed: not found or unauthorized")),
                };
            }
            self.local.stat(path)
        }
        fn copy_path(&self, source: &Path, destination: &Path) -> anyhow::Result<()> {
            self.mutations.fetch_add(1, Ordering::Relaxed);
            self.local.copy_path(source, destination)
        }
        fn move_path(&self, source: &Path, destination: &Path) -> anyhow::Result<()> {
            self.mutations.fetch_add(1, Ordering::Relaxed);
            self.local.move_path(source, destination)
        }
    }
    #[test]
    fn newest_destination_lookup_failure_preserves_bytes_before_copy_or_move() {
        for kind in [Some(std::io::ErrorKind::PermissionDenied), Some(std::io::ErrorKind::TimedOut), Some(std::io::ErrorKind::NotADirectory)] {
            for moving in [false, true] {
                let root = tempfile::tempdir().unwrap();
                std::fs::write(root.path().join("source"), b"incoming").unwrap();
                std::fs::write(root.path().join("destination"), b"existing").unwrap();
                let drive = LookupFault { local: LocalDrive::new("local", root.path()), error: kind, mutations: AtomicUsize::new(0) };
                // Same resolver and dispatch as drive_copy_path/drive_move_path;
                // the real LocalDrive performs any mutation admitted by it.
                let result = (|| -> Result<(), String> {
                    if let Some(destination) = resolve_mutation_path(&drive, Path::new("source"), Path::new("destination"), ConflictPolicy::NewestWins)? {
                        let operation = if moving { drive.move_path(Path::new("source"), &destination) } else { drive.copy_path(Path::new("source"), &destination) };
                        operation.map_err(|error| error.to_string())?;
                    }
                    Ok(())
                })();
                assert_eq!(std::fs::read(root.path().join("destination")).unwrap(), b"existing", "lookup={kind:?}, moving={moving}, result={result:?}");
                assert_eq!(std::fs::read(root.path().join("source")).unwrap(), b"incoming");
                assert!(result.is_err());
                assert_eq!(drive.mutations.load(Ordering::Relaxed), 0);
            }
        }
    }
}
