//! FUSE filesystem implementation backed by a `CloudDrive`.
//!
//! Read-only: all write operations return `EROFS`.
//! Inode 1 is the root directory.  Inodes are assigned dynamically
//! on `readdir` and cached in a bidirectional `path ↔ ino` map.

use crate::drives::{CloudDrive, DirEntry, FileStat};
use crate::sync::transfer_queue::TransferQueue;
use anyhow::Result;
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// TTL for cached attributes (5 seconds).
const ATTR_TTL: Duration = Duration::from_secs(5);
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

struct ContentCache {
    max_bytes: u64,
    bytes: u64,
    entries: HashMap<PathBuf, Vec<u8>>,
    lru: VecDeque<PathBuf>,
}

impl ContentCache {
    fn new(max_bytes: u64) -> Self {
        Self { max_bytes, bytes: 0, entries: HashMap::new(), lru: VecDeque::new() }
    }

    fn get(&mut self, path: &Path) -> Option<Vec<u8>> {
        let data = self.entries.get(path)?.clone();
        self.lru.retain(|item| item != path);
        self.lru.push_back(path.to_owned());
        Some(data)
    }

    fn insert(&mut self, path: PathBuf, data: Vec<u8>) {
        let size = data.len() as u64;
        if size > self.max_bytes { return; }
        if let Some(previous) = self.entries.remove(&path) {
            self.bytes = self.bytes.saturating_sub(previous.len() as u64);
            self.lru.retain(|item| item != &path);
        }
        while self.bytes + size > self.max_bytes {
            let Some(oldest) = self.lru.pop_front() else { break; };
            if let Some(previous) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(previous.len() as u64);
            }
        }
        self.bytes += size;
        self.entries.insert(path.clone(), data);
        self.lru.push_back(path);
    }

    #[cfg(test)]
    fn stats(&self) -> (u64, usize) { (self.bytes, self.entries.len()) }
}

/// FUSE filesystem backed by a `CloudDrive`.
pub struct FuseDriveFs {
    drive: Arc<dyn CloudDrive>,
    queue: TransferQueue,
    /// Bidirectional inode ↔ path map, protected by a mutex.
    state: Mutex<InodeState>,
    content_cache: Mutex<ContentCache>,
}

struct InodeState {
    /// inode → (path, is_dir)
    ino_to_path: HashMap<u64, (PathBuf, bool)>,
    /// path → inode
    path_to_ino: HashMap<PathBuf, u64>,
    /// Next available inode number.
    next_ino: u64,
}

impl InodeState {
    fn new() -> Self {
        let mut ino_to_path = HashMap::new();
        let mut path_to_ino = HashMap::new();
        // Root inode = 1, maps to empty path.
        ino_to_path.insert(1, (PathBuf::new(), true));
        path_to_ino.insert(PathBuf::new(), 1);
        Self {
            ino_to_path,
            path_to_ino,
            next_ino: 2,
        }
    }

    /// Get or assign an inode for a path.
    fn get_or_assign(&mut self, path: &Path, is_dir: bool) -> u64 {
        if let Some(&ino) = self.path_to_ino.get(path) {
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.ino_to_path.insert(ino, (path.to_owned(), is_dir));
        self.path_to_ino.insert(path.to_owned(), ino);
        ino
    }

    fn path_for_ino(&self, ino: u64) -> Option<(PathBuf, bool)> {
        self.ino_to_path.get(&ino).cloned()
    }
}

impl FuseDriveFs {
    /// Create a new FUSE filesystem for the given drive.
    pub fn new(drive: Arc<dyn CloudDrive>) -> Self {
        Self::with_queue(drive, TransferQueue::shared())
    }

    /// Create a filesystem with an explicit queue, allowing the application
    /// to share its queue and tests to use a deterministic concurrency limit.
    pub fn with_queue(drive: Arc<dyn CloudDrive>, queue: TransferQueue) -> Self {
        Self::with_queue_and_cache(drive, queue, DEFAULT_CACHE_MAX_BYTES)
    }

