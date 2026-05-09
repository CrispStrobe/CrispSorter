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
pub mod internxt;
pub mod tauri_commands;
pub mod webdav;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Trait ──────────────────────────────────────────────────────────────────

/// A file-system-like storage backend.
pub trait CloudDrive: Send + Sync {
    /// Human-readable label for the drive.
    fn label(&self) -> &str;

    /// List directory entries.  Returns relative names (not full paths).
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    /// Read a file's bytes.
    fn read_file(&self, path: &Path) -> Result<Vec<u8>>;

    /// Write bytes to a path (creates parent directories if needed).
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()>;

    /// Delete a file or empty directory.
    fn delete(&self, path: &Path) -> Result<()>;

    /// File/directory metadata.
    fn stat(&self, path: &Path) -> Result<FileStat>;

    /// Underlying type for display.
    fn drive_type(&self) -> DriveType;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
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
    pub path:        PathBuf,
    pub is_dir:      bool,
    pub size:        Option<u64>,
    pub mtime_unix:  Option<i64>,
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
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if let Some(max) = max_depth {
            if depth > max { continue; }
        }
        let entries = match drive.list_dir(&dir) {
            Ok(e) => e,
            Err(e) => { on_error(&dir, e); continue; }
        };
        for ent in entries {
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
    root:  PathBuf,
}

impl LocalDrive {
    pub fn new(label: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self { label: label.into(), root: root.into() }
    }

    fn full(&self, rel: &Path) -> PathBuf {
        if rel.is_absolute() { rel.to_owned() } else { self.root.join(rel) }
    }
}

impl CloudDrive for LocalDrive {
    fn label(&self) -> &str { &self.label }
    fn drive_type(&self) -> DriveType { DriveType::Local }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let p = self.full(path);
        let rd = std::fs::read_dir(&p)
            .with_context(|| format!("list_dir: {}", p.display()))?;
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

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let p = self.full(path);
        let meta = std::fs::metadata(&p)
            .with_context(|| format!("stat: {}", p.display()))?;
        let mtime = meta.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        Ok(FileStat { size: meta.len(), is_dir: meta.is_dir(), mtime_unix: mtime })
    }
}

// ── DriveConfig + Registry ────────────────────────────────────────────────

