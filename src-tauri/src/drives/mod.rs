//! P11 Pillar 5 — Cloud drive abstraction.
//!
//! `trait CloudDrive` is the uniform interface for anything that can hold
//! files: a local/OS-mounted path (covers SMB / SFTP via mount), a Filen
//! folder, an Internxt drive, etc.
//!
//! First-cut implementations:
//!   * `LocalDrive`  — delegates to `std::fs`; covers local paths, NFS mounts,
//!     SMB-mounted shares (`/Volumes/…` on macOS, `\\server\share` on Windows).
//!
//! Future implementations:
//!   * `FilenDrive`    — subprocess to `filen-cli`
//!   * `InternxtDrive` — subprocess to `internxt-cli`
//!   * `SftpDrive`     — direct SSH via `russh` or OS SFTP mount
//!
//! `DriveRegistry` holds the user-configured drives.  Each drive has a
//! stable `id` (UUID), a human label, and a type tag.  The registry is
//! serialised to `{data_dir}/drives.json` so it survives app restarts.

pub mod filen;
#[cfg(feature = "drive-filen-native")]
pub mod filen_native_drive;
pub mod fuse_mount;
pub mod google_drive;
pub mod internxt;
#[cfg(feature = "drive-internxt-native")]
pub mod internxt_native;
#[cfg(feature = "drive-internxt-native")]
pub mod internxt_native_drive;
pub(crate) mod oauth;
pub mod onedrive;
pub(crate) mod secret;
pub mod tauri_commands;
pub mod webdav;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// ── Platform support for the subprocess-backed drives ──────────────────────

/// Guard for the drives that work by spawning a Python CLI (Filen, Internxt).
///
/// iOS and Android cannot run them **at all**: the sandbox denies `fork`/`exec`,
/// so the `posix_spawn` behind `std::process::Command` fails with EPERM, and
/// neither platform ships a Python interpreter we are allowed to execute —
/// App Review 2.5.2 also bars shipping one to run third-party code. Before this
/// guard existed the drive picker offered both kinds on iOS and they failed at
/// use time with a raw spawn error.
///
/// The modules stay compiled on every target (they build fine; it is the
/// *runtime* that refuses), so the registry dispatch and its tests are
/// platform-independent. `src/lib/platform.ts` hides the matching UI options,
/// exactly as it already does for the Ollama / llama.cpp / MLX sidecars; this
/// is the backstop for anything that gets past the UI.
///
/// Note also that the Mac App Store build is sandboxed, where exec'ing a
/// user-chosen interpreter outside the container is denied without a
/// temporary-exception entitlement. These drives are a direct-download feature.
/// Tracked in PLAN.md: native Rust replacements would lift both limits.
pub(crate) fn ensure_subprocess_drives_supported(kind: &str) -> Result<()> {
    if cfg!(any(target_os = "ios", target_os = "android")) {
        return Err(anyhow!(unsupported_drive_message(kind)));
    }
    Ok(())
}

/// Split out from the `cfg!` above so the wording is assertable on every
/// target. Left inline, only the *polarity* of the guard would be covered —
/// and only on whichever platform happens to be running the tests, which is
/// never the one the guard exists for.
fn unsupported_drive_message(kind: &str) -> String {
    format!(
        "the {kind} drive needs to run a Python CLI as a subprocess, which \
         iOS and Android do not permit — use it from a desktop build, or \
         reach the same storage over WebDAV"
    )
}

// ── Trait ──────────────────────────────────────────────────────────────────

/// Operations a drive can safely perform without probing it first.
///
/// The capability set is intentionally explicit: a provider may implement
/// listing and byte transfers while not supporting server-side rename, copy,
/// or streaming.  Callers must check this before rendering destructive UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriveCapabilities {
    pub list: bool,
    pub read: bool,
    pub write: bool,
    pub delete: bool,
    pub stat: bool,
    pub create_dir: bool,
    pub rename: bool,
    pub move_path: bool,
    pub copy: bool,
    pub streaming: bool,
    pub resumable_upload: bool,
    pub resumable_download: bool,
    pub share_links: bool,
    pub versions: bool,
}

impl DriveCapabilities {
    fn basic() -> Self {
        Self {
            list: true,
            read: true,
            write: true,
            delete: true,
            stat: true,
            create_dir: false,
            rename: false,
            move_path: false,
            copy: false,
            streaming: false,
            resumable_upload: false,
            resumable_download: false,
            share_links: false,
            versions: false,
        }
    }
}

/// A file-system-like storage backend.
pub trait CloudDrive: Send + Sync {
    /// Human-readable label for the drive.
    fn label(&self) -> &str;

    /// List directory entries.  Returns relative names (not full paths).
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    /// Read a file's bytes.
    fn read_file(&self, path: &Path) -> Result<Vec<u8>>;

    /// Read an inclusive byte range when the provider can guarantee partial
    /// content semantics. Providers that cannot do this fail explicitly.
    fn read_range(&self, _path: &Path, _start: u64, _end: u64) -> Result<Vec<u8>> {
        Err(anyhow!("{} does not support range reads", self.drive_type().label()))
    }

    /// Write bytes to a path (creates parent directories if needed).
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()>;

