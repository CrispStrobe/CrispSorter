//! Provider-neutral filesystem contract and existing OS drive.
use super::mounted_read;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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
    /// The backend can restore an item previously removed by `delete`.
    /// This is separate from `delete`: WebDAV DELETE and local filesystem
    /// removal are not recoverable operations.
    pub reversible_trash: bool,
}

impl DriveCapabilities {
    pub(crate) fn basic() -> Self {
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
            reversible_trash: false,
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
        Err(anyhow!(
            "{} does not support range reads",
            self.drive_type().label()
        ))
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

    /// Restore an item from the provider's recoverable trash. Backends
    /// without a real restore API fail explicitly instead of pretending that
    /// DELETE can be undone.
    fn restore_deleted(&self, _trash_path: &Path, _destination: Option<&Path>) -> Result<()> {
        Err(anyhow!(
            "{} does not support restoring deleted items",
            self.drive_type().label()
        ))
    }

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
    /// SMB share using the saved mounted or direct Rust transport.
    Smb,
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
            Self::Smb => "SMB",
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

/// Maximum provider file size inspected by opt-in bounded content search.
pub const MAX_CONTENT_SEARCH_BYTES: u64 = 256 * 1024;

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

/// Case-insensitive text probe used by bounded provider content search.
/// Invalid UTF-8 is treated lossily; empty queries never match.
pub fn content_search_matches(bytes: &[u8], query: &str) -> bool {
    !query.is_empty()
        && String::from_utf8_lossy(bytes)
            .to_lowercase()
            .contains(query)
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

    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        let p = self.full(path);
        // In particular, SMB/NFS mounts must not buffer the entire remote file
        // before the destination receives anything. io::copy provides bounded
        // buffering and stops immediately when the caller's writer fails.
        mounted_read::read_to_writer(&p, writer)
            .with_context(|| format!("read_file_to_writer: {}", p.display()))
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