    pub fn with_cache_limit(
        drive: Arc<dyn CloudDrive>,
        cache_max_bytes: u64,
    ) -> Self {
        Self {
            drive,
            queue: TransferQueue::shared(),
            state: Mutex::new(InodeState::new()),
            content_cache: Mutex::new(ContentCache::new(cache_max_bytes)),
        }
    }

    fn with_queue_and_cache(
        drive: Arc<dyn CloudDrive>,
        queue: TransferQueue,
        cache_max_bytes: u64,
    ) -> Self {
        Self {
            drive,
            queue,
            state: Mutex::new(InodeState::new()),
            content_cache: Mutex::new(ContentCache::new(cache_max_bytes)),
        }
    }

    /// Build a `FileAttr` from a `FileStat` and inode.
    fn make_attr(ino: u64, stat: &FileStat) -> FileAttr {
        let kind = if stat.is_dir {
            FileType::Directory
        } else {
            FileType::RegularFile
        };
        let mtime = stat
            .mtime_unix
            .map(|t| UNIX_EPOCH + Duration::from_secs(t.max(0) as u64))
            .unwrap_or(UNIX_EPOCH);
        let perm = if stat.is_dir { 0o755 } else { 0o444 }; // read-only

        FileAttr {
            ino,
            size: stat.size,
            blocks: (stat.size + 511) / 512,
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind,
            perm,
            nlink: if stat.is_dir { 2 } else { 1 },
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn dir_attr(ino: u64) -> FileAttr {
        Self::make_attr(
            ino,
            &FileStat {
                size: 0,
                is_dir: true,
                mtime_unix: None,
            },
        )
    }
}

impl Filesystem for FuseDriveFs {
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let (path, is_dir) = {
            let state = self.state.lock().unwrap();
            match state.path_for_ino(ino) {
                Some(p) => p,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        };

        match self.drive.stat(&path) {
            Ok(stat) => reply.attr(&ATTR_TTL, &Self::make_attr(ino, &stat)),
            Err(_) => {
                // If stat fails but we know it's a dir (from readdir),
                // return a synthetic dir attr.
                if is_dir {
                    reply.attr(&ATTR_TTL, &Self::dir_attr(ino));
                } else {
                    reply.error(libc::EIO);
                }
            }
        }
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent_path = {
            let state = self.state.lock().unwrap();
            match state.path_for_ino(parent) {
                Some((p, _)) => p,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        };

        let child_path = parent_path.join(name);
        match self.drive.stat(&child_path) {
            Ok(stat) => {
                let ino = {
                    let mut state = self.state.lock().unwrap();
                    state.get_or_assign(&child_path, stat.is_dir)
                };
                reply.entry(&ATTR_TTL, &Self::make_attr(ino, &stat), 0);
            }
            Err(_) => reply.error(libc::ENOENT),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let dir_path = {
            let state = self.state.lock().unwrap();
            match state.path_for_ino(ino) {
                Some((p, true)) => p,
                _ => {
                    reply.error(libc::ENOTDIR);
                    return;
                }
            }
        };

        let entries = match self.drive.list_dir(&dir_path) {
            Ok(e) => e,
            Err(_) => {
                reply.error(libc::EIO);
                return;
            }
        };

        // Build the full entry list: ".", "..", then directory contents.
        let mut all_entries: Vec<(u64, FileType, String)> = Vec::with_capacity(entries.len() + 2);
        all_entries.push((ino, FileType::Directory, ".".into()));
        all_entries.push((ino, FileType::Directory, "..".into()));

        {
            let mut state = self.state.lock().unwrap();
            for ent in &entries {
                let child_path = dir_path.join(&ent.name);
                let child_ino = state.get_or_assign(&child_path, ent.is_dir);
                let kind = if ent.is_dir {
                    FileType::Directory
                } else {
                    FileType::RegularFile
                };
                all_entries.push((child_ino, kind, ent.name.clone()));
            }
        }

        // Skip entries before the offset and add the rest.
        for (i, (child_ino, kind, name)) in all_entries.iter().enumerate().skip(offset as usize) {
            // reply.add returns true when the buffer is full.
            if reply.add(*child_ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let file_path = {
            let state = self.state.lock().unwrap();
            match state.path_for_ino(ino) {
                Some((p, false)) => p,
                _ => {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        };

        // Read the entire file (CloudDrive only supports full reads), but
        // retain bounded results so repeated FUSE page reads do not redownload.
        let data = if let Some(cached) = self.content_cache.lock().unwrap().get(&file_path) {
            Some(cached)
        } else {
            let drive = Arc::clone(&self.drive);
            let drive_id = drive.label().to_owned();
            let fetched = self.queue.download_blocking(
                drive_id,
                file_path.clone(),
                None,
                move |path| drive.read_file(path),
            ).ok();
            if let Some(ref bytes) = fetched {
                self.content_cache.lock().unwrap().insert(file_path.clone(), bytes.clone());
            }
            fetched
        };
        match data {
            Some(data) => {
                let start = (offset as usize).min(data.len());
                let end = (start + size as usize).min(data.len());
                reply.data(&data[start..end]);
            }
            None => reply.error(libc::EIO),
        }
    }

    // Write operations → EROFS (read-only filesystem).

    fn write(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        reply.error(libc::EROFS);
    }

    fn mkdir(
        &mut self,
        _req: &Request,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(libc::EROFS);
    }

    fn unlink(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }

    fn rmdir(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: fuser::ReplyEmpty) {
        reply.error(libc::EROFS);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::ContentCache;
    use std::path::Path;

    #[test]
    fn cache_evicts_least_recently_used_entries_by_bytes() {
        let mut cache = ContentCache::new(5);
        cache.insert("a".into(), b"123".to_vec());
        cache.insert("b".into(), b"45".to_vec());
        assert_eq!(cache.stats(), (5, 2));
        assert_eq!(cache.get(Path::new("a")).unwrap(), b"123");
        cache.insert("c".into(), b"xy".to_vec());
        assert!(cache.get(Path::new("b")).is_none());
        assert_eq!(cache.stats(), (5, 2));
    }

    #[test]
    fn oversized_entries_are_not_cached() {
        let mut cache = ContentCache::new(2);
        cache.insert("large".into(), b"123".to_vec());
        assert!(cache.get(Path::new("large")).is_none());
        assert_eq!(cache.stats(), (0, 0));
    }
}

// ── Mount / unmount helpers ──────────────────────────────────────────────

/// Mount a cloud drive as a read-only FUSE filesystem.
///
/// This function blocks (runs the FUSE event loop) — call it from a
/// dedicated thread.  Returns when the filesystem is unmounted.
pub fn mount_blocking(drive: Arc<dyn CloudDrive>, mount_point: &Path) -> Result<()> {
    mount_blocking_with_cache(drive, mount_point, DEFAULT_CACHE_MAX_BYTES)
}

/// Mount with an explicit maximum in-memory content-cache size.
pub fn mount_blocking_with_cache(
    drive: Arc<dyn CloudDrive>,
    mount_point: &Path,
    cache_max_bytes: u64,
) -> Result<()> {
    std::fs::create_dir_all(mount_point)?;
    let fs = FuseDriveFs::with_cache_limit(drive, cache_max_bytes);
    let options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::FSName("crispsorter".to_string()),
        fuser::MountOption::AutoUnmount,
    ];
    fuser::mount2(fs, mount_point, &options)?;
    Ok(())
}

// Note: tests for the FUSE filesystem require a real FUSE mount which
// needs root/fuse-group privileges.  Unit tests for the inode mapping
// and config are in the parent mod.rs (unconditional).  Integration
// tests are in src-tauri/tests/ and tagged #[ignore].
