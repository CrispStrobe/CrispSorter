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

/// Below this size a chunk-addressed provider's file is still downloaded once
/// into the disk cache: one transfer beats many small ranged requests.
const CHUNKED_RANGE_MIN_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Bytes fetched per ranged miss. A whole number of Filen's 1 MiB encrypted
/// chunks, so sequential FUSE pages are served from memory instead of
/// re-downloading and re-authenticating the same chunk for every page.
const CHUNKED_RANGE_WINDOW_BYTES: u64 = 4 * 1024 * 1024;

/// Providers whose `read_range` fetches whole encrypted chunks: worth a
/// ranged read for large files, but only through an aligned window.
fn chunked_range_provider(kind: &crate::drives::DriveType) -> bool {
    matches!(kind, crate::drives::DriveType::Filen)
}

/// The inclusive, `window`-aligned range covering `offset..offset + size`,
/// clamped to `file_size`; `None` for an empty request or one at/after EOF.
fn aligned_window(offset: u64, size: u32, file_size: u64, window: u64) -> Option<(u64, u64)> {
    if size == 0 || offset >= file_size || window == 0 {
        return None;
    }
    let start = offset / window * window;
    let wanted_end = offset.saturating_add(size as u64).min(file_size);
    let end = wanted_end
        .div_ceil(window)
        .saturating_mul(window)
        .min(file_size);
    Some((start, end - 1))
}

/// One fetched window of a remote file, kept in memory for the next pages.
struct RangeWindow {
    path: PathBuf,
    file_size: u64,
    start: u64,
    bytes: Vec<u8>,
}

impl RangeWindow {
    /// The requested slice when this window fully covers it (or the request
    /// is at/after EOF of this file); `None` on a miss.
    fn slice(&self, path: &Path, offset: u64, size: u32) -> Option<Vec<u8>> {
        if self.path != path {
            return None;
        }
        if offset >= self.file_size {
            return Some(Vec::new());
        }
        let window_end = self.start + self.bytes.len() as u64;
        let wanted_end = offset.saturating_add(size as u64).min(self.file_size);
        if offset < self.start || wanted_end > window_end {
            return None;
        }
        Some(self.bytes[(offset - self.start) as usize..(wanted_end - self.start) as usize].to_vec())
    }
}

