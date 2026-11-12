//! `CloudDrive` adapter for a keychain-backed native Internxt session.
//!
//! One `InternxtNativeClient` is kept per drive instance (see
//! [`super::provider_client`]), so its pooled HTTP connections and folder
//! listing cache survive between calls. Ancestor folders resolve through that
//! cache; the leaf entry is read from an uncached listing of its parent.

use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::internxt_native::{InternxtNativeClient, InternxtSession, NativeItem};
use super::provider_client::{self, ProviderClientCache};
use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat};
use crate::operation_log::OperationLog;

const BACKEND: &str = "internxt";
const MODE: &str = "drives.internxt.native";

pub struct NativeInternxtDrive {
    label: String,
    drive_id: String,
    proxy: crisp_internxt::HttpConfig,
    clients: ProviderClientCache<InternxtSession, InternxtNativeClient>,
}

fn split_remote_name(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => (name, "file"),
    }
}

fn load_session(drive_id: &str) -> Result<InternxtSession> {
    super::secret::get_session(drive_id)
        .and_then(|serialized| serialized.ok_or_else(|| anyhow!("no native Internxt session")))
        .and_then(|serialized| InternxtSession::decode(&serialized))
        .map_err(|error| anyhow!("native Internxt session unavailable: {error:#}"))
}

/// Transfer settings for the shared client. Kept at the library defaults on
/// purpose: one download worker and serial multipart parts are the gateway-safe
/// choice the crate documents, and part sizes are protocol-shaped. The only
/// caller-requested knob, the resumable-upload worker count, is passed per call
/// to `upload_path_with_resume_state_with_workers`.
fn transfer_config() -> crisp_internxt::TransferConfig {
    crisp_internxt::TransferConfig::default()
}

/// Resolve `path` with cached ancestors and an uncached leaf listing.
fn resolve_fresh(
    client: &InternxtNativeClient,
    session: &InternxtSession,
    path: &Path,
) -> Result<NativeItem> {
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        // The root (or an empty path) needs no network access.
        return client.resolve_path(session, path);
    };
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    provider_client::retry_with_fresh_listings(
        || {
            let folder = client.resolve_path(session, parent)?;
            anyhow::ensure!(folder.is_dir, "Internxt path traverses through a file");
            client
                .list_folder(&folder.uuid)?
                .into_iter()
                .find(|item| item.name == name)
                .ok_or_else(|| anyhow!("Internxt path component not found: {name}"))
        },
        || client.clear_listing_cache(),
    )
}

/// Walk the folder chain for `path`, creating missing folders when `create`.
/// A component missing from the cache is re-checked against an uncached
/// listing before it is created or reported missing.
fn resolve_folder(
    client: &InternxtNativeClient,
    session: &InternxtSession,
    path: &Path,
    create: bool,
) -> Result<String> {
    let mut folder_uuid = session.root_folder_id.clone();
    for component in path.components() {
        let component = component.as_os_str().to_string_lossy();
        if component.is_empty() || component == "." || component == "/" {
            continue;
        }
        let find = |items: Vec<NativeItem>| {
            items
                .into_iter()
                .find(|item| item.is_dir && item.name == component)
                .map(|item| item.uuid)
        };
        let found = match find(client.list_folder_cached(&folder_uuid)?) {
            Some(uuid) => Some(uuid),
            None => find(client.list_folder(&folder_uuid)?),
        };
        folder_uuid = match found {
            Some(uuid) => uuid,
            None if create => client.create_folder(&folder_uuid, &component)?,
            None => return Err(anyhow!("Internxt folder not found: {component}")),
        };
    }
    Ok(folder_uuid)
}

impl NativeInternxtDrive {
    pub fn from_keychain(label: impl Into<String>, drive_id: &str) -> Self {
        Self::from_keychain_with_proxy(label, drive_id, &Default::default())
    }

    /// The session is read from the keychain on first use, and again after
    /// the gateway rejects its token, so a live instance picks up a token the
    /// app's refresh command stored (or reports a logout).
    pub fn from_keychain_with_proxy(
        label: impl Into<String>,
        drive_id: &str,
        proxy: &crate::sync::proxy::ProxyConfig,
    ) -> Self {
        Self {
            label: label.into(),
            drive_id: drive_id.to_owned(),
            proxy: crisp_internxt::HttpConfig {
                proxy_url: proxy.url.clone(),
                proxy_username: proxy.username.clone(),
                proxy_password: proxy.password.clone(),
            },
            clients: ProviderClientCache::new(),
        }
    }