    /// Resume a native-provider upload from a durable provider-owned state
    /// file. Legacy providers deliberately reject this instead of silently
    /// restarting with new encryption/session material.
    fn upload_file_resumable(
        &self,
        local_path: &Path,
        remote_path: &Path,
        state_path: &Path,
        workers: usize,
    ) -> Result<()> {
        let _ = (local_path, remote_path, state_path, workers);
        Err(anyhow!(
            "{} does not support durable resumable uploads",
            self.drive_type().label()
        ))
    }

    /// Resume a native-provider download into a durable local checkpoint.
    fn download_file_resumable(
        &self,
        remote_path: &Path,
        local_path: &Path,
        state_path: &Path,
    ) -> Result<()> {
        let _ = (remote_path, local_path, state_path);
        Err(anyhow!(
            "{} does not support durable resumable downloads",
            self.drive_type().label()
        ))
    }

    /// Stream a remote file into a caller-provided writer. Legacy providers
    /// fall back to their whole-buffer API; native streaming providers
    /// override this without changing the object-safe trait boundary.
    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        let data = self.read_file(path)?;
        writer.write_all(&data)?;
        Ok(data.len() as u64)
    }

    /// Upload from a caller-provided reader. The exact plaintext size is
    /// required by encrypted gateways. Legacy providers use a checked,
    /// bounded-by-size fallback buffer; native providers stream directly.
    fn write_file_from_reader(&self, path: &Path, reader: &mut dyn Read, size: u64) -> Result<()> {
        let mut data = Vec::new();
        reader.take(size).read_to_end(&mut data)?;
        anyhow::ensure!(
            data.len() as u64 == size,
            "reader ended before declared size"
        );
        let mut extra = [0u8; 1];
        anyhow::ensure!(
            reader.read(&mut extra)? == 0,
            "reader has data beyond declared size"
        );
        self.write_file(path, &data)
    }

    /// Delete a file or empty directory.
    fn delete(&self, path: &Path) -> Result<()>;

    /// File/directory metadata.
    fn stat(&self, path: &Path) -> Result<FileStat>;

    /// Underlying type for display.
    fn drive_type(&self) -> DriveType;

    /// Return the operations this backend implements.
    fn capabilities(&self) -> DriveCapabilities {
        let mut capabilities = DriveCapabilities::basic();
        capabilities.share_links = matches!(
            self.drive_type(),
            DriveType::OneDrive | DriveType::GoogleDrive
        );
        capabilities.versions = capabilities.share_links;
        capabilities
    }

    /// Probe provider-advertised capabilities when a backend has a safe,
    /// provider-specific discovery endpoint. The default is deliberately
    /// side-effect free and preserves the static capability set.
    fn probed_capabilities(&self) -> DriveCapabilities {
        self.capabilities()
    }

    /// Create a directory, including missing parents where the provider
    /// supports it.
    fn create_dir(&self, _path: &Path) -> Result<()> {
        Err(anyhow!(
            "{} does not support create_dir",
            self.drive_type().label()
        ))
    }

    /// Rename/move a path within the same provider.
    fn move_path(&self, _source: &Path, _destination: &Path) -> Result<()> {
        Err(anyhow!(
            "{} does not support move_path",
            self.drive_type().label()
        ))
    }

    /// Copy a path within the same provider.
    fn copy_path(&self, _source: &Path, _destination: &Path) -> Result<()> {
        Err(anyhow!(
            "{} does not support copy",
            self.drive_type().label()
        ))
    }

    // ── P29.5: Share links ───────────────────────────────────────────

    /// Generate a public share link for a file.  Returns `None` when the
    /// provider does not support sharing.  Default: not supported.
    fn share_link(&self, _path: &Path) -> Result<Option<String>> {
        Ok(None)
    }

    // ── P29.6: Version history ───────────────────────────────────────

    /// List version history for a file.  Returns empty when the provider
    /// does not support versioning.  Default: empty.
    fn list_versions(&self, _path: &Path) -> Result<Vec<FileVersion>> {
        Ok(Vec::new())
    }

    /// Restore a previous version of a file.  Default: not supported.
    fn restore_version(&self, _path: &Path, _version_id: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "{} does not support version restore",
            self.drive_type().label()
        ))
    }
}

// ── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriveType {
    /// OS-visible path (local disk, NFS, SMB mount, SFTP-fuse).
    Local,
    /// Filen cloud storage (via filen-cli subprocess).
    Filen,
    /// Internxt cloud storage (via internxt-cli subprocess).
    Internxt,
    /// Raw SFTP (future — via russh or OS mount).
    Sftp,
    /// Generic WebDAV server (Nextcloud, ownCloud, mailbox.org,
    /// `filen webdav-start`, `internxt webdav-enable`, Synology DSM, …).
    WebDav,
    /// Microsoft OneDrive / SharePoint via Microsoft Graph API.
    OneDrive,
    /// Google Drive via Drive API v3.
    GoogleDrive,
}

impl DriveType {
    /// Human-readable label for error messages.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Filen => "Filen",
            Self::Internxt => "Internxt",
            Self::Sftp => "SFTP",
            Self::WebDav => "WebDAV",
            Self::OneDrive => "OneDrive",
            Self::GoogleDrive => "Google Drive",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// A single version of a file (P29.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersion {
    /// Provider-specific version identifier.
    pub id: String,
    /// When this version was last modified (epoch seconds).
    pub modified_at: Option<i64>,
    /// Size of this version in bytes.
    pub size: Option<u64>,
    /// Name of the person who modified this version (if available).
    pub modifier_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
    pub mtime_unix: Option<i64>,
}