/// Serialisable config for one drive entry in the registry.
///
/// For backwards compatibility, the auth fields are `#[serde(default)]` so
/// existing `drives.json` files (written before WebDAV support) round-trip
/// unchanged.  Auth lives in plaintext inside `drives.json`; users with
/// secret-store ambitions should mount a WebDAV server locally and use a
/// `Local` drive on the FUSE path instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    pub id:    String,
    pub label: String,
    pub kind:  DriveType,
    /// For `Local` drives: the root path.
    /// For CLI-based drives: the mount-point path (if OS-mounted) or the
    /// base path used by the CLI tool.
    /// For `WebDav`: the base URL ending in `/` (e.g.
    /// `https://webdav.example.com/dav/`).
    pub path:  String,
    /// Optional WebDAV basic-auth username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Optional WebDAV basic-auth password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
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
            serde_json::from_str::<Vec<DriveConfig>>(&json)
                .with_context(|| format!("parsing {}", path.display()))?
        } else {
            Vec::new()
        };
        Ok(Self { path, drives })
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.drives)
            .context("serialising drives")?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("writing {}", self.path.display()))
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
        if self.drives.len() < before { self.save()?; Ok(true) } else { Ok(false) }
    }

    /// Instantiate a drive from its config.
    pub fn instantiate(config: &DriveConfig) -> Box<dyn CloudDrive> {
        match config.kind {
            DriveType::Local | DriveType::Sftp => {
                // Local + raw SFTP both go through `LocalDrive` for now;
                // SFTP relies on the user mounting the share via OS/FUSE.
                Box::new(LocalDrive::new(config.label.clone(), PathBuf::from(&config.path)))
            }
            DriveType::Filen => {
                Box::new(filen::FilenDrive::new(config.label.clone(), PathBuf::from(&config.path)))
            }
            DriveType::Internxt => {
                Box::new(internxt::InternxtDrive::new(config.label.clone(), PathBuf::from(&config.path)))
            }
            DriveType::WebDav => {
                Box::new(webdav::WebDavDrive::new(
                    config.label.clone(),
                    config.path.clone(),
                    config.username.clone(),
                    config.password.clone(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, LocalDrive) {
        let tmp = tempfile::tempdir().unwrap();
        let drive = LocalDrive::new("test", tmp.path().to_owned());
        (tmp, drive)
    }

    #[test]
    fn local_drive_label_and_type() {
        let (_tmp, drive) = fixture();
        assert_eq!(drive.label(), "test");
        assert_eq!(drive.drive_type(), DriveType::Local);
    }

    #[test]
    fn local_drive_write_then_read_round_trips() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("hello.txt"), b"world").unwrap();
        let bytes = drive.read_file(Path::new("hello.txt")).unwrap();
        assert_eq!(bytes, b"world");
    }

    #[test]
    fn local_drive_creates_parent_dirs_on_write() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("nested/deep/file.txt"), b"x").unwrap();
        assert_eq!(drive.read_file(Path::new("nested/deep/file.txt")).unwrap(), b"x");
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
        assert_eq!(entries[1].size, None);  // dirs report no size
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
    fn drive_registry_persists_across_open_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = DriveConfig {
            id: "abc-123".to_owned(),
            label: "Backup SSD".to_owned(),
            kind: DriveType::Local,
            path: "/Volumes/Backup".to_owned(),
            username: None,
            password: None,
        };
        {
            let mut reg = DriveRegistry::open(tmp.path()).unwrap();
            assert_eq!(reg.drives.len(), 0);
            reg.add(cfg.clone()).unwrap();
        }
        // Re-open and verify persistence.
        let reg = DriveRegistry::open(tmp.path()).unwrap();
        assert_eq!(reg.drives.len(), 1);
        assert_eq!(reg.drives[0].id,    "abc-123");
        assert_eq!(reg.drives[0].label, "Backup SSD");
        assert_eq!(reg.drives[0].kind,  DriveType::Local);
    }

    #[test]
    fn drive_registry_dedupes_by_id_on_add() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = DriveRegistry::open(tmp.path()).unwrap();
        reg.add(DriveConfig {
            id: "x".into(), label: "v1".into(),
            kind: DriveType::Local, path: "/a".into(),
            username: None, password: None,
        }).unwrap();
        // Same id, different label → must replace not duplicate.
        reg.add(DriveConfig {
            id: "x".into(), label: "v2".into(),
            kind: DriveType::Local, path: "/b".into(),
            username: None, password: None,
        }).unwrap();
        assert_eq!(reg.drives.len(), 1);
        assert_eq!(reg.drives[0].label, "v2");
        assert_eq!(reg.drives[0].path, "/b");
    }

    #[test]
    fn drive_registry_remove_returns_found_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = DriveRegistry::open(tmp.path()).unwrap();
        reg.add(DriveConfig {
            id: "abc".into(), label: "a".into(),
            kind: DriveType::Local, path: "/a".into(),
            username: None, password: None,
        }).unwrap();
        assert!( reg.remove("abc").unwrap());
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
    fn registry_instantiate_routes_each_kind_correctly() {
        // Local + Sftp both go through LocalDrive for now (Sftp relies on
        // OS-level mounts).  Filen / Internxt route to their own subprocess
        // drives.  This guards against future refactors silently swapping
        // the dispatch back to LocalDrive.
        for (kind, expected) in [
            (DriveType::Local,    DriveType::Local),
            // Sftp piggybacks on LocalDrive (which only knows DriveType::Local),
            // so the instance reports Local even though the config said Sftp.
            (DriveType::Sftp,     DriveType::Local),
            (DriveType::Filen,    DriveType::Filen),
            (DriveType::Internxt, DriveType::Internxt),
            (DriveType::WebDav,   DriveType::WebDav),
        ] {
            let path = if matches!(kind, DriveType::WebDav) {
                "https://example.com/dav/".to_owned()
            } else { "/tmp".to_owned() };
            let cfg = DriveConfig {
                id: "x".into(), label: "lbl".into(), kind: kind.clone(), path,
                username: None, password: None,
            };
            let drive = DriveRegistry::instantiate(&cfg);
            assert_eq!(drive.drive_type(), expected,
                "DriveType::{:?} should instantiate to a drive that reports drive_type() == {:?}",
                kind, expected);
            assert_eq!(drive.label(), "lbl");
        }
    }
}
