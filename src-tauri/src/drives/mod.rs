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

pub mod tauri_commands;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    pub id:    String,
    pub label: String,
    pub kind:  DriveType,
    /// For `Local` drives: the root path.
    /// For CLI-based drives: the mount-point path (if OS-mounted) or the
    /// base path used by the CLI tool.
    pub path:  String,
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
            DriveType::Local | DriveType::Sftp | DriveType::Filen | DriveType::Internxt => {
                // All types use LocalDrive for now — CLI-based impls come later.
                Box::new(LocalDrive::new(config.label.clone(), PathBuf::from(&config.path)))
            }
        }
    }
}