/// A flat row produced by `walk` — one per file (or per dir, if requested).
/// `path` is the full path relative to the drive root, so callers can pass
/// it back to `read_file` / `stat` directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub mtime_unix: Option<i64>,
}

/// Recursively walk a drive starting at `root`, returning one entry per
/// file (folders are descended into but not emitted, by default).
/// Stops at `max_depth` (counting from `root`), `None` = unbounded.
///
/// This is a free function rather than a default trait method so we can
/// keep the trait object-safe (associated functions with `Self: Sized`
/// would prevent `Box<dyn CloudDrive>` from being usable as the receiver).
pub fn walk(
    drive: &dyn CloudDrive,
    root: &Path,
    max_depth: Option<usize>,
    on_error: &mut dyn FnMut(&Path, anyhow::Error),
) -> Vec<WalkEntry> {
    walk_limited(drive, root, max_depth, None, on_error)
}

/// Recursive walk with an optional file-entry scan budget. The budget is
/// useful for interactive provider search, where a result limit must also
/// prevent enumerating an unbounded remote tree.
pub fn walk_limited(
    drive: &dyn CloudDrive,
    root: &Path,
    max_depth: Option<usize>,
    max_files: Option<usize>,
    on_error: &mut dyn FnMut(&Path, anyhow::Error),
) -> Vec<WalkEntry> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if max_files.is_some_and(|limit| out.len() >= limit) {
            break;
        }
        if let Some(max) = max_depth {
            if depth > max {
                continue;
            }
        }
        let entries = match drive.list_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                on_error(&dir, e);
                continue;
            }
        };
        for ent in entries {
            if max_files.is_some_and(|limit| out.len() >= limit) {
                break;
            }
            // Build the full path relative to the drive root.
            let full = if dir.as_os_str().is_empty() {
                PathBuf::from(&ent.name)
            } else {
                dir.join(&ent.name)
            };
            if ent.is_dir {
                stack.push((full, depth + 1));
            } else {
                // Stat for mtime; tolerate failure (some drives are flaky
                // about modificationTime on individual files).
                let mtime = drive.stat(&full).ok().and_then(|s| s.mtime_unix);
                out.push(WalkEntry {
                    path: full,
                    is_dir: false,
                    size: ent.size,
                    mtime_unix: mtime,
                });
            }
        }
    }
    out
}

// ── LocalDrive ──────────────────────────────────────────────────────────────

/// Delegates all operations to `std::fs`.  Covers any path the OS can see:
/// local disks, NFS/SMB/SFTP mounts.
pub struct LocalDrive {
    label: String,
    root: PathBuf,
}

impl LocalDrive {
    pub fn new(label: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            root: root.into(),
        }
    }

    fn full(&self, rel: &Path) -> PathBuf {
        if rel.is_absolute() {
            rel.to_owned()
        } else {
            self.root.join(rel)
        }
    }
}

impl CloudDrive for LocalDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn drive_type(&self) -> DriveType {
        DriveType::Local
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            copy: true,
            ..DriveCapabilities::basic()
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let p = self.full(path);
        let rd = std::fs::read_dir(&p).with_context(|| format!("list_dir: {}", p.display()))?;
        let mut entries = Vec::new();
        for e in rd.filter_map(|e| e.ok()) {
            let meta = e.metadata().ok();
            entries.push(DirEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                size: meta.as_ref().filter(|m| !m.is_dir()).map(|m| m.len()),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let p = self.full(path);
        std::fs::read(&p).with_context(|| format!("read_file: {}", p.display()))
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        let p = self.full(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all: {}", parent.display()))?;
        }
        std::fs::write(&p, data).with_context(|| format!("write_file: {}", p.display()))
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let p = self.full(path);
        if p.is_dir() {
            std::fs::remove_dir(&p).with_context(|| format!("remove_dir: {}", p.display()))
        } else {
            std::fs::remove_file(&p).with_context(|| format!("remove_file: {}", p.display()))
        }
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        let p = self.full(path);
        std::fs::create_dir_all(&p).with_context(|| format!("create_dir: {}", p.display()))
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let from = self.full(source);
        let to = self.full(destination);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all: {}", parent.display()))?;
        }
        std::fs::rename(&from, &to)
            .with_context(|| format!("move {} -> {}", from.display(), to.display()))
    }

    fn copy_path(&self, source: &Path, destination: &Path) -> Result<()> {
        fn copy_recursive(source: &Path, destination: &Path) -> Result<()> {
            let metadata = std::fs::metadata(source)
                .with_context(|| format!("copy stat: {}", source.display()))?;
            if metadata.is_dir() {
                std::fs::create_dir_all(destination)
                    .with_context(|| format!("copy mkdir: {}", destination.display()))?;
                for entry in std::fs::read_dir(source)
                    .with_context(|| format!("copy list: {}", source.display()))?
                {
                    let entry =
                        entry.with_context(|| format!("copy entry: {}", source.display()))?;
                    copy_recursive(&entry.path(), &destination.join(entry.file_name()))?;
                }
            } else {
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("copy mkdir: {}", parent.display()))?;
                }
                std::fs::copy(source, destination).with_context(|| {
                    format!("copy {} -> {}", source.display(), destination.display())
                })?;
            }
            Ok(())
        }

        copy_recursive(&self.full(source), &self.full(destination))
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let p = self.full(path);
        let meta = std::fs::metadata(&p).with_context(|| format!("stat: {}", p.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        Ok(FileStat {
            size: meta.len(),
            is_dir: meta.is_dir(),
            mtime_unix: mtime,
        })
    }
}

