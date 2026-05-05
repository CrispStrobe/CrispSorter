//! In-memory file index with O(1) size-bucket duplicate lookups.
//!
//! Mirrors Catfish's `FileIndex` data layout so the .caf round-trip
//! (load from .caf → in-memory → save to .caf) is a straight
//! pass-through, but uses `Vec<usize>` indices into the canonical
//! `all_files` vector instead of Python's reference-everywhere model
//! so we don't pay clone costs on every bucket insert.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One file's metadata as stored in a catalog. Matches the on-disk
/// shape of a .caf entry (mtime is unix epoch seconds, fitting the
/// `<L>` slot), with an optional hash that's never round-tripped via
/// .caf — Cathy's format doesn't carry hashes, so they're recomputed
/// on demand for dedup.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u32,
    /// Hex-encoded hash digest; `None` when not yet computed.
    pub hash: Option<String>,
}

impl FileEntry {
    pub fn new(path: PathBuf, size: u64, mtime: u32) -> Self {
        Self { path, size, mtime, hash: None }
    }

    pub fn with_hash(mut self, hash: String) -> Self {
        self.hash = Some(hash);
        self
    }
}

/// Volume-level metadata captured from a `.caf` header (or supplied
/// when scanning fresh). Round-tripped through `read_file` /
/// `write_file` since v0.1.36 so re-saving a catalog doesn't drop the
/// drive label / serial / free-space-at-scan-time the user originally
/// captured.
///
/// Defaults are zero/empty — what `FileIndex::new` sets when scanning
/// a fresh folder without an existing `.caf` to inherit from.
// Eq isn't derivable because `f32 freesize` only implements PartialEq —
// fine for our use (header equality checks happen in tests, not in
// HashMap keys).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct VolumeHeader {
    /// User-facing volume name as printed by Cathy/Catfish (`Archive`,
    /// `Macintosh HD`). Empty when not set.
    pub label: String,
    /// Per-machine alias — Cathy/Catfish use this for shorter labels in
    /// listings. Often duplicates `label`.
    pub alias: String,
    /// `<L>` Volume serial number. 0 when unknown.
    pub serial: u32,
    /// Free user comment. Available v ≥ 4. Empty by default.
    pub comment: String,
    /// Free space at scan time (bytes-as-`f32`). Available v ≥ 1.
    pub freesize: f32,
    /// Archive flag. Available v ≥ 6. Cathy uses bit 0 = "this catalog
    /// represents an archive volume the user shouldn't expect to be
    /// constantly mounted." Pass-through value.
    pub archive: i16,
    /// Creation date as unix epoch seconds. Cathy stores it `<L>` so it
    /// wraps in 2106; close enough for a catalog timestamp. 0 when
    /// unknown.
    pub date: u32,
}

/// Catalog of files under a root path.
///
/// The buckets store indices into `all_files` rather than cloning
/// entries — this keeps memory bounded for million-entry catalogs and
/// matches what Catfish's `defaultdict(list)` is doing semantically.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileIndex {
    /// Root path of the cataloged drive/folder. Stored verbatim from
    /// the .caf header so an offline browse still shows the original
    /// device prefix even when the drive isn't mounted.
    pub root_path: PathBuf,

    /// True when the catalog was produced on Windows (path separator
    /// `\\` or drive letter prefix). Drives the path-component logic
    /// during reads so a Windows .caf opened on macOS still parses.
    pub is_windows_path: bool,

    /// All files in insertion order.
    pub all_files: Vec<FileEntry>,

    /// Volume-level metadata read from the `.caf` header. Carries
    /// label / alias / serial / comment / freesize / archive flag /
    /// creation date through a load → save round-trip — fresh scans
    /// start with `VolumeHeader::default()`.
    #[serde(default)]
    pub header: VolumeHeader,

    /// On-disk format version this catalog was loaded from (1..=8),
    /// or `8` for fresh `FileIndex::new` instances. The writer
    /// inspects this to decide whether to emit a v6 (legacy
    /// Cathy-compatible) or v8 (current Catfish) `.caf` — so a
    /// load-from-v6 → save round-trip preserves the format unless the
    /// caller explicitly upgrades. Out-of-range values in older
    /// serialised state get clamped to 8 by the writer.
    #[serde(default = "default_save_version")]
    pub save_version: u8,

    /// `size → indices into all_files`. O(1) lookup of dup candidates
    /// by size — same trick Catfish uses.
    #[serde(skip)]
    pub size_index: HashMap<u64, Vec<usize>>,

    /// `(size, hash) → indices into all_files`. Only populated for
    /// entries that have a hash computed.
    #[serde(skip)]
    pub hash_index: HashMap<(u64, String), Vec<usize>>,
}

