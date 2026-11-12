//! `CloudDrive` adapter for a keychain-backed native Filen session.
//!
//! One `FilenNativeClient` is kept per drive instance (see
//! [`super::provider_client`]), so its pooled HTTP connections and 600 s
//! folder-listing cache survive between calls. Ancestor folders resolve
//! through that cache; the leaf entry is read from a fresh listing of its
//! parent, so stat/read/write never act on a stale size, key or uuid.

use super::provider_client::{self, ProviderClientCache};
use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat};
use crate::operation_log::OperationLog;
use anyhow::{anyhow, Result};
use crisp_filen::{FilenNativeClient, FilenSession, NativeItem};
use std::io::{Read, Write};
use std::path::Path;

const MIME: &str = "application/octet-stream";
const BACKEND: &str = "filen";
const MODE: &str = "drives.filen.native";

pub struct NativeFilenDrive {
    label: String,
    drive_id: String,
    proxy: crisp_filen::HttpConfig,
    clients: ProviderClientCache<FilenSession, FilenNativeClient>,
}

/// Where an overwrite-or-create write lands.
enum WriteTarget<'a> {
    /// An existing file: replaced through the library's trash-then-upload.
    Replace(&'a NativeItem),
    /// No file at the path: a new upload into `parent`.
    Create { parent: &'a str, name: &'a str },
}

fn load_session(drive_id: &str) -> Result<FilenSession> {
    super::secret::get_session(drive_id)
        .and_then(|value| value.ok_or_else(|| anyhow!("no native Filen session")))
        .and_then(|value| FilenSession::decode(&value))
        .map_err(|e| anyhow!("native Filen session unavailable: {e:#}"))
}

fn kind(item: &NativeItem) -> &'static str {
    if item.is_dir {
        "folder"
    } else {
        "file"
    }
}

fn leaf_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Resolve `path` with cached ancestors and a fresh leaf listing.
fn resolve_fresh(
    client: &FilenNativeClient,
    session: &FilenSession,
    path: &Path,
) -> Result<NativeItem> {
    let Some(name) = leaf_name(path) else {
        // The root (or an empty path) needs no network access.
        return client.resolve_path(session, path);
    };
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    provider_client::retry_with_fresh_listings(
        || {
            let folder = client.resolve_path(session, parent)?;
            anyhow::ensure!(folder.is_dir, "Filen path not found: {path:?}");
            client
                .list_folder_fresh(&folder.uuid)?
                .into_iter()
                .find(|item| item.name == name)
                .ok_or_else(|| anyhow!("Filen path not found: {path:?}"))
        },
        || client.invalidate_listings(),
    )
}

/// Resolve `path` entirely through the listing cache (falling back to fresh
/// listings once). Used for ranged reads, where a fresh parent listing per
/// FUSE page would cost far more than the bytes requested; a stale item fails
/// at the chunk download, and that error empties the cache.
fn resolve_cached(
    client: &FilenNativeClient,
    session: &FilenSession,
    path: &Path,
) -> Result<NativeItem> {
    provider_client::retry_with_fresh_listings(
        || client.resolve_path(session, path),
        || client.invalidate_listings(),
    )
}

/// Walk (and create) the folder chain for `path`, returning its uuid. A
/// component missing from the cache is re-checked against a fresh listing
/// before it is created: Filen permits duplicate names, so a stale cache would
/// otherwise create a second folder beside one made from another device.
fn ensure_folder(
    client: &FilenNativeClient,
    session: &FilenSession,
    path: &Path,
) -> Result<String> {
    let mut uuid = session.root_folder_uuid.clone();
    for part in path.components() {
        let part = part.as_os_str().to_string_lossy();
        if part.is_empty() || part == "." || part == "/" {
            continue;
        }
        let find = |items: Vec<NativeItem>| {
            items
                .into_iter()
                .find(|item| item.is_dir && item.name == part)
                .map(|item| item.uuid)
        };
        uuid = match find(client.list_folder(&uuid)?) {
            Some(found) => found,
            None => match find(client.list_folder_fresh(&uuid)?) {
                Some(found) => found,
                None => client.create_folder(&uuid, &part)?,
            },
        };
    }
    Ok(uuid)
}

impl NativeFilenDrive {
    pub fn from_keychain(label: impl Into<String>, drive_id: &str) -> Self {
        Self::from_keychain_with_proxy(label, drive_id, &Default::default())
    }