// ── DriveConfig + Registry ────────────────────────────────────────────────

/// Serialisable config for one drive entry in the registry.
///
/// For backwards compatibility, the auth fields are `#[serde(default)]` so
/// Auth is never serialized.  Provider credentials and OAuth tokens live in
/// the OS keychain, keyed by `id`; this struct only contains non-secret drive
/// metadata and the in-memory compatibility fields used by constructors and
/// tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    pub id: String,
    pub label: String,
    pub kind: DriveType,
    /// For `Local` drives: the root path.
    /// For CLI-based drives: the mount-point path (if OS-mounted) or the
    /// base path used by the CLI tool.
    /// For `WebDav`: the base URL ending in `/` (e.g.
    /// `https://webdav.example.com/dav/`).
    pub path: String,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub username: Option<String>,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub password: Option<String>,
    /// WebDAV-only: skip TLS certificate verification.  Off by default;
    /// flip on for self-signed servers like the local one started by
    /// `internxt-cli webdav-start` (HTTPS to 127.0.0.1 with a generated
    /// cert).  Has no effect on non-WebDAV drives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insecure_tls: Option<bool>,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub access_token: Option<String>,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub refresh_token: Option<String>,
    /// Public OAuth2 client ID; retained in memory only until keychain-backed
    /// OAuth setup is complete.
    #[serde(skip)]
    pub client_id: Option<String>,
    /// Deprecated and intentionally never accepted or persisted.  Public
    /// desktop OAuth clients must use PKCE instead of a bundled secret.
    #[serde(skip)]
    pub client_secret: Option<String>,
}

/// Loads and saves the list of configured drives.
pub struct DriveRegistry {
    path: PathBuf,
    pub drives: Vec<DriveConfig>,
}

