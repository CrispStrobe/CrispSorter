//! Parallel directory scanner that produces a `FileIndex`.
//!
//! `jwalk` (rayon-backed) is used so a full hard-drive walk uses every
//! core for the metadata-stat phase. Hashing, when requested, runs as
//! a second rayon pass so the I/O-bound walk doesn't get blocked on
//! CPU-bound digests.

use std::path::Path;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::index::{FileEntry, FileIndex};

/// Hash algorithms supported when scanning. Mirrors Catfish's `--hash`
/// CLI options so a CrispSorter-produced catalog interchanges 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgo {
    pub fn name(self) -> &'static str {
        match self {
            HashAlgo::Md5 => "md5",
            HashAlgo::Sha1 => "sha1",
            HashAlgo::Sha256 => "sha256",
        }
    }
}

/// Tunables for a single scan run. Defaults match Catfish's behaviour.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// When `Some`, hash every file with the given algorithm; when
    /// `None`, leave `FileEntry::hash` empty (Cathy-classic behavior —
    /// hashes are computed lazily during dedup).
    pub hash: Option<HashAlgo>,
    /// When true, follow symlinks. Off by default — symlinks tend to
    /// inflate catalogs and risk infinite loops.
    pub follow_symlinks: bool,
    /// Skip files larger than this (bytes); useful when only hashing
    /// small files. None = no limit.
    pub max_size_bytes: Option<u64>,
}

/// Walk `root` recursively, building a `FileIndex` with optional
/// hashes. The walk uses jwalk's parallel mode (one thread pool per
/// process; size scales to logical core count automatically).
pub fn scan_dir(root: &Path, options: ScanOptions) -> std::io::Result<FileIndex> {
    let is_windows_path = cfg!(windows);
    let mut index = FileIndex::new(root.to_path_buf(), is_windows_path);

    // Phase 1 — parallel walk + stat. jwalk fans out across rayon's
    // pool. We collect the metadata-only entries first so the walk
    // doesn't sit on a CPU-bound hash spinner.
    let walker = jwalk::WalkDir::new(root)
        .follow_links(options.follow_symlinks)
        .skip_hidden(false);

    let raw: Vec<FileEntry> = walker
        .into_iter()
        .filter_map(|res| res.ok())
        .filter(|de| de.file_type().is_file())
        .filter_map(|de| {
            let path = de.path();
            let meta = de.metadata().ok()?;
            let size = meta.len();
            if let Some(max) = options.max_size_bytes {
                if size > max {
                    return None;
                }
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as u32)
                .unwrap_or(0);
            Some(FileEntry::new(path, size, mtime))
        })
        .collect();

    // Phase 2 — optional parallel hashing. We use rayon directly here
    // (not jwalk) because hashing is CPU-bound and benefits from the
    // pure compute scheduling.
    let hashed: Vec<FileEntry> = if let Some(algo) = options.hash {
        raw.into_par_iter()
            .map(|mut entry| {
                if let Ok(h) = hash_file(&entry.path, algo) {
                    entry.hash = Some(h);
                }
                entry
            })
            .collect()
    } else {
        raw
    };

    for entry in hashed {
        index.add(entry);
    }
    Ok(index)
}

/// Hash a single file's contents and return the hex digest. ~1 MB
/// streaming buffer; the hashers are reset per call so this is safe
/// to call from rayon workers in parallel.
pub fn hash_file(path: &Path, algo: HashAlgo) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    match algo {
        HashAlgo::Md5 => {
            use md5::{Digest, Md5};
            let mut h = Md5::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(hex::encode(h.finalize()))
        }
        HashAlgo::Sha1 => {
            use sha1::{Digest, Sha1};
            let mut h = Sha1::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(hex::encode(h.finalize()))
        }
        HashAlgo::Sha256 => {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(hex::encode(h.finalize()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scans_files_without_hashing() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), b"hello").unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub/b.bin"), &[0u8; 4096]).unwrap();
        let idx = scan_dir(tmp.path(), ScanOptions::default()).unwrap();
        assert_eq!(idx.len(), 2);
        // Hashes should be empty when no algo requested.
        assert!(idx.all_files.iter().all(|e| e.hash.is_none()));
    }

    #[test]
    fn scans_files_with_md5() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), b"hello").unwrap();
        let idx = scan_dir(
            tmp.path(),
            ScanOptions {
                hash: Some(HashAlgo::Md5),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(idx.len(), 1);
        // Known: md5("hello") == 5d41402abc4b2a76b9719d911017c592
        assert_eq!(
            idx.all_files[0].hash.as_deref(),
            Some("5d41402abc4b2a76b9719d911017c592")
        );
    }

    #[test]
    fn max_size_filter_excludes_large_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("small.txt"), b"hi").unwrap();
        fs::write(tmp.path().join("big.bin"), &[0u8; 8192]).unwrap();
        let idx = scan_dir(
            tmp.path(),
            ScanOptions {
                max_size_bytes: Some(100),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(
            idx.all_files[0].path.file_name().unwrap().to_string_lossy(),
            "small.txt"
        );
    }
}
