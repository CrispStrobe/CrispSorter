//! Directory scanner with explicit completeness and optional parallel hashing.
//!
//! jwalk enumerates directories in parallel; per-file metadata is consumed by
//! the calling iterator. Hashing is a separate Rayon pass. No-hash scans read
//! metadata only. A scan is not an atomic snapshot of a changing filesystem.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

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

impl std::str::FromStr for HashAlgo {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "md5" => Ok(Self::Md5),
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            _ => Err(format!("unknown hash algorithm `{value}`; expected md5, sha1, or sha256")),
        }
    }
}

/// Detailed diagnostics are bounded independently of the catalog's file count.
pub const MAX_SCAN_ISSUES: usize = 256;
const HASH_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanIssueKind {
    Traversal,
    Metadata,
    ModifiedTime,
    Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub kind: ScanIssueKind,
    pub message: String,
}

/// Successfully observed files plus a completeness report. A partial result
/// must not silently replace an existing catalog. CAF has no completeness or
/// error fields; callers retaining a partial index must also retain its report.
#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub index: FileIndex,
    pub issues: Vec<ScanIssue>,
    pub suppressed_issues: u64,
    pub skipped_symlinks: u64,
    pub skipped_special_entries: u64,
    pub excluded_by_size: u64,
}

impl ScanReport {
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty() && self.suppressed_issues == 0
    }

    pub fn issue_count(&self) -> u64 {
        self.issues.len() as u64 + self.suppressed_issues
    }

    /// Strict callers fail before writing or using a misleading partial index.
    pub fn require_complete(&self) -> io::Result<()> {
        if self.is_complete() {
            return Ok(());
        }
        let first = self.issues.first().map(|issue| {
            format!("; first {:?} error at {}: {}", issue.kind, issue.path.display(), issue.message)
        }).unwrap_or_default();
        Err(io::Error::other(format!(
            "incomplete scan of {}: {} issue(s), {} detail(s) suppressed{first}",
            self.index.root_path.display(), self.issue_count(), self.suppressed_issues,
        )))
    }

    pub fn into_complete_index(self) -> io::Result<FileIndex> {
        self.require_complete()?;
        Ok(self.index)
    }
}

#[derive(Default)]
struct Diagnostics {
    issues: Vec<ScanIssue>,
    suppressed: u64,
}

impl Diagnostics {
    fn push(&mut self, path: &Path, kind: ScanIssueKind, error: impl std::fmt::Display) {
        if self.issues.len() < MAX_SCAN_ISSUES {
            self.issues.push(ScanIssue {
                path: path.to_owned(), kind,
                message: error.to_string().chars().take(2048).collect(),
            });
        } else {
            self.suppressed += 1;
        }
    }

    fn merge(&mut self, other: Self) {
        self.suppressed += other.suppressed;
        for issue in other.issues {
            if self.issues.len() < MAX_SCAN_ISSUES {
                self.issues.push(issue);
            } else {
                self.suppressed += 1;
            }
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

/// Compatibility entry point for callers requiring a complete catalog.
/// Invalid roots and observed traversal, metadata, timestamp, or hash failures
/// return an error. Use [`scan_dir_with_report`] to inspect partial results.
pub fn scan_dir(root: &Path, options: ScanOptions) -> io::Result<FileIndex> {
    scan_dir_with_report(root, options)?.into_complete_index()
}

/// Keep all successfully observed files and report failures rather than
/// dropping them silently. Size exclusions, ordinary directories, skipped
/// links, and special non-file entries are intentional, not scan failures.
pub fn scan_dir_with_report(root: &Path, options: ScanOptions) -> io::Result<ScanReport> {
    // jwalk has always followed an explicitly selected root symlink, even
    // when descendant links are disabled. Preserve that caller-facing policy.
    let root_metadata = fs::metadata(root)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", root.display())))?;
    if !root_metadata.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("{} is not a directory", root.display())));
    }

    // Keep the established jwalk default. A warm-cache fixture alone does not
    // justify changing traversal policy on other filesystems or devices.
    let walker = jwalk::WalkDir::new(root)
        .follow_links(options.follow_symlinks)
        .skip_hidden(false);
    Ok(scan_entries(root, options, walker))
}