impl DriveRegistry {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("drives.json");
        let drives = if path.exists() {
            let json = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let mut raw: serde_json::Value = serde_json::from_str(&json)
                .with_context(|| format!("parsing {}", path.display()))?;
            // One-time migration for pre-keychain files.  Read legacy fields
            // only to move them into the OS keychain, then rewrite the file
            // through the redacted DriveConfig serializer.
            let mut migrated = false;
            if let Some(items) = raw.as_array_mut() {
                for item in items {
                    let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let credentials = crate::drives::secret::DriveCredentials {
                        username: item
                            .get("username")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        password: item
                            .get("password")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        access_token: item
                            .get("access_token")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        refresh_token: item
                            .get("refresh_token")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        client_id: item
                            .get("client_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                    };
                    let had_legacy = credentials.username.is_some()
                        || credentials.password.is_some()
                        || credentials.access_token.is_some()
                        || credentials.refresh_token.is_some()
                        || credentials.client_id.is_some()
                        || item.get("client_secret").is_some();
                    if had_legacy {
                        crate::drives::secret::set_credentials(id, &credentials)
                            .context("migrating drive credentials to the OS keychain")?;
                        for key in [
                            "username",
                            "password",
                            "access_token",
                            "refresh_token",
                            "client_id",
                            "client_secret",
                        ] {
                            if let Some(object) = item.as_object_mut() {
                                object.remove(key);
                            }
                        }
                        migrated = true;
                    }
                }
            }
            let drives = serde_json::from_value::<Vec<DriveConfig>>(raw)
                .with_context(|| format!("parsing {}", path.display()))?;
            if migrated {
                let redacted = serde_json::to_string_pretty(&drives)
                    .context("redacting legacy drive credentials")?;
                std::fs::write(&path, redacted)
                    .with_context(|| format!("rewriting {} without secrets", path.display()))?;
            }
            drives
        } else {
            Vec::new()
        };
        Ok(Self { path, drives })
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.drives).context("serialising drives")?;
        std::fs::write(&self.path, json).with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn add(&mut self, config: DriveConfig) -> Result<()> {
        // Deduplicate by id.
        self.drives.retain(|d| d.id != config.id);
        self.drives.push(config);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<bool> {
        let before = self.drives.len();
        self.drives.retain(|d| d.id != id);
        if self.drives.len() < before {
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Instantiate a drive from its config using the default network policy.
    pub fn instantiate(config: &DriveConfig) -> Box<dyn CloudDrive> {
        Self::instantiate_with_proxy(config, &crate::sync::proxy::ProxyConfig::default())
            .expect("default cloud-drive HTTP client must build")
    }

    /// Instantiate a drive while applying the app-wide proxy policy.
    /// Providers without a native HTTP client (Local/Filen/Internxt CLI)
    /// retain their existing construction path.
    pub fn instantiate_with_proxy(
        config: &DriveConfig,
        proxy: &crate::sync::proxy::ProxyConfig,
    ) -> Result<Box<dyn CloudDrive>> {
        match config.kind {
            DriveType::Local | DriveType::Sftp => {
                // Local + raw SFTP both go through `LocalDrive` for now;
                // SFTP relies on the user mounting the share via OS/FUSE.
                Ok(Box::new(LocalDrive::new(
                    config.label.clone(),
                    PathBuf::from(&config.path),
                )))
            }
            DriveType::Filen => {
                #[cfg(feature = "drive-filen-native")]
                {
                    Ok(Box::new(
                        filen_native_drive::NativeFilenDrive::from_keychain_with_proxy(
                            config.label.clone(),
                            &config.id,
                            proxy,
                        ),
                    ))
                }
                #[cfg(not(feature = "drive-filen-native"))]
                {
                    Ok(Box::new(filen::FilenDrive::new(
                        config.label.clone(),
                        PathBuf::from(&config.path),
                    )))
                }
            }
            DriveType::Internxt => {
                #[cfg(feature = "drive-internxt-native")]
                {
                    Ok(Box::new(
                        internxt_native_drive::NativeInternxtDrive::from_keychain_with_proxy(
                            config.label.clone(),
                            &config.id,
                            proxy,
                        ),
                    ))
                }
                #[cfg(not(feature = "drive-internxt-native"))]
                {
                    Ok(Box::new(internxt::InternxtDrive::new(
                        config.label.clone(),
                        PathBuf::from(&config.path),
                    )))
                }
            }
            DriveType::WebDav => {
                let credentials = crate::drives::secret::get_credentials(&config.id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Ok(Box::new(webdav::WebDavDrive::new_with_proxy(
                    config.label.clone(),
                    config.path.clone(),
                    credentials.username.or_else(|| config.username.clone()),
                    credentials.password.or_else(|| config.password.clone()),
                    config.insecure_tls.unwrap_or(false),
                    proxy,
                )?))
            }
            DriveType::OneDrive => {
                let credentials = crate::drives::secret::get_credentials(&config.id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Ok(Box::new(onedrive::OneDriveDrive::new_with_proxy(
                    config.label.clone(),
                    credentials
                        .access_token
                        .or_else(|| config.access_token.clone())
                        .unwrap_or_default(),
                    credentials
                        .refresh_token
                        .or_else(|| config.refresh_token.clone()),
                    credentials.client_id.or_else(|| config.client_id.clone()),
                    None,
                    proxy,
                )?))
            }
            DriveType::GoogleDrive => {
                let credentials = crate::drives::secret::get_credentials(&config.id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Ok(Box::new(google_drive::GoogleDriveDrive::new_with_proxy(
                    config.label.clone(),
                    credentials
                        .access_token
                        .or_else(|| config.access_token.clone())
                        .unwrap_or_default(),
                    credentials
                        .refresh_token
                        .or_else(|| config.refresh_token.clone()),
                    credentials.client_id.or_else(|| config.client_id.clone()),
                    None,
                    proxy,
                )?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture() -> (tempfile::TempDir, LocalDrive) {
        let tmp = tempfile::tempdir().unwrap();
        let drive = LocalDrive::new("test", tmp.path().to_owned());
        (tmp, drive)
    }

    struct CapabilityStub;

    impl CloudDrive for CapabilityStub {
        fn label(&self) -> &str {
            "capability-stub"
        }

        fn list_dir(&self, _path: &Path) -> Result<Vec<DirEntry>> {
            Ok(Vec::new())
        }

        fn read_file(&self, _path: &Path) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<()> {
            Ok(())
        }

        fn delete(&self, _path: &Path) -> Result<()> {
            Ok(())
        }

        fn stat(&self, _path: &Path) -> Result<FileStat> {
            Ok(FileStat {
                size: 0,
                is_dir: false,
                mtime_unix: None,
            })
        }

        fn drive_type(&self) -> DriveType {
            DriveType::WebDav
        }
    }

    #[test]
    fn local_drive_label_and_type() {
        let (_tmp, drive) = fixture();
        assert_eq!(drive.label(), "test");
        assert_eq!(drive.drive_type(), DriveType::Local);
    }

    #[test]
    fn drive_config_serialization_redacts_all_auth_fields() {
        let config = DriveConfig {
            id: "redaction".into(), label: "Redaction".into(), kind: DriveType::WebDav,
            path: "https://example.invalid/dav/".into(), username: Some("alice".into()),
            password: Some("password".into()), insecure_tls: Some(false),
            access_token: Some("access-token".into()), refresh_token: Some("refresh-token".into()),
            client_id: Some("public-client".into()), client_secret: Some("must-not-ship".into()),
        };
        let json = serde_json::to_string(&config).unwrap();
        for secret in ["alice", "password", "access-token", "refresh-token", "public-client", "must-not-ship"] {
            assert!(!json.contains(secret), "serialized drive config leaked {secret}");
        }
        assert!(json.contains("redaction"));
    }

    #[test]
    fn local_drive_write_then_read_round_trips() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("hello.txt"), b"world").unwrap();
        let bytes = drive.read_file(Path::new("hello.txt")).unwrap();
        assert_eq!(bytes, b"world");
    }

    #[test]
    fn streaming_facade_round_trips_with_legacy_provider_fallback() {
        let (_tmp, drive) = fixture();
        let mut input = Cursor::new(b"streamed".to_vec());
        drive
            .write_file_from_reader(Path::new("stream.txt"), &mut input, 8)
            .unwrap();
        let mut output = Vec::new();
        assert_eq!(
            drive
                .read_file_to_writer(Path::new("stream.txt"), &mut output)
                .unwrap(),
            8
        );
        assert_eq!(output, b"streamed");
    }

    #[test]
    fn legacy_resume_operations_fail_explicitly_and_do_not_probe_provider() {
        let (_tmp, drive) = fixture();
        let error = drive
            .upload_file_resumable(
                Path::new("local.bin"),
                Path::new("remote.bin"),
                Path::new("state.json"),
                1,
            )
            .expect_err("legacy local drive must reject resumable uploads");
        assert!(error
            .to_string()
            .contains("does not support durable resumable uploads"));

        let error = drive
            .download_file_resumable(
                Path::new("remote.bin"),
                Path::new("local.bin"),
                Path::new("state.json"),
            )
            .expect_err("legacy local drive must reject resumable downloads");
        assert!(error
            .to_string()
            .contains("does not support durable resumable downloads"));
    }

    #[test]
    fn default_unsupported_operations_match_the_capability_contract() {
        let drive = CapabilityStub;
        let capabilities = drive.capabilities();
        assert!(!capabilities.create_dir);
        assert!(!capabilities.rename);
        assert!(!capabilities.move_path);
        assert!(!capabilities.copy);
        assert!(!capabilities.share_links);
        assert!(!capabilities.versions);

        assert!(drive.create_dir(Path::new("new")).is_err());
        assert!(drive
            .move_path(Path::new("old"), Path::new("new"))
            .is_err());
        assert!(drive
            .copy_path(Path::new("old"), Path::new("new"))
            .is_err());
        assert_eq!(drive.share_link(Path::new("file")).unwrap(), None);
        assert!(drive.list_versions(Path::new("file")).unwrap().is_empty());
        assert!(drive.restore_version(Path::new("file"), "v1").is_err());
    }

    #[test]
    fn streaming_contract_rejects_short_and_overlong_readers() {
        let (_tmp, drive) = fixture();
        let mut short = Cursor::new(b"short".to_vec());
        assert!(drive
            .write_file_from_reader(Path::new("short.txt"), &mut short, 6)
            .is_err());

        let mut overlong = Cursor::new(b"overlong".to_vec());
        assert!(drive
            .write_file_from_reader(Path::new("overlong.txt"), &mut overlong, 4)
            .is_err());
    }

    #[test]
    fn local_drive_creates_parent_dirs_on_write() {
        let (_tmp, drive) = fixture();
        drive
            .write_file(Path::new("nested/deep/file.txt"), b"x")
            .unwrap();
        assert_eq!(
            drive.read_file(Path::new("nested/deep/file.txt")).unwrap(),
            b"x"
        );
    }

    #[test]
    fn local_drive_list_dir_sorted() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("zzz.txt"), b"z").unwrap();
        drive.write_file(Path::new("aaa.txt"), b"a").unwrap();
        drive.write_file(Path::new("mmm/inner.txt"), b"m").unwrap();
        let entries = drive.list_dir(Path::new(".")).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "aaa.txt");
        assert_eq!(entries[1].name, "mmm");
        assert_eq!(entries[2].name, "zzz.txt");
        assert!(entries[1].is_dir);
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, Some(1));
        assert_eq!(entries[1].size, None); // dirs report no size
    }

    #[test]
    fn local_drive_stat_file_and_dir() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("a.txt"), b"hello").unwrap();
        let s = drive.stat(Path::new("a.txt")).unwrap();
        assert_eq!(s.size, 5);
        assert!(!s.is_dir);
        assert!(s.mtime_unix.is_some());

        drive.write_file(Path::new("subdir/x.txt"), b"x").unwrap();
        let d = drive.stat(Path::new("subdir")).unwrap();
        assert!(d.is_dir);
    }

    #[test]
    fn local_drive_delete_file_and_dir() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("doomed.txt"), b"!").unwrap();
        drive.delete(Path::new("doomed.txt")).unwrap();
        assert!(drive.stat(Path::new("doomed.txt")).is_err());
    }

    #[test]
    fn local_drive_read_missing_file_errors() {
        let (_tmp, drive) = fixture();
        assert!(drive.read_file(Path::new("does/not/exist")).is_err());
    }

    #[test]
    fn local_drive_capabilities_include_safe_mutations() {
        let (_tmp, drive) = fixture();
        let caps = drive.capabilities();
        assert!(caps.create_dir);
        assert!(caps.rename && caps.move_path && caps.copy);
        assert!(!caps.share_links);
    }

    #[test]
    fn provider_capability_matrix_is_explicit_without_network_or_keychain() {
        // Instantiate every non-native provider with inert configuration and
        // inspect only its declaration.  This is deliberately a pure contract
        // test: capability discovery must not spawn a CLI, make HTTP calls,
        // or consult credentials/keychain state.
        let cases = [
            (
                DriveType::Local,
                true,
                true,
                true,
                true,
                false,
                false,
                false,
                false,
            ),
            (
                DriveType::Filen,
                true,
                true,
                true,
                true,
                false,
                false,
                cfg!(feature = "drive-filen-native"),
                cfg!(feature = "drive-filen-native"),
            ),
            (
                DriveType::Internxt,
                true,
                true,
                true,
                // copy: NOT native-only. `InternxtDrive::copy_path` shells out
                // to `internxt cp` on the CLI path, and `capabilities()`
                // declares `copy: true` unconditionally — so gating the
                // expectation on the native feature made this row disagree with
                // the driver in exactly the default build. Same stale-matrix bug
                // as the OneDrive row below; streaming and resumable transfers
                // below genuinely are native-only and stay gated.
                true,
                false,
                false,
                cfg!(feature = "drive-internxt-native"),
                cfg!(feature = "drive-internxt-native"),
            ),
            (
                DriveType::WebDav,
                true,
                true,
                true,
                true,
                false,
                false,
                true,
                false,
            ),
            (
                DriveType::OneDrive,
                true,
                true,
                true,
                // copy: Graph exposes an async copy endpoint and
                // `OneDriveDrive::copy_path` implements it, so the driver
                // reports `copy: true` — as does its own unit test
                // `capabilities_include_graph_mutations_and_copy`. This row said
                // `false` and had been failing since the driver gained copy;
                // nobody saw it because the lib test target did not compile.
                true,
                true,
                true,
                false,
                false,
            ),
            (
                DriveType::GoogleDrive,
                true,
                true,
                true,
                true,
                true,
                true,
                false,
                false,
            ),
        ];

        for (
            kind,
            create_dir,
            rename,
            move_path,
            copy,
            share_links,
            versions,
            streaming,
            resumable,
        ) in cases
        {
            let config = DriveConfig {
                id: format!("capability-{kind:?}"),
                label: format!("capability-{kind:?}"),
                kind: kind.clone(),
                path: "https://example.invalid/drive/".to_owned(),
                username: None,
                password: None,
                insecure_tls: None,
                access_token: Some("unit-test-token".to_owned()),
                refresh_token: None,
                client_id: None,
                client_secret: None,
            };
            let caps = DriveRegistry::instantiate(&config).capabilities();
            assert_eq!(caps.create_dir, create_dir, "{kind:?} create_dir");
            assert_eq!(caps.rename, rename, "{kind:?} rename");
            assert_eq!(caps.move_path, move_path, "{kind:?} move_path");
            assert_eq!(caps.copy, copy, "{kind:?} copy");
            assert_eq!(caps.share_links, share_links, "{kind:?} share_links");
            assert_eq!(caps.versions, versions, "{kind:?} versions");
            assert_eq!(caps.streaming, streaming, "{kind:?} streaming");
            assert_eq!(
                caps.resumable_upload, resumable,
                "{kind:?} resumable upload"
            );
            assert_eq!(
                caps.resumable_download, resumable,
                "{kind:?} resumable download"
            );
        }
    }

    #[test]
    fn local_drive_mutations_cover_directory_move_and_recursive_copy() {
        let (_tmp, drive) = fixture();
        drive.create_dir(Path::new("source/nested")).unwrap();
        drive
            .write_file(Path::new("source/nested/file.txt"), b"payload")
            .unwrap();
        drive
            .copy_path(Path::new("source"), Path::new("copied"))
            .unwrap();
        assert_eq!(
            drive
                .read_file(Path::new("copied/nested/file.txt"))
                .unwrap(),
            b"payload"
        );
        drive
            .move_path(Path::new("copied"), Path::new("moved"))
            .unwrap();
        assert_eq!(
            drive.read_file(Path::new("moved/nested/file.txt")).unwrap(),
            b"payload"
        );
        assert!(drive.stat(Path::new("copied")).is_err());
    }

    #[test]
    fn drive_registry_persists_across_open_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = DriveConfig {
            id: "abc-123".to_owned(),
            label: "Backup SSD".to_owned(),
            kind: DriveType::Local,
            path: "/Volumes/Backup".to_owned(),
            username: None,
            password: None,
            insecure_tls: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        };
        {
            let mut reg = DriveRegistry::open(tmp.path()).unwrap();
            assert_eq!(reg.drives.len(), 0);
            reg.add(cfg.clone()).unwrap();
        }
        // Re-open and verify persistence.
        let reg = DriveRegistry::open(tmp.path()).unwrap();
        assert_eq!(reg.drives.len(), 1);
        assert_eq!(reg.drives[0].id, "abc-123");
        assert_eq!(reg.drives[0].label, "Backup SSD");
        assert_eq!(reg.drives[0].kind, DriveType::Local);
    }

    #[test]
    fn drive_registry_dedupes_by_id_on_add() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = DriveRegistry::open(tmp.path()).unwrap();
        reg.add(DriveConfig {
            id: "x".into(),
            label: "v1".into(),
            kind: DriveType::Local,
            path: "/a".into(),
            username: None,
            password: None,
            insecure_tls: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        })
        .unwrap();
        // Same id, different label → must replace not duplicate.
        reg.add(DriveConfig {
            id: "x".into(),
            label: "v2".into(),
            kind: DriveType::Local,
            path: "/b".into(),
            username: None,
            password: None,
            insecure_tls: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        })
        .unwrap();
        assert_eq!(reg.drives.len(), 1);
        assert_eq!(reg.drives[0].label, "v2");
        assert_eq!(reg.drives[0].path, "/b");
    }

