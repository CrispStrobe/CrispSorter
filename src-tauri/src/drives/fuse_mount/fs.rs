//! FUSE filesystem implementation backed by a `CloudDrive`.
//!
//! Read-only: all write operations return `EROFS`.
//! Inode 1 is the root directory.  Inodes are assigned dynamically
//! on `readdir` and cached in a bidirectional `path ↔ ino` map.

use crate::drives::{CloudDrive, FileStat};
use crate::drives::read_cache::{CachedFile, ContentCache};
use crate::sync::transfer_queue::TransferQueue;
use anyhow::Result;
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

/// TTL for cached attributes (5 seconds).
const ATTR_TTL: Duration = Duration::from_secs(5);
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

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

    /// Read one FUSE request through the real queue and cache without coupling
    /// storage I/O to a kernel reply. This is also testable without mounting.
    fn read_file_slice(&self, file_path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        if size == 0 {
            return Ok(Vec::new());
        }

        // Mounted local/SMB/NFS files support true seek+range reads; a FUSE
        // page must not allocate or copy the entire mounted file first.
        if self.drive.drive_type() == crate::drives::DriveType::Local {
            let drive = Arc::clone(&self.drive);
            let end = offset.saturating_add(size as u64 - 1);
            return self.queue.download_blocking(
                drive.label().to_owned(), file_path.to_owned(), None,
                move |path| drive.read_range(path, offset, end),
            );
        }

        // Other providers materialize once into a private disk-backed LRU.
        // Each read allocates only the requested slice, including cache hits.
        let cached = self.content_cache.lock().unwrap().get(file_path);
        let file = match cached {
            Some(cached) => cached,
            None => {
                let drive = Arc::clone(&self.drive);
                let drive_id = drive.label().to_owned();
                let size_hint = drive.stat(file_path).ok().map(|stat| stat.size);
                let fetched = Arc::new(CachedFile::create(|destination| {
                    self.queue.download_to_file_blocking(
                        drive_id, file_path.to_owned(), destination.to_owned(), size_hint,
                        move |path, writer| drive.read_file_to_writer(path, writer),
                    )
                })?);
                self.content_cache.lock().unwrap().insert(file_path.to_owned(), Arc::clone(&fetched));
                fetched
            }
        };
        file.read(offset, size)
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
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        if size == 0 {
            reply.data(&[]);
            return;
        }
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

        match self.read_file_slice(&file_path, offset as u64, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(_) => reply.error(libc::EIO),
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

// ── Mount / unmount helpers ──────────────────────────────────────────────

/// Mount a cloud drive as a read-only FUSE filesystem.
///
/// This function blocks (runs the FUSE event loop) — call it from a
/// dedicated thread.  Returns when the filesystem is unmounted.
pub fn mount_blocking(drive: Arc<dyn CloudDrive>, mount_point: &Path) -> Result<()> {
    mount_blocking_with_cache(drive, mount_point, DEFAULT_CACHE_MAX_BYTES)
}

/// Mount with an explicit maximum on-disk content-cache size.
pub fn mount_blocking_with_cache(
    drive: Arc<dyn CloudDrive>,
    mount_point: &Path,
    cache_max_bytes: u64,
) -> Result<()> {
    let log = crate::operation_log::OperationLog::start(
        "storage.fuse_mount", serde_json::json!({"surface":"native"}),
    );
    let result = (|| -> Result<()> {
        log.step("prepare_directory", serde_json::Value::Null);
        std::fs::create_dir_all(mount_point)?;
        let fs = FuseDriveFs::with_cache_limit(drive, cache_max_bytes);
        let options = vec![
            fuser::MountOption::RO,
            fuser::MountOption::FSName("crispsorter".to_string()),
            fuser::MountOption::AutoUnmount,
        ];
        log.step("mounting", serde_json::Value::Null);
        fuser::mount2(fs, mount_point, &options)?;
        Ok(())
    })();
    match &result {
        // mount2 blocks for the entire mounted session. Starting its thread
        // cannot acknowledge a completed or successfully mounted session.
        Ok(()) => log.succeed(serde_json::Value::Null),
        Err(_) => log.fail("fuse_mount_failed"),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drives::{DirEntry, DriveType, LocalDrive};
    use crate::sync::transfer_queue::TransferState;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Exercise the actual mounted reader while rejecting whole-buffer reads.
    /// WebDAV mode uses the same source to test the cloud materialization path.
    struct CheckedDrive {
        local: LocalDrive,
        kind: DriveType,
        downloads: AtomicUsize,
        fail_download: AtomicBool,
        ranges: Mutex<Vec<(u64, u64)>>,
    }

    impl CheckedDrive {
        fn new(root: &Path, kind: DriveType) -> Self {
            Self {
                local: LocalDrive::new("fuse-test", root),
                kind,
                downloads: AtomicUsize::new(0),
                fail_download: AtomicBool::new(false),
                ranges: Mutex::new(Vec::new()),
            }
        }
    }

    impl CloudDrive for CheckedDrive {
        fn label(&self) -> &str { self.local.label() }
        fn drive_type(&self) -> DriveType { self.kind.clone() }
        fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> { self.local.list_dir(path) }
        fn stat(&self, path: &Path) -> Result<FileStat> { self.local.stat(path) }
        fn read_file(&self, _path: &Path) -> Result<Vec<u8>> {
            panic!("FUSE must not use whole-buffer reads");
        }
        fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>> {
            assert_eq!(self.kind, DriveType::Local);
            self.ranges.lock().unwrap().push((start, end));
            self.local.read_range(path, start, end)
        }
        fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
            assert_ne!(self.kind, DriveType::Local, "local reads must use ranges");
            self.downloads.fetch_add(1, Ordering::SeqCst);
            if self.fail_download.swap(false, Ordering::SeqCst) {
                writer.write_all(b"partial")?;
                anyhow::bail!("scripted incomplete file");
            }
            self.local.read_file_to_writer(path, writer)
        }
        fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<()> {
            anyhow::bail!("read-only test drive")
        }
        fn delete(&self, _path: &Path) -> Result<()> {
            anyhow::bail!("read-only test drive")
        }
    }

    #[test]
    fn invalid_mount_directory_emits_failure_without_a_kernel_mount() {
        let directory = tempfile::tempdir().unwrap();
        let mount_point = directory.path().join("private-mount-target");
        std::fs::write(&mount_point, b"existing file").unwrap();
        let drive = Arc::new(LocalDrive::new("private-drive-label", directory.path()));
        let (result, events) = crate::operation_log::capture(|| {
            mount_blocking_with_cache(drive, &mount_point, 64)
        });

        assert!(result.is_err());
        assert_eq!(events.iter().map(|event| event.phase.as_str()).collect::<Vec<_>>(),
            ["started", "stage", "failed"]);
        assert!(events.iter().all(|event| event.operation == "storage.fuse_mount"
            && event.operation_id == events[0].operation_id));
        assert_eq!(events[1].stage.as_deref(), Some("prepare_directory"));
        assert_eq!(events[2].code.as_deref(), Some("fuse_mount_failed"));
        assert_eq!(std::fs::read(&mount_point).unwrap(), b"existing file");
        let logged = serde_json::to_string(&events).unwrap();
        assert!(!logged.contains("private-mount-target"));
        assert!(!logged.contains("private-drive-label"));
        assert!(!logged.contains(directory.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn local_fuse_reads_use_exact_ranges_without_content_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = Path::new("document");
        std::fs::write(directory.path().join(path), b"abcdefghij").unwrap();
        let drive = Arc::new(CheckedDrive::new(directory.path(), DriveType::Local));
        let queue = TransferQueue::with_concurrency(1);
        let fs = FuseDriveFs::with_queue_and_cache(drive.clone(), queue.clone(), 64);

        assert_eq!(fs.read_file_slice(path, 2, 3).unwrap(), b"cde");
        std::fs::write(directory.path().join(path), b"0123456789").unwrap();
        assert_eq!(fs.read_file_slice(path, 8, 4).unwrap(), b"89");
        assert!(fs.read_file_slice(path, 10, 2).unwrap().is_empty());
        assert!(fs.read_file_slice(path, 0, 0).unwrap().is_empty());
        assert_eq!(*drive.ranges.lock().unwrap(), vec![(2, 4), (8, 11), (10, 11)]);
        assert_eq!(drive.downloads.load(Ordering::SeqCst), 0);
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (0, 0));
        assert_eq!(queue.snapshot().len(), 3);
        assert!(queue.snapshot().iter().all(|job| matches!(&job.state, TransferState::Done)));
    }

    #[test]
    fn cloud_fuse_reads_stream_once_and_reuse_only_the_requested_slice() {
        let directory = tempfile::tempdir().unwrap();
        let path = Path::new("document");
        std::fs::write(directory.path().join(path), b"abcdefghij").unwrap();
        let drive = Arc::new(CheckedDrive::new(directory.path(), DriveType::WebDav));
        let queue = TransferQueue::with_concurrency(1);
        let fs = FuseDriveFs::with_queue_and_cache(drive.clone(), queue.clone(), 64);

        assert_eq!(fs.read_file_slice(path, 2, 3).unwrap(), b"cde");
        // A cache hit must not contact the provider again.
        std::fs::remove_file(directory.path().join(path)).unwrap();
        assert_eq!(fs.read_file_slice(path, 8, 4).unwrap(), b"ij");
        assert!(fs.read_file_slice(path, 10, 2).unwrap().is_empty());
        assert_eq!(drive.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (10, 1));
        let jobs = queue.snapshot();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].bytes_done, 10);
        assert!(matches!(&jobs[0].state, TransferState::Done));
    }

    #[test]
    fn failed_fuse_download_never_caches_partial_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = Path::new("document");
        std::fs::write(directory.path().join(path), b"abcdefghij").unwrap();
        let drive = Arc::new(CheckedDrive::new(directory.path(), DriveType::WebDav));
        drive.fail_download.store(true, Ordering::SeqCst);
        let queue = TransferQueue::with_concurrency(1);
        let fs = FuseDriveFs::with_queue_and_cache(drive.clone(), queue.clone(), 64);

        assert!(fs.read_file_slice(path, 0, 10).is_err());
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (0, 0));
        assert_eq!(fs.read_file_slice(path, 0, 10).unwrap(), b"abcdefghij");
        assert_eq!(drive.downloads.load(Ordering::SeqCst), 2);
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (10, 1));
        let jobs = queue.snapshot();
        assert_eq!(jobs.len(), 2);
        assert!(matches!(&jobs[0].state, TransferState::Failed { .. }));
        assert!(matches!(&jobs[1].state, TransferState::Done));
    }
}