    /// The session is read from the keychain on first use, and again after
    /// the gateway rejects it, so a live instance picks up a re-login.
    pub fn from_keychain_with_proxy(
        label: impl Into<String>,
        drive_id: &str,
        proxy: &crate::sync::proxy::ProxyConfig,
    ) -> Self {
        Self {
            label: label.into(),
            drive_id: drive_id.to_owned(),
            proxy: crisp_filen::HttpConfig {
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
    ///
    /// The client keeps the library's default `TransferConfig`: its 1 MiB
    /// `chunk_size` is the Filen wire chunk size (`download_file_range` maps
    /// offsets with it) and must not be tuned, and `workers` stays at the
    /// library's gateway-safe serial default.
    fn with_client<T>(
        &self,
        log: Option<&OperationLog>,
        op: impl FnOnce(&FilenSession, &FilenNativeClient) -> Result<T>,
    ) -> Result<T> {
        let lease = self.clients.lease(
            || load_session(&self.drive_id),
            |session| FilenNativeClient::from_session_with_http_config(session, &self.proxy),
        )?;
        if let Some(log) = log {
            log.step("client", serde_json::json!({ "reused": lease.reused }));
        }
        let result = op(lease.session.as_ref(), lease.client.as_ref());
        if let Err(error) = &result {
            lease.client.invalidate_listings();
            if provider_client::is_auth_error(error) {
                self.clients.invalidate();
                provider_client::log_session_reset(BACKEND);
            }
        }
        result
    }

    fn resolve_file(
        client: &FilenNativeClient,
        session: &FilenSession,
        path: &Path,
    ) -> Result<NativeItem> {
        let item = resolve_fresh(client, session, path)?;
        anyhow::ensure!(
            !item.is_dir,
            "Filen path is a directory: {}",
            path.display()
        );
        Ok(item)
    }

    /// Overwrite-or-create shared by `write_file` and `write_file_from_reader`.
    /// An existing file is replaced through the library's trash-then-upload
    /// (Filen permits duplicate names, so a plain upload would add a sibling).
    /// An existing *folder* at the path keeps this adapter's historical
    /// behaviour: it is moved to the (reversible) trash and the file uploaded.
    /// Conflict policy is decided by the caller before this is reached.
    fn write_with(
        &self,
        path: &Path,
        size: u64,
        write: impl FnOnce(&FilenNativeClient, WriteTarget<'_>) -> Result<()>,
    ) -> Result<()> {
        let name = leaf_name(path).ok_or_else(|| anyhow!("Filen write path has no filename"))?;
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        provider_client::logged("storage.filen.write", BACKEND, MODE, |log| {
            self.with_client(Some(log), |session, client| {
                match resolve_fresh(client, session, path) {
                    Ok(existing) if !existing.is_dir => {
                        log.step("replace", serde_json::json!({ "bytes": size }));
                        write(client, WriteTarget::Replace(&existing))
                    }
                    existing => {
                        if let Ok(folder) = existing {
                            client.trash(&folder.uuid, kind(&folder))?;
                        }
                        let parent_uuid = ensure_folder(client, session, parent)?;
                        log.step("upload", serde_json::json!({ "bytes": size }));
                        write(
                            client,
                            WriteTarget::Create {
                                parent: &parent_uuid,
                                name: &name,
                            },
                        )
                    }
                }
            })
        })
    }
}

impl CloudDrive for NativeFilenDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            anyhow::ensure!(
                item.is_dir,
                "Filen path is not a directory: {}",
                path.display()
            );
            // User-visible listings stay fresh; this also refreshes the cache
            // entry that later path resolution reads.
            Ok(client
                .list_folder_fresh(&item.uuid)?
                .into_iter()
                .map(|i| DirEntry {
                    name: i.name,
                    is_dir: i.is_dir,
                    size: (!i.is_dir).then_some(i.size),
                })
                .collect())
        })
    }
    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.with_client(None, |session, client| {
            let item = Self::resolve_file(client, session, path)?;
            client.download_file(&item)
        })
    }

    /// Inclusive range read backed by `download_file_range`, which fetches and
    /// authenticates only the encrypted chunks covering the range. A range
    /// running past EOF is clamped, and one starting at/after EOF yields no
    /// bytes, which is what a FUSE final-page read expects.
    fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>> {
        provider_client::inclusive_range_len(start, end)?;
        provider_client::logged("storage.filen.read_range", BACKEND, MODE, |log| {
            self.with_client(Some(log), |session, client| {
                let item = resolve_cached(client, session, path)?;
                anyhow::ensure!(
                    !item.is_dir,
                    "Filen path is a directory: {}",
                    path.display()
                );
                let Some((offset, length)) =
                    provider_client::clamp_range_to_size(start, end, item.size)?
                else {
                    return Ok(Vec::new());
                };
                let bytes = client.download_file_range(&item, offset, length)?;
                log.step(
                    "transferred",
                    serde_json::json!({ "bytes": bytes.len() as u64 }),
                );
                Ok(bytes)
            })
        })
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.write_with(path, data.len() as u64, |client, target| match target {
            WriteTarget::Replace(existing) => client.replace_file(existing, MIME, data),
            WriteTarget::Create { parent, name } => client.upload_file(parent, name, MIME, data),
        })
    }

    /// Resumable uploads go through the library's `resume_upload_from_reader`,
    /// which uploads chunks sequentially from the reader and has no worker
    /// knob, so the requested count is recorded but cannot speed this path up.
    /// TODO(crisp-cloud-rs): honour `workers` once the upstream resume path
    /// uploads checkpointed chunks concurrently.
    fn upload_file_resumable(
        &self,
        local_path: &Path,
        remote_path: &Path,
        state_path: &Path,
        workers: usize,
    ) -> Result<()> {
        let name =
            leaf_name(remote_path).ok_or_else(|| anyhow!("Filen remote path has no filename"))?;
        let parent = remote_path.parent().unwrap_or_else(|| Path::new("/"));
        let size = std::fs::metadata(local_path)
            .map_err(|e| anyhow!("reading resumable upload source: {e}"))?
            .len();
        provider_client::logged("storage.filen.upload_resumable", BACKEND, MODE, |log| {
            log.step(
                "plan",
                serde_json::json!({ "bytes": size, "count": workers as u64 }),
            );
            if workers > 1 {
                log.warn("filen_resume_serial", serde_json::Value::Null);
            }
            self.with_client(Some(log), |session, client| {
                let parent_uuid = ensure_folder(client, session, parent)?;
                let mut state = match FilenNativeClient::load_upload_resume_state(state_path)? {
                    Some(state) => state,
                    None => client.begin_upload(&parent_uuid, &name, MIME, size)?,
                };
                anyhow::ensure!(state.parent == parent_uuid, "resume state parent mismatch");
                anyhow::ensure!(state.name == name, "resume state filename mismatch");
                anyhow::ensure!(state.size == size, "resume state size mismatch");
                client.resume_upload_from_reader(&mut state, std::fs::File::open(local_path)?)?;
                FilenNativeClient::clear_upload_resume_state(state_path)
            })
        })
    }
    fn delete(&self, path: &Path) -> Result<()> {
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, path)?;
            client.trash(&item.uuid, kind(&item))
        })
    }

    fn restore_deleted(&self, trash_path: &Path, _destination: Option<&Path>) -> Result<()> {
        let name =
            leaf_name(trash_path).ok_or_else(|| anyhow!("Filen trash path has no item name"))?;
        self.with_client(None, |_session, client| {
            // The trash changes outside this client (other devices, the web
            // app), so never answer this from the listing cache.
            let item = client
                .list_folder_fresh("trash")?
                .into_iter()
                .find(|item| item.name == name)
                .ok_or_else(|| anyhow!("Filen trash item not found: {}", trash_path.display()))?;
            client.restore(&item.uuid, kind(&item))
        })
    }
    fn stat(&self, path: &Path) -> Result<FileStat> {
        let item =
            self.with_client(None, |session, client| resolve_fresh(client, session, path))?;
        Ok(FileStat {
            size: item.size,
            is_dir: item.is_dir,
            // Filen stores metadata timestamps in milliseconds; CloudDrive
            // exposes Unix seconds. Zero is the native client's sentinel for
            // folders or unavailable gateway metadata.
            mtime_unix: (item.modified > 0).then_some(item.modified / 1_000),
        })
    }
    fn drive_type(&self) -> DriveType {
        DriveType::Filen
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
            ensure_folder(client, session, path).map(|_| ())
        })
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("/"));
        let destination_name = leaf_name(destination).ok_or_else(|| {
            anyhow!(
                "move destination has no filename: {}",
                destination.display()
            )
        })?;
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, source)?;
            let destination_uuid = ensure_folder(client, session, destination_parent)?;
            if item.name != destination_name {
                client.rename_item(&item, &destination_name)?;
            }
            if item.parent != destination_uuid {
                client.move_item(&item.uuid, &destination_uuid, item.is_dir)?;
            }
            Ok(())
        })
    }

    fn copy_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("/"));
        let destination_name = leaf_name(destination).ok_or_else(|| {
            anyhow!(
                "copy destination has no filename: {}",
                destination.display()
            )
        })?;
        self.with_client(None, |session, client| {
            let item = resolve_fresh(client, session, source)?;
            let destination_uuid = ensure_folder(client, session, destination_parent)?;
            let copied_uuid = client.copy_item(&item, &destination_uuid)?;
            if item.name != destination_name {
                let copied = client
                    .list_folder_fresh(&destination_uuid)?
                    .into_iter()
                    .find(|candidate| candidate.uuid == copied_uuid)
                    .ok_or_else(|| anyhow!("copied Filen item was not returned by the gateway"))?;
                client.rename_item(&copied, &destination_name)?;
            }
            Ok(())
        })
    }

    fn read_file_to_writer(&self, path: &Path, mut writer: &mut dyn Write) -> Result<u64> {
        self.with_client(None, |session, client| {
            let item = Self::resolve_file(client, session, path)?;
            client.download_file_to_writer(&item, &mut writer)
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
                "Filen resumable download path is a directory: {}",
                remote_path.display()
            );
            client.download_file_to_path_resumable(&item, local_path, state_path)
        })
    }

    fn write_file_from_reader(&self, path: &Path, reader: &mut dyn Read, size: u64) -> Result<()> {
        self.write_with(path, size, |client, target| match target {
            WriteTarget::Replace(existing) => {
                client.replace_file_from_reader(existing, MIME, size, reader)
            }
            WriteTarget::Create { parent, name } => {
                client.upload_file_from_reader(parent, name, MIME, size, reader)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_session_is_reported_without_network() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeFilenDrive::from_keychain("Native Filen", "missing-filendrive");
        assert_eq!(drive.drive_type(), DriveType::Filen);
        let capabilities = drive.capabilities();
        assert!(capabilities.streaming);
        assert!(capabilities.resumable_upload);
        assert!(capabilities.resumable_download);
        assert!(format!("{}", drive.list_dir(Path::new("/")).unwrap_err())
            .contains("native Filen session"));
    }

    #[test]
    fn capabilities_include_native_mutations_without_network() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeFilenDrive::from_keychain("Native Filen", "missing-filendrive");
        let capabilities = drive.capabilities();
        assert!(capabilities.create_dir);
        assert!(capabilities.rename);
        assert!(capabilities.move_path);
        assert!(capabilities.copy);
        assert!(capabilities.streaming);
        assert!(!capabilities.versions);
    }

    #[test]
    fn missing_session_is_not_cached_and_is_reread_after_login() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeFilenDrive::from_keychain("Native Filen", "filen-late-login");
        assert!(drive.stat(Path::new("/a")).is_err());
        assert!(!drive.clients.is_cached());
        // A session stored after the failure is read on the next call; the
        // root resolves without any network access.
        let session = FilenSession {
            gateway_url: "http://127.0.0.1:1".into(),
            ingest_url: "http://127.0.0.1:1".into(),
            egest_url: "http://127.0.0.1:1".into(),
            email: "test@example.test".into(),
            api_key: "test-api-key".into(),
            auth_version: 2,
            file_encryption_version: 2,
            metadata_encryption_version: 2,
            root_folder_uuid: "root".into(),
            master_keys: vec![
                b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_vec(),
            ],
            dek: None,
            kek: None,
            private_key: None,
            hmac_key: None,
        };
        crate::drives::secret::set_session("filen-late-login", &session.encode().unwrap()).unwrap();
        let root = drive.stat(Path::new("/")).unwrap();
        assert!(root.is_dir);
        assert!(drive.clients.is_cached());
        crate::drives::secret::delete_session("filen-late-login").unwrap();
    }

    #[test]
    fn inverted_ranges_fail_before_touching_the_session() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeFilenDrive::from_keychain("Native Filen", "missing-filendrive");
        let error = drive.read_range(Path::new("/a"), 5, 2).unwrap_err();
        assert!(error.to_string().contains("invalid byte range"));
        assert!(!drive.clients.is_cached());
    }
}