/// FUSE filesystem backed by a `CloudDrive`.
pub struct FuseDriveFs {
    drive: Arc<dyn CloudDrive>,
    queue: TransferQueue,
    /// Bidirectional inode ↔ path map, protected by a mutex.
    state: Mutex<InodeState>,
    content_cache: Mutex<ContentCache>,
    /// Last range window fetched from a chunk-addressed provider (Filen).
    range_window: Mutex<Option<RangeWindow>>,
    /// Files at least this large are range-read from chunk-addressed
    /// providers instead of being materialised whole into `content_cache`.
    chunked_range_min_bytes: u64,
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
            range_window: Mutex::new(None),
            chunked_range_min_bytes: CHUNKED_RANGE_MIN_FILE_BYTES,
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
            range_window: Mutex::new(None),
            chunked_range_min_bytes: CHUNKED_RANGE_MIN_FILE_BYTES,
        }
    }

    /// Fetch the aligned window around a request through the transfer queue,
    /// keep it for the following pages, and return the requested slice.
    fn read_chunked_window(
        &self,
        file_path: &Path,
        offset: u64,
        size: u32,
        file_size: u64,
    ) -> Result<Vec<u8>> {
        let Some((start, end)) = aligned_window(offset, size, file_size, CHUNKED_RANGE_WINDOW_BYTES) else {
            return Ok(Vec::new());
        };
        let drive = Arc::clone(&self.drive);
        let bytes = self.queue.download_blocking(
            drive.label().to_owned(), file_path.to_owned(), Some(end - start + 1),
            move |path| drive.read_range(path, start, end),
        )?;
        let window = RangeWindow { path: file_path.to_owned(), file_size, start, bytes };
        // A short window means the remote file changed size since the stat.
        let slice = window.slice(file_path, offset, size)
            .ok_or_else(|| anyhow::anyhow!("provider returned a short byte range"))?;
        *self.range_window.lock().unwrap() = Some(window);
        Ok(slice)
    }

    /// Read one FUSE request through the real queue and cache without coupling
    /// storage I/O to a kernel reply. This is also testable without mounting.
    fn read_file_slice(&self, file_path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
        if size == 0 {
            return Ok(Vec::new());
        }

        // Mounted local/SMB/NFS files support true seek+range reads; a FUSE
        // page must not allocate or copy the entire mounted file first.
        if matches!(self.drive.drive_type(), crate::drives::DriveType::Local | crate::drives::DriveType::Smb) {
            let drive = Arc::clone(&self.drive);
            let end = offset.saturating_add(size as u64 - 1);
            return self.queue.download_blocking(
                drive.label().to_owned(), file_path.to_owned(), None,
                move |path| {
                    // SMB's explicit range API rejects ranges beyond EOF;
                    // FUSE routinely requests a full final page. Keep that
                    // API contract while returning the available bytes here.
                    let end = if drive.drive_type() == crate::drives::DriveType::Smb {
                        let length = drive.stat(path)?.size;
                        if offset >= length { return Ok(Vec::new()); }
                        end.min(length - 1)
                    } else { end };
                    drive.read_range(path, offset, end)
                },
            );
        }

        // Chunk-addressed providers (Filen) serve large files by aligned range
        // windows; a page inside the last window needs no provider call.
        let chunked = chunked_range_provider(&self.drive.drive_type());
        if chunked {
            let hit = self.range_window.lock().unwrap().as_ref()
                .and_then(|window| window.slice(file_path, offset, size));
            if let Some(bytes) = hit {
                return Ok(bytes);
            }
        }

        // Other providers materialize once into a private disk-backed LRU.
        // Each read allocates only the requested slice, including cache hits.
        let cached = self.content_cache.lock().unwrap().get(file_path);
        let file = match cached {
            Some(cached) => cached,
            None => {
                let drive = Arc::clone(&self.drive);
                let drive_id = drive.label().to_owned();
                // The window of this file already knows its size; reusing it
                // spares a provider stat for every window of a sequential read.
                let known_size = if chunked {
                    self.range_window.lock().unwrap().as_ref()
                        .filter(|window| window.path == file_path)
                        .map(|window| window.file_size)
                } else { None };
                let size_hint = known_size.or_else(|| drive.stat(file_path).ok().map(|stat| stat.size));
                if let (true, Some(file_size)) = (chunked, size_hint) {
                    if file_size >= self.chunked_range_min_bytes {
                        return self.read_chunked_window(file_path, offset, size, file_size);
                    }
                }
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
            assert!(matches!(self.kind, DriveType::Local | DriveType::Filen), "only ranged providers may use read_range");
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
    fn smb_fuse_ranges_clamp_the_last_page_and_eof_without_caching() {
        let directory = tempfile::tempdir().unwrap();
        let path = Path::new("document");
        std::fs::write(directory.path().join(path), b"abcdefghij").unwrap();
        let drive = Arc::new(crate::drives::smb::SmbDrive::new(
            "fuse-smb".into(), "smb://unused.invalid/share",
            crisp_smb_source::Settings {
                mount_path: Some(directory.path().to_string_lossy().into_owned()),
                ..Default::default()
            }, "", "",
        ).unwrap());
        let queue = TransferQueue::with_concurrency(1);
        let fs = FuseDriveFs::with_queue_and_cache(drive, queue.clone(), 64);
        assert_eq!(fs.read_file_slice(path, 8, 4096).unwrap(), b"ij");
        assert!(fs.read_file_slice(path, 10, 4096).unwrap().is_empty());
        assert!(fs.read_file_slice(path, 100, 4096).unwrap().is_empty());
        std::fs::write(directory.path().join(path), b"0123456789").unwrap();
        assert_eq!(fs.read_file_slice(path, 8, 4096).unwrap(), b"89");
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (0, 0));
        assert!(queue.snapshot().iter().all(|job| matches!(&job.state, TransferState::Done)));
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

    #[test]
    fn aligned_windows_cover_the_request_and_clamp_to_eof() {
        const W: u64 = 4;
        assert_eq!(aligned_window(5, 2, 100, W), Some((4, 7)));
        assert_eq!(aligned_window(3, 2, 100, W), Some((0, 7)));
        assert_eq!(aligned_window(8, 4, 100, W), Some((8, 11)));
        assert_eq!(aligned_window(97, 50, 99, W), Some((96, 98)));
        assert_eq!(aligned_window(99, 1, 99, W), None);
        assert_eq!(aligned_window(0, 0, 99, W), None);
        assert_eq!(aligned_window(u64::MAX - 1, u32::MAX, u64::MAX, W), Some((u64::MAX - 3, u64::MAX - 1)));
    }

    #[test]
    fn range_window_serves_only_fully_covered_pages() {
        let window = RangeWindow { path: PathBuf::from("a"), file_size: 10, start: 4, bytes: b"4567".to_vec() };
        assert_eq!(window.slice(Path::new("a"), 5, 2).unwrap(), b"56");
        assert_eq!(window.slice(Path::new("a"), 4, 4).unwrap(), b"4567");
        assert!(window.slice(Path::new("a"), 6, 4).is_none());
        assert!(window.slice(Path::new("a"), 3, 2).is_none());
        assert!(window.slice(Path::new("b"), 5, 1).is_none());
        assert!(window.slice(Path::new("a"), 10, 4).unwrap().is_empty());
        let tail = RangeWindow { path: PathBuf::from("a"), file_size: 10, start: 8, bytes: b"89".to_vec() };
        assert_eq!(tail.slice(Path::new("a"), 9, 4096).unwrap(), b"9");
    }

    #[test]
    fn filen_large_reads_use_aligned_windows_without_content_cache() {
        const MIB: u64 = 1024 * 1024;
        let directory = tempfile::tempdir().unwrap();
        let path = Path::new("large.bin");
        let length = 9 * MIB + 100;
        let content: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
        std::fs::write(directory.path().join(path), &content).unwrap();
        let drive = Arc::new(CheckedDrive::new(directory.path(), DriveType::Filen));
        let queue = TransferQueue::with_concurrency(1);
        let mut fs = FuseDriveFs::with_queue_and_cache(drive.clone(), queue.clone(), 64);
        fs.chunked_range_min_bytes = 0;
        let slice = |offset: u64, size: u64| content[offset as usize..(offset + size).min(length) as usize].to_vec();

        assert_eq!(fs.read_file_slice(path, 5 * MIB + 10, 4096).unwrap(), slice(5 * MIB + 10, 4096));
        // The next sequential page is inside the fetched window.
        assert_eq!(fs.read_file_slice(path, 5 * MIB + 4106, 4096).unwrap(), slice(5 * MIB + 4106, 4096));
        assert_eq!(fs.read_file_slice(path, 8 * MIB + 50, MIB as u32).unwrap(), slice(8 * MIB + 50, MIB));
        assert!(fs.read_file_slice(path, length, 4096).unwrap().is_empty());
        assert_eq!(*drive.ranges.lock().unwrap(), vec![(4 * MIB, 8 * MIB - 1), (8 * MIB, length - 1)]);
        assert_eq!(drive.downloads.load(Ordering::SeqCst), 0);
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (0, 0));
        assert!(queue.snapshot().iter().all(|job| matches!(&job.state, TransferState::Done)));
    }

    #[test]
    fn filen_small_files_still_materialise_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = Path::new("document");
        std::fs::write(directory.path().join(path), b"abcdefghij").unwrap();
        let drive = Arc::new(CheckedDrive::new(directory.path(), DriveType::Filen));
        let queue = TransferQueue::with_concurrency(1);
        let fs = FuseDriveFs::with_queue_and_cache(drive.clone(), queue, 64);

        assert_eq!(fs.read_file_slice(path, 2, 3).unwrap(), b"cde");
        assert_eq!(fs.read_file_slice(path, 8, 4).unwrap(), b"ij");
        assert!(drive.ranges.lock().unwrap().is_empty());
        assert_eq!(drive.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(fs.content_cache.lock().unwrap().stats(), (10, 1));
    }
}