fn default_save_version() -> u8 {
    8
}

impl FileIndex {
    pub fn new(root_path: PathBuf, is_windows_path: bool) -> Self {
        Self {
            root_path,
            is_windows_path,
            all_files: Vec::new(),
            header: VolumeHeader::default(),
            save_version: 8,
            size_index: HashMap::new(),
            hash_index: HashMap::new(),
        }
    }

    /// Insert an entry and update both bucket indexes.
    pub fn add(&mut self, entry: FileEntry) {
        let idx = self.all_files.len();
        let size = entry.size;
        let hash = entry.hash.clone();
        self.all_files.push(entry);
        self.size_index.entry(size).or_default().push(idx);
        if let Some(h) = hash {
            self.hash_index.entry((size, h)).or_default().push(idx);
        }
    }

    pub fn len(&self) -> usize {
        self.all_files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all_files.is_empty()
    }

    /// Total bytes across all files in the catalog.
    pub fn total_size(&self) -> u64 {
        self.all_files.iter().map(|e| e.size).sum()
    }

    /// Files with the same size as `size` — the cheap dup pre-filter.
    /// Returns an empty slice if no other file shares the size.
    pub fn by_size(&self, size: u64) -> impl Iterator<Item = &FileEntry> {
        self.size_index
            .get(&size)
            .into_iter()
            .flat_map(|idxs| idxs.iter().map(|&i| &self.all_files[i]))
    }

    /// Rebuild the bucket indexes from `all_files`. Useful after
    /// deserializing a `FileIndex` (the buckets are `#[serde(skip)]`).
    pub fn rebuild_indexes(&mut self) {
        self.size_index.clear();
        self.hash_index.clear();
        for (idx, entry) in self.all_files.iter().enumerate() {
            self.size_index.entry(entry.size).or_default().push(idx);
            if let Some(h) = &entry.hash {
                self.hash_index
                    .entry((entry.size, h.clone()))
                    .or_default()
                    .push(idx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_populates_buckets() {
        let mut idx = FileIndex::new(PathBuf::from("/tmp"), false);
        idx.add(FileEntry::new(PathBuf::from("/tmp/a"), 100, 1700000000));
        idx.add(FileEntry::new(PathBuf::from("/tmp/b"), 100, 1700000001));
        idx.add(FileEntry::new(PathBuf::from("/tmp/c"), 200, 1700000002));
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.size_index.get(&100).unwrap().len(), 2);
        assert_eq!(idx.size_index.get(&200).unwrap().len(), 1);
        assert_eq!(idx.total_size(), 400);
    }

    #[test]
    fn rebuild_indexes_recovers_from_serde_skip() {
        let mut idx = FileIndex::new(PathBuf::from("/tmp"), false);
        idx.add(FileEntry::new(PathBuf::from("/tmp/a"), 100, 1700000000));
        idx.add(
            FileEntry::new(PathBuf::from("/tmp/b"), 100, 1700000001)
                .with_hash("deadbeef".to_string()),
        );
        // Round-trip via serde clears the skipped bucket fields.
        let json = serde_json::to_string(&idx).unwrap();
        let mut deser: FileIndex = serde_json::from_str(&json).unwrap();
        assert!(deser.size_index.is_empty());
        deser.rebuild_indexes();
        assert_eq!(deser.size_index.get(&100).unwrap().len(), 2);
        assert_eq!(
            deser.hash_index.get(&(100, "deadbeef".to_string())).unwrap().len(),
            1
        );
    }
}