fn scan_entries(
    root: &Path,
    options: ScanOptions,
    walker: impl IntoIterator<Item = jwalk::Result<jwalk::DirEntry<((), ())>>>,
) -> ScanReport {
    let mut index = FileIndex::new(root.to_owned(), cfg!(windows));
    let mut diagnostics = Diagnostics::default();
    let (mut skipped_symlinks, mut skipped_special_entries, mut excluded_by_size) = (0, 0, 0);
    for result in walker {
        let mut entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(error.path().unwrap_or(root), ScanIssueKind::Traversal, &error);
                continue;
            }
        };
        // jwalk attaches failures opening a directory to the directory entry,
        // not necessarily to an Err iterator item. Inspect before filtering.
        if let Some(error) = entry.read_children_error.take() {
            diagnostics.push(error.path().unwrap_or(root), ScanIssueKind::Traversal, &error);
        }
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            // The explicitly selected root alias is followed by jwalk; it is
            // not an omitted descendant and must not inflate this counter.
            if entry.depth() != 0 { skipped_symlinks += 1; }
            continue;
        }
        if !file_type.is_file() {
            if !file_type.is_dir() { skipped_special_entries += 1; }
            continue;
        }
        let path = entry.path();
        // Reuse this owned path rather than constructing it again inside
        // jwalk::DirEntry::metadata. Follow only an explicitly followed link.
        let metadata = if entry.path_is_symlink() {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        let metadata = match metadata {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                diagnostics.push(&path, ScanIssueKind::Metadata,
                    "Entry is no longer a regular file");
                continue;
            }
            Err(error) => {
                diagnostics.push(&path, ScanIssueKind::Metadata, error);
                continue;
            }
        };
        let size = metadata.len();
        if options.max_size_bytes.is_some_and(|max| size > max) {
            excluded_by_size += 1;
            continue;
        }
        let mtime = match metadata.modified() {
            Ok(time) => match time.duration_since(UNIX_EPOCH)
                .ok().and_then(|duration| u32::try_from(duration.as_secs()).ok()) {
                    Some(seconds) => seconds,
                    None => {
                        diagnostics.push(&path, ScanIssueKind::ModifiedTime,
                            "Modification time is outside the CAF unsigned 32-bit epoch range");
                        0
                    }
                },
            Err(error) => {
                diagnostics.push(&path, ScanIssueKind::ModifiedTime, error);
                0
            }
        };
        // No intermediate raw/hashed vector in the common metadata-only path.
        index.add(FileEntry::new(path, size, mtime));
    }

    if let Some(algo) = options.hash {
        diagnostics.merge(hash_entries(&mut index.all_files, algo));
        // Size buckets are already valid; only the now-populated hash buckets
        // need construction after in-place hashing.
        for (position, entry) in index.all_files.iter().enumerate() {
            if let Some(hash) = &entry.hash {
                index.hash_index.entry((entry.size, hash.clone())).or_default().push(position);
            }
        }
    }

    ScanReport { index, issues: diagnostics.issues,
        suppressed_issues: diagnostics.suppressed, skipped_symlinks,
        skipped_special_entries, excluded_by_size }
}

fn hash_entries(entries: &mut [FileEntry], algo: HashAlgo) -> Diagnostics {
    let diagnostics = Mutex::new(Diagnostics::default());
    // Reuse a streaming buffer within each Rayon job instead of allocating and
    // zeroing a MiB for every small file. Rayon may create more jobs than threads.
    entries.par_iter_mut().for_each_init(|| vec![0u8; HASH_BUFFER_BYTES], |buffer, entry| {
        let result = fs::File::open(&entry.path)
            .and_then(|mut file| hash_reader(&mut file, algo, buffer));
        match result {
            Ok(hash) => entry.hash = Some(hash),
            Err(error) => diagnostics.lock().unwrap()
                .push(&entry.path, ScanIssueKind::Hash, error),
        }
    });
    diagnostics.into_inner().unwrap()
}

/// Hash one file with a bounded streaming buffer. The scan's parallel pass
/// reuses its own buffers; this standalone helper remains API-compatible.
pub fn hash_file(path: &Path, algo: HashAlgo) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    hash_reader(&mut file, algo, &mut vec![0u8; HASH_BUFFER_BYTES])
}

fn hash_reader(reader: &mut impl Read, algo: HashAlgo, buffer: &mut [u8]) -> io::Result<String> {
    match algo {
        HashAlgo::Md5 => digest_reader::<md5::Md5>(reader, buffer),
        HashAlgo::Sha1 => digest_reader::<sha1::Sha1>(reader, buffer),
        HashAlgo::Sha256 => digest_reader::<sha2::Sha256>(reader, buffer),
    }
}

