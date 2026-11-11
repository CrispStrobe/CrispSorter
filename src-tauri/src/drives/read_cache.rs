//! Disk-backed FUSE content cache. A read allocates only its requested slice.
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct CachedFile {
    _directory: tempfile::TempDir,
    path: PathBuf,
    bytes: u64,
}

impl CachedFile {
    pub(crate) fn create(download: impl FnOnce(&Path) -> Result<u64>) -> Result<Self> {
        let directory = tempfile::Builder::new().prefix("crispsorter-fuse-").tempdir()
            .context("create FUSE cache directory")?;
        let path = directory.path().join("content");
        let bytes = download(&path)?;
        anyhow::ensure!(std::fs::metadata(&path)?.len() == bytes, "FUSE cache transfer size mismatch");
        Ok(Self { _directory: directory, path, bytes })
    }

    pub(crate) fn read(&self, offset: u64, size: u32) -> Result<Vec<u8>> {
        let length = (size as u64).min(self.bytes.saturating_sub(offset));
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(length).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

pub(crate) struct ContentCache {
    max_bytes: u64,
    bytes: u64,
    entries: HashMap<PathBuf, Arc<CachedFile>>,
    lru: VecDeque<PathBuf>,
}

impl ContentCache {
    pub(crate) fn new(max_bytes: u64) -> Self {
        Self { max_bytes, bytes: 0, entries: HashMap::new(), lru: VecDeque::new() }
    }

    pub(crate) fn get(&mut self, path: &Path) -> Option<Arc<CachedFile>> {
        let file = Arc::clone(self.entries.get(path)?);
        self.lru.retain(|item| item != path);
        self.lru.push_back(path.to_owned());
        Some(file)
    }

    pub(crate) fn insert(&mut self, path: PathBuf, file: Arc<CachedFile>) {
        if file.bytes > self.max_bytes { return; }
        if let Some(previous) = self.entries.remove(&path) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            self.lru.retain(|item| item != &path);
        }
        while self.bytes.saturating_add(file.bytes) > self.max_bytes {
            let Some(oldest) = self.lru.pop_front() else { break; };
            if let Some(previous) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(previous.bytes);
            }
        }
        self.bytes += file.bytes;
        self.entries.insert(path.clone(), file);
        self.lru.push_back(path);
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> (u64, usize) { (self.bytes, self.entries.len()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(bytes: &[u8]) -> Arc<CachedFile> {
        Arc::new(CachedFile::create(|path| { std::fs::write(path, bytes)?; Ok(bytes.len() as u64) }).unwrap())
    }

    #[test]
    fn disk_cache_returns_only_requested_slice_and_empty_eof() {
        let file = cached(b"abcdef");
        assert_eq!(file.read(2, 2).unwrap(), b"cd");
        assert_eq!(file.read(4, 100).unwrap(), b"ef");
        assert!(file.read(6, 100).unwrap().is_empty());
        assert!(file.read(10, 100).unwrap().is_empty());
    }

    #[test]
    fn eviction_releases_disk_file_after_last_active_reader() {
        let mut cache = ContentCache::new(5);
        let first = cached(b"abc");
        let path = first.path.clone();
        cache.insert("first".into(), first);
        let reader = cache.get(Path::new("first")).unwrap();
        cache.insert("second".into(), cached(b"def"));
        assert!(cache.get(Path::new("first")).is_none());
        assert_eq!(cache.stats(), (3, 1));
        assert_eq!(reader.read(0, 3).unwrap(), b"abc");
        assert!(path.exists());
        drop(reader);
        assert!(!path.exists());
    }

    #[test]
    fn cache_evicts_least_recently_used_entries_by_bytes() {
        let mut cache = ContentCache::new(5);
        cache.insert("a".into(), cached(b"123"));
        cache.insert("b".into(), cached(b"45"));
        assert_eq!(cache.stats(), (5, 2));
        assert_eq!(cache.get(Path::new("a")).unwrap().read(0, 3).unwrap(), b"123");
        cache.insert("c".into(), cached(b"xy"));
        assert!(cache.get(Path::new("b")).is_none());
        assert_eq!(cache.stats(), (5, 2));
    }

    #[test]
    fn oversize_entry_is_not_retained_and_temporary_storage_is_cleaned() {
        let mut cache = ContentCache::new(2);
        let file = cached(b"abc");
        let path = file.path.clone();
        cache.insert("large".into(), file);
        assert_eq!(cache.stats(), (0, 0));
        assert!(!path.exists());
    }

    #[test]
    fn failed_download_cleans_its_private_cache_directory() {
        let path = std::sync::Mutex::new(PathBuf::new());
        assert!(CachedFile::create(|target| {
            *path.lock().unwrap() = target.to_owned();
            std::fs::write(target, b"partial")?;
            anyhow::bail!("download failed")
        }).is_err());
        assert!(!path.lock().unwrap().parent().unwrap().exists());
    }
}
