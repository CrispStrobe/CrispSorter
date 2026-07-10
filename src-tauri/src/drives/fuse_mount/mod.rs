//! P29.9 — FUSE mounting for cloud drive indexing.
//!
//! Read-only FUSE filesystem that delegates to a `CloudDrive` trait
//! object.  Enables indexing cloud-stored documents via the existing
//! folder watcher without downloading entire libraries locally.
//!
//! # Architecture
//!
//! `FuseDriveFs` implements `fuser::Filesystem` by mapping FUSE
//! operations (readdir, getattr, read) to `CloudDrive` methods.
//! Write operations are stubbed (return EROFS) — this is an indexing
//! mount, not a general-purpose file system.
//!
//! Inode management: inode 1 = root.  Other inodes are assigned on
//! first discovery via `readdir` and cached in a path ↔ inode map.
//!
//! File content is cached in a local LRU directory to avoid
//! re-downloading on every read.
//!
//! # Platform support
//!
//! - Linux: needs `libfuse3-dev` + user in `fuse` group.
//! - macOS: needs macFUSE or FUSE-T.
//! - Windows: not supported (WinFSP/Dokany is a separate effort).
//!
//! Gated behind `--features fuse`.

#[cfg(feature = "fuse")]
pub mod fs;

// Re-export core types unconditionally so callers can reference them
// without feature-gating every use site.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for a FUSE mount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuseMountConfig {
    /// ID of the registered cloud drive to mount.
    pub drive_id: String,
    /// Local path where the drive will be mounted.
    pub mount_point: PathBuf,
    /// Maximum cache size in bytes (default 2 GB).
    #[serde(default = "default_cache_max")]
    pub cache_max_bytes: u64,
}

fn default_cache_max() -> u64 {
    2 * 1024 * 1024 * 1024 // 2 GB
}

/// Status of a FUSE mount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuseMountStatus {
    pub drive_id: String,
    pub mount_point: PathBuf,
    pub active: bool,
    pub cached_bytes: u64,
    pub cached_files: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_config_serde_round_trips() {
        let cfg = FuseMountConfig {
            drive_id: "gdrive-1".into(),
            mount_point: PathBuf::from("/mnt/cloud"),
            cache_max_bytes: 1_000_000_000,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: FuseMountConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.drive_id, "gdrive-1");
        assert_eq!(back.mount_point, PathBuf::from("/mnt/cloud"));
        assert_eq!(back.cache_max_bytes, 1_000_000_000);
    }

    #[test]
    fn default_cache_is_2gb() {
        let json = r#"{"drive_id":"x","mount_point":"/mnt/x"}"#;
        let cfg: FuseMountConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.cache_max_bytes, 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn mount_status_serde() {
        let s = FuseMountStatus {
            drive_id: "d1".into(),
            mount_point: "/mnt/d1".into(),
            active: true,
            cached_bytes: 500_000,
            cached_files: 42,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"active\":true"));
    }
}