    #[test]
    fn drive_registry_remove_returns_found_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = DriveRegistry::open(tmp.path()).unwrap();
        reg.add(DriveConfig {
            id: "abc".into(),
            label: "a".into(),
            kind: DriveType::Local,
            path: "/a".into(),
            username: None,
            password: None,
            insecure_tls: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        })
        .unwrap();
        assert!(reg.remove("abc").unwrap());
        assert!(!reg.remove("abc").unwrap()); // already gone
        assert!(!reg.remove("never-existed").unwrap());
        assert_eq!(reg.drives.len(), 0);
    }

    #[test]
    fn drive_type_serializes_snake_case() {
        let json = serde_json::to_string(&DriveType::Local).unwrap();
        assert_eq!(json, "\"local\"");
        let json = serde_json::to_string(&DriveType::Filen).unwrap();
        assert_eq!(json, "\"filen\"");
        // Round-trips.
        let back: DriveType = serde_json::from_str("\"internxt\"").unwrap();
        assert_eq!(back, DriveType::Internxt);
    }

    #[test]
    fn the_unsupported_drive_message_is_actionable() {
        // Runs on every target, unlike the polarity check below: names the
        // drive rather than leaving the user with a raw EPERM, says which
        // platforms are affected, and points at the way out.
        let msg = unsupported_drive_message("Filen");
        assert!(msg.contains("Filen"), "should name the drive: {msg}");
        assert!(
            msg.contains("iOS") && msg.contains("Android"),
            "should say which platforms cannot do this: {msg}"
        );
        assert!(
            msg.contains("WebDAV"),
            "should offer the alternative: {msg}"
        );
    }

    #[test]
    fn subprocess_drives_are_refused_on_mobile_and_allowed_on_desktop() {
        // Only one arm can run per target, so this covers the polarity and
        // nothing else. On desktop the guard must be transparent (the drives
        // are a working direct-download feature); on mobile it must refuse
        // before any spawn is attempted. That the refusal *would* otherwise be
        // an EPERM from the sandbox is platform behaviour our tests cannot
        // reach — it is why the guard exists rather than something it proves.
        let got = ensure_subprocess_drives_supported("Filen");
        if cfg!(any(target_os = "ios", target_os = "android")) {
            assert!(got.is_err(), "mobile must refuse the subprocess drives");
        } else {
            assert!(got.is_ok(), "desktop must keep working: {got:?}");
        }
    }

    #[test]
    fn registry_instantiate_routes_each_kind_correctly() {
        // Local + Sftp both go through LocalDrive for now (Sftp relies on
        // OS-level mounts).  Filen / Internxt route to their own subprocess
        // drives.  This guards against future refactors silently swapping
        // the dispatch back to LocalDrive.
        for (kind, expected) in [
            (DriveType::Local, DriveType::Local),
            // Sftp piggybacks on LocalDrive (which only knows DriveType::Local),
            // so the instance reports Local even though the config said Sftp.
            (DriveType::Sftp, DriveType::Local),
            (DriveType::Filen, DriveType::Filen),
            (DriveType::Internxt, DriveType::Internxt),
            (DriveType::WebDav, DriveType::WebDav),
            (DriveType::OneDrive, DriveType::OneDrive),
            (DriveType::GoogleDrive, DriveType::GoogleDrive),
        ] {
            let path = if matches!(kind, DriveType::WebDav) {
                "https://example.com/dav/".to_owned()
            } else {
                "/tmp".to_owned()
            };
            let cfg = DriveConfig {
                id: "x".into(),
                label: "lbl".into(),
                kind: kind.clone(),
                path,
                username: None,
                password: None,
                insecure_tls: None,
                access_token: None,
                refresh_token: None,
                client_id: None,
                client_secret: None,
            };
            let drive = DriveRegistry::instantiate(&cfg);
            assert_eq!(
                drive.drive_type(),
                expected,
                "DriveType::{:?} should instantiate to a drive that reports drive_type() == {:?}",
                kind,
                expected
            );
            assert_eq!(drive.label(), "lbl");
        }
    }

    #[test]
    fn drive_type_label_covers_all_variants() {
        let labels = vec![
            (DriveType::Local, "Local"),
            (DriveType::Filen, "Filen"),
            (DriveType::Internxt, "Internxt"),
            (DriveType::Sftp, "SFTP"),
            (DriveType::WebDav, "WebDAV"),
            (DriveType::OneDrive, "OneDrive"),
            (DriveType::GoogleDrive, "Google Drive"),
        ];
        for (dt, expected) in labels {
            assert_eq!(dt.label(), expected);
        }
    }

    #[test]
    fn file_version_serde_round_trips() {
        let v = FileVersion {
            id: "v123".into(),
            modified_at: Some(1720000000),
            size: Some(12345),
            modifier_name: Some("Alice".into()),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: FileVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "v123");
        assert_eq!(back.modified_at, Some(1720000000));
        assert_eq!(back.size, Some(12345));
        assert_eq!(back.modifier_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn local_drive_default_version_methods() {
        let (_tmp, drive) = fixture();
        // Default implementations should return empty/error.
        let versions = drive.list_versions(Path::new("any")).unwrap();
        assert!(versions.is_empty());
        assert!(drive.restore_version(Path::new("any"), "v1").is_err());
        assert!(drive.share_link(Path::new("any")).unwrap().is_none());
    }
}