    /// Run `op` with the shared client. Any failure empties the listing cache
    /// (staleness may be why it failed); a credential rejection also drops the
    /// client and session so the next call re-reads the keychain.
    fn with_client<T>(
        &self,
        log: Option<&OperationLog>,
        op: impl FnOnce(&InternxtSession, &InternxtNativeClient) -> Result<T>,
    ) -> Result<T> {
        let lease = self.clients.lease(
            || load_session(&self.drive_id),
            |session| {
                InternxtNativeClient::new_with_config_and_http(
                    &session.drive_api_url,
                    session.active_token(),
                    transfer_config(),
                    &self.proxy,
                )
            },
        )?;
        if let Some(log) = log {
            log.step("client", serde_json::json!({ "reused": lease.reused }));
        }
        let result = op(lease.session.as_ref(), lease.client.as_ref());
        if let Err(error) = &result {
            lease.client.clear_listing_cache();
            if provider_client::is_auth_error(error) {
                self.clients.invalidate();
                provider_client::log_session_reset(BACKEND);
            }
        }
        result
    }

    /// Overwrite-or-create shared by `write_file` and `write_file_from_reader`.
    ///
    /// Still trash-then-upload by hand: the vendored crate's replace primitive
    /// (`update_file`, upload-beside-then-swap) only accepts a local path, and
    /// these trait methods receive bytes or a reader.
    /// TODO(crisp-cloud-rs): switch to a reader-based `update_file` once the
    /// upstream crate offers one, which also avoids the trashed-but-not-yet-
    /// replaced window on failure.
    fn write_with(
        &self,
        path: &Path,
        size: u64,
        upload: impl FnOnce(&InternxtNativeClient, &InternxtSession, &str, &str, &str) -> Result<()>,
    ) -> Result<()> {
        let filename = path
            .file_name()
            .ok_or_else(|| anyhow!("Internxt write path has no filename: {}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        let filename_path = PathBuf::from(&filename);
        let plain_name = filename_path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| filename.clone());
        let file_type = filename_path
            .extension()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        provider_client::logged("storage.internxt.write", BACKEND, MODE, |log| {
            self.with_client(Some(log), |session, client| {
                let folder_uuid = resolve_folder(client, session, parent, true)?;
                if let Ok(existing) = resolve_fresh(client, session, path) {
                    if existing.is_dir {
                        return Err(anyhow!(
                            "Internxt write path is a directory: {}",
                            path.display()
                        ));
                    }
                    log.step("replace", serde_json::json!({ "bytes": size }));
                    client.trash(&existing.uuid, "file")?;
                } else {
                    log.step("upload", serde_json::json!({ "bytes": size }));
                }
                upload(
                    client,
                    session,
                    folder_uuid.as_str(),
                    plain_name.as_str(),
                    file_type.as_str(),
                )
            })
        })
    }
}

impl CloudDrive for NativeInternxtDrive {
    fn label(&self) -> &str {
        &self.label
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            if !item.is_dir {
                return Err(anyhow!(
                    "Internxt path is not a directory: {}",
                    path.display()
                ));
            }
            Ok(client
                .list_folder(&item.uuid)?
                .into_iter()
                .map(|item| DirEntry {
                    name: item.name,
                    is_dir: item.is_dir,
                    size: (!item.is_dir).then_some(item.size),
                })
                .collect())
        })
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            if item.is_dir {
                return Err(anyhow!("Internxt path is a directory: {}", path.display()));
            }
            client.download_file(session, &item.uuid)
        })
    }

    // TODO(crisp-cloud-rs): `read_range`. The vendored crate's only ranged
    // primitive, `download_file_to_path_ranged`, downloads the whole file to a
    // path (ranges are a parallelism detail), so it cannot serve a partial
    // read. A real implementation needs an upstream `download_file_range`
    // (Range GET on the presigned shard URL + `crypt_at` at the CTR offset).
    // Until then the trait default refuses, and the FUSE mount keeps
    // materialising Internxt files once into its disk cache.

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.write_with(
            path,
            data.len() as u64,
            |client, session, folder_uuid, plain_name, file_type| {
                client.upload_file(session, folder_uuid, plain_name, file_type, data)
            },
        )
    }

    fn upload_file_resumable(
        &self,
        local_path: &Path,
        remote_path: &Path,
        state_path: &Path,
        workers: usize,
    ) -> Result<()> {
        let filename = remote_path
            .file_name()
            .ok_or_else(|| anyhow!("Internxt remote path has no filename"))?
            .to_string_lossy()
            .into_owned();
        let parent = remote_path.parent().unwrap_or_else(|| Path::new("/"));
        let workers = workers.max(1);
        provider_client::logged("storage.internxt.upload_resumable", BACKEND, MODE, |log| {
            log.step("plan", serde_json::json!({ "count": workers as u64 }));
            self.with_client(Some(log), |session, client| {
                let folder_uuid = resolve_folder(client, session, parent, true)?;
                let (plain_name, file_type) = split_remote_name(&filename);
                client.upload_path_with_resume_state_with_workers(
                    session,
                    &folder_uuid,
                    plain_name,
                    file_type,
                    local_path,
                    state_path,
                    workers,
                )
            })
        })
    }

    fn download_file_resumable(
        &self,
        remote_path: &Path,
        local_path: &Path,
        state_path: &Path,
    ) -> Result<()> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, remote_path)?;
            anyhow::ensure!(
                !item.is_dir,
                "Internxt remote path is a directory: {}",
                remote_path.display()
            );
            client.download_file_to_path_resumable(session, &item.uuid, local_path, state_path)
        })
    }

    fn delete(&self, path: &Path) -> Result<()> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            client.trash(&item.uuid, if item.is_dir { "folder" } else { "file" })
        })
    }

    fn restore_deleted(&self, trash_path: &Path, destination: Option<&Path>) -> Result<()> {
        let name = trash_path
            .file_name()
            .ok_or_else(|| anyhow!("Internxt trash path has no item name"))?
            .to_string_lossy()
            .into_owned();
        let destination =
            destination.ok_or_else(|| anyhow!("Internxt restore requires a destination folder"))?;
        self.with_client(None, |session, client| {
            let item = client
                .list_trash(None, usize::MAX)?
                .into_iter()
                .find(|item| item.name == name)
                .ok_or_else(|| {
                    anyhow!("Internxt trash item not found: {}", trash_path.display())
                })?;
            let folder_uuid = resolve_folder(client, session, destination, false)?;
            client.restore_from_trash(
                &item.uuid,
                if item.is_dir { "folders" } else { "files" },
                &folder_uuid,
            )
        })
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            copy: true,
            streaming: true,
            resumable_upload: true,
            resumable_download: true,
            reversible_trash: true,
            ..DriveCapabilities::basic()
        }
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        self.with_client(None, |session, client| {
            resolve_folder(client, session, path, true).map(|_| ())
        })
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("/"));
        let destination_name = destination
            .file_name()
            .ok_or_else(|| {
                anyhow!(
                    "move destination has no filename: {}",
                    destination.display()
                )
            })?
            .to_string_lossy()
            .into_owned();
        let source_parent = source.parent().unwrap_or_else(|| Path::new("/"));
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, source)?;
            let destination_folder_uuid =
                resolve_folder(client, session, destination_parent, false)?;
            if item.name != destination_name {
                if item.is_dir {
                    client.rename_folder(&item.uuid, &destination_name)?;
                } else {
                    let (plain_name, file_type) = split_remote_name(&destination_name);
                    client.rename_file(&item.uuid, plain_name, file_type)?;
                }
            }
            if source_parent != destination_parent {
                if item.is_dir {
                    client.move_folder(&item.uuid, &destination_folder_uuid)?;
                } else {
                    client.move_file(&item.uuid, &destination_folder_uuid)?;
                }
            }
            Ok(())
        })
    }

    fn copy_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("/"));
        let destination_name = destination
            .file_name()
            .ok_or_else(|| {
                anyhow!(
                    "copy destination has no filename: {}",
                    destination.display()
                )
            })?
            .to_string_lossy()
            .into_owned();
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, source)?;
            let destination_folder_uuid =
                resolve_folder(client, session, destination_parent, false)?;
            if item.is_dir {
                client.copy_folder(
                    session,
                    &item.uuid,
                    &destination_folder_uuid,
                    Some(&destination_name),
                )?;
            } else {
                // The native copy API takes a plain name and retains the source
                // extension. Rename the returned item afterward when the full
                // destination leaf differs from the source leaf.
                let copied =
                    client.copy_file(session, &item.uuid, &destination_folder_uuid, None)?;
                if copied.name != destination_name {
                    let (plain_name, file_type) = split_remote_name(&destination_name);
                    client.rename_file(&copied.uuid, plain_name, file_type)?;
                }
            }
            Ok(())
        })
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            if item.is_dir {
                return Ok(FileStat {
                    size: 0,
                    is_dir: true,
                    mtime_unix: None,
                });
            }
            let metadata = client.file_metadata(&item.uuid)?;
            let size = metadata
                .get("size")
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
                .unwrap_or(item.size);
            Ok(FileStat {
                size,
                is_dir: false,
                mtime_unix: None,
            })
        })
    }

    fn drive_type(&self) -> DriveType {
        DriveType::Internxt
    }

    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            anyhow::ensure!(
                !item.is_dir,
                "Internxt path is a directory: {}",
                path.display()
            );
            client.download_file_to_writer(session, &item.uuid, writer)
        })
    }

    fn write_file_from_reader(&self, path: &Path, reader: &mut dyn Read, size: u64) -> Result<()> {
        self.write_with(
            path,
            size,
            |client, session, folder_uuid, plain_name, file_type| {
                client.upload_reader(session, folder_uuid, plain_name, file_type, reader, size)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_metadata_is_available_without_a_session() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        assert_eq!(drive.label(), "Native Internxt");
        assert_eq!(drive.drive_type(), DriveType::Internxt);
        let capabilities = drive.capabilities();
        assert!(capabilities.create_dir);
        assert!(capabilities.rename);
        assert!(capabilities.move_path);
        assert!(capabilities.copy);
        assert!(capabilities.streaming);
        assert!(capabilities.resumable_upload);
        assert!(capabilities.resumable_download);
    }

    #[test]
    fn missing_session_is_reported_before_network_access() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        let error = drive
            .list_dir(Path::new("/"))
            .expect_err("missing session should fail");
        assert!(format!("{error:#}").contains("native Internxt session unavailable"));
        assert!(!drive.clients.is_cached());
    }

    #[test]
    fn range_reads_are_still_refused_explicitly() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        let error = drive.read_range(Path::new("/a"), 0, 1).unwrap_err();
        assert!(error.to_string().contains("does not support range reads"));
    }

    #[test]
    fn client_is_reused_across_calls_and_uses_the_refreshed_token() {
        crate::drives::secret::install_mock_for_tests();
        let session = InternxtSession {
            drive_api_url: "http://127.0.0.1:1/drive".into(),
            network_url: "http://127.0.0.1:1/network".into(),
            email: "test@example.test".into(),
            token: "old-token".into(),
            new_token: "refreshed-token".into(),
            mnemonic: String::new(),
            user_id: "user".into(),
            root_folder_id: "root".into(),
            bridge_user: "bridge".into(),
            bucket_id: String::new(),
        };
        crate::drives::secret::set_session("internxt-reuse", &session.encode().unwrap()).unwrap();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "internxt-reuse");
        // The root resolves locally, so these calls need no network access.
        assert!(drive.stat(Path::new("/")).unwrap().is_dir);
        let first = drive
            .clients
            .lease(|| unreachable!(), |_| unreachable!())
            .unwrap();
        assert!(first.reused);
        assert!(drive.stat(Path::new("")).unwrap().is_dir);
        let second = drive
            .clients
            .lease(|| unreachable!(), |_| unreachable!())
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&first.client, &second.client));
        assert_eq!(first.session.active_token(), "refreshed-token");
        crate::drives::secret::delete_session("internxt-reuse").unwrap();
    }
}