fn digest_reader<D: sha2::Digest>(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<String> {
    let mut digest = D::new();
    loop {
        match reader.read(buffer) {
            Ok(0) => break,
            Ok(count) => digest.update(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(hex::encode(digest.finalize()))
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

    #[test]
    fn invalid_roots_are_errors_instead_of_empty_successes() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("file");
        fs::write(&file, b"data").unwrap();
        assert_eq!(scan_dir(&file, Default::default()).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        let missing = tmp.path().join("missing");
        let error = scan_dir_with_report(&missing, Default::default()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn metadata_failure_retains_siblings_and_rejects_complete_conversion() {
        let tmp = TempDir::new().unwrap();
        let gone = tmp.path().join("gone");
        fs::write(&gone, b"removed before stat").unwrap();
        fs::write(tmp.path().join("keep"), b"keep").unwrap();
        // Capture actual listing entries, then mutate deterministically before
        // the same production metadata-consumption function runs. No sleeps.
        let listed: Vec<_> = jwalk::WalkDir::new(tmp.path()).into_iter().collect();
        fs::remove_file(&gone).unwrap();
        let report = scan_entries(tmp.path(), Default::default(), listed);
        assert_eq!(report.index.len(), 1);
        assert_eq!(report.index.total_size(), 4);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].kind, ScanIssueKind::Metadata);
        assert_eq!(report.issues[0].path, gone);
        assert!(!report.is_complete());
        assert!(report.into_complete_index().unwrap_err().to_string().contains("gone"));
    }

    #[test]
    fn changed_entry_type_is_not_catalogued_with_a_directory_size() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("changed");
        fs::write(&path, b"file").unwrap();
        let listed: Vec<_> = jwalk::WalkDir::new(tmp.path()).into_iter().collect();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let report = scan_entries(tmp.path(), Default::default(), listed);
        assert!(report.index.is_empty());
        assert_eq!(report.issues[0].kind, ScanIssueKind::Metadata);
        assert!(!report.is_complete());
    }

    #[test]
    fn directory_read_errors_attached_to_jwalk_entries_are_reported() {
        let tmp = TempDir::new().unwrap();
        let directory = tmp.path().join("removed-directory");
        fs::create_dir(&directory).unwrap();
        fs::write(tmp.path().join("keep"), b"ok").unwrap();
        let remove = directory.clone();
        let walker = jwalk::WalkDir::new(tmp.path()).process_read_dir(move |depth, _, _, _| {
            if depth == Some(0) {
                // jwalk has listed the child, but has not scheduled reading it
                // until this callback returns. This is a deterministic failure.
                fs::remove_dir(&remove).unwrap();
            }
        });
        let report = scan_entries(tmp.path(), Default::default(), walker);
        assert_eq!(report.index.len(), 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].kind, ScanIssueKind::Traversal);
        assert_eq!(report.issues[0].path, directory);
        assert!(!report.is_complete());
    }

    #[test]
    fn many_failed_stats_keep_a_bounded_report_and_exact_error_count() {
        let tmp = TempDir::new().unwrap();
        for number in 0..MAX_SCAN_ISSUES + 3 {
            fs::write(tmp.path().join(format!("file-{number}")), b"x").unwrap();
        }
        let listed: Vec<_> = jwalk::WalkDir::new(tmp.path()).into_iter().collect();
        for entry in &listed {
            let entry = entry.as_ref().unwrap();
            if entry.file_type().is_file() { fs::remove_file(entry.path()).unwrap(); }
        }
        let report = scan_entries(tmp.path(), Default::default(), listed);
        assert!(report.index.is_empty());
        assert_eq!(report.issues.len(), MAX_SCAN_ISSUES);
        assert_eq!(report.suppressed_issues, 3);
        assert_eq!(report.issue_count(), MAX_SCAN_ISSUES as u64 + 3);
        assert!(report.require_complete().is_err());
    }

    #[test]
    fn failed_hash_does_not_discard_metadata_or_other_hashes() {
        let tmp = TempDir::new().unwrap();
        let good = tmp.path().join("good");
        let gone = tmp.path().join("gone");
        fs::write(&good, b"abc").unwrap();
        let mut entries = vec![FileEntry::new(good, 3, 1), FileEntry::new(gone.clone(), 9, 1)];
        let diagnostics = hash_entries(&mut entries, HashAlgo::Sha256);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].hash.as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert_eq!(entries[1].size, 9);
        assert!(entries[1].hash.is_none());
        assert_eq!(diagnostics.issues.len(), 1);
        assert_eq!(diagnostics.issues[0].kind, ScanIssueKind::Hash);
        assert_eq!(diagnostics.issues[0].path, gone);
    }

    #[test]
    fn reused_hash_buffers_preserve_all_algorithms_and_hash_buckets() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("abc"), b"abc").unwrap();
        fs::write(tmp.path().join("zero"), b"").unwrap();
        for (algo, abc, zero) in [
            (HashAlgo::Md5, "900150983cd24fb0d6963f7d28e17f72", "d41d8cd98f00b204e9800998ecf8427e"),
            (HashAlgo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (HashAlgo::Sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        ] {
            let report = scan_dir_with_report(tmp.path(), ScanOptions { hash: Some(algo), ..Default::default() }).unwrap();
            assert!(report.is_complete());
            for (size, hash) in [(3, abc), (0, zero)] {
                let positions = &report.index.hash_index[&(size, hash.to_owned())];
                assert_eq!(positions.len(), 1);
                assert_eq!(report.index.all_files[positions[0]].hash.as_deref(), Some(hash));
            }
            assert_eq!(report.index.by_size(0).count(), 1);
        }
    }

    #[test]
    fn streaming_hash_handles_more_than_one_buffer_and_interrupted_reads() {
        use sha2::Digest;
        let data = vec![0x5a; HASH_BUFFER_BYTES * 2 + 37];
        let expected = hex::encode(sha2::Sha256::digest(&data));
        struct InterruptedOnce { first: bool, bytes: io::Cursor<Vec<u8>> }
        impl Read for InterruptedOnce {
            fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else { self.bytes.read(target) }
            }
        }
        let mut reader = InterruptedOnce { first: true, bytes: io::Cursor::new(data) };
        assert_eq!(hash_reader(&mut reader, HashAlgo::Sha256, &mut vec![0; HASH_BUFFER_BYTES]).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn selected_root_alias_is_followed_but_descendant_links_are_counted_and_skipped() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("real");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(".hidden"), b"abc").unwrap();
        fs::write(root.join("large"), b"0123456789").unwrap();
        symlink(root.join(".hidden"), root.join("link")).unwrap();
        symlink(root.join("missing"), root.join("broken")).unwrap();
        let alias = tmp.path().join("alias");
        symlink(&root, &alias).unwrap();
        let report = scan_dir_with_report(&alias, ScanOptions { max_size_bytes: Some(3), ..Default::default() }).unwrap();
        assert!(report.is_complete());
        assert_eq!(report.skipped_symlinks, 2);
        assert_eq!(report.excluded_by_size, 1);
        assert_eq!(report.index.len(), 1);
        assert_eq!(report.index.all_files[0].path, alias.join(".hidden"));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_link_following_preserves_file_aliases_and_reports_broken_links() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("data"), b"abc").unwrap();
        symlink(tmp.path().join("data"), tmp.path().join("alias")).unwrap();
        symlink(tmp.path().join("absent"), tmp.path().join("broken")).unwrap();
        let report = scan_dir_with_report(tmp.path(), ScanOptions { follow_symlinks: true, hash: Some(HashAlgo::Md5), ..Default::default() }).unwrap();
        assert_eq!(report.index.len(), 2);
        assert!(report.index.all_files.iter().all(|entry| entry.hash.as_deref() == Some("900150983cd24fb0d6963f7d28e17f72")));
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].kind, ScanIssueKind::Traversal);
        assert!(!report.is_complete());
    }

    #[cfg(unix)]
    #[test]
    fn timestamps_outside_caf_range_are_reported_without_losing_file_metadata() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("old");
        let file = fs::File::create(&path).unwrap();
        file.set_len(7).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH - std::time::Duration::from_secs(1))).unwrap();
        let report = scan_dir_with_report(tmp.path(), Default::default()).unwrap();
        assert_eq!(report.index.total_size(), 7);
        assert_eq!(report.index.all_files[0].mtime, 0);
        assert_eq!(report.issues[0].kind, ScanIssueKind::ModifiedTime);
        assert!(report.require_complete().is_err());
    }
}
