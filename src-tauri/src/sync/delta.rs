//! P29.2 — Block-level delta sync.
//!
//! Computes block-level differences between local and remote files so that
//! only changed blocks need uploading.  Uses Adler-32 weak hash + SHA-256
//! strong hash per block (default 4 MB).
//!
//! # Algorithm
//!
//! 1. Compute a `Blockmap` for both local and remote copies of a file.
//!    Each block records its offset, size, Adler-32 rolling checksum, and
//!    SHA-256 content hash.
//! 2. Compare blockmaps: blocks whose strong hashes differ (or that don't
//!    exist in the remote map) are marked as `ChangedBlock`.
//! 3. Only `ChangedBlock` data is uploaded, saving bandwidth proportional
//!    to the fraction of the file that changed.
//!
//! Lance `.lance` data files are append-mostly (new row groups appended,
//! old ones rarely rewritten), so delta sync naturally exploits this —
//! only tail blocks change.  Tantivy segments are immutable once written;
//! only `meta.json` + new segments need uploading.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Default block size: 4 MB.
///
/// This is a *protocol* constant, not a local tuning knob. The same value
/// appears in the `crispcloud_delta` server app, the oCIS Go sidecar, the
/// Dart reference client and the ownCloud/Nextcloud desktop forks, and the
/// block map wire format carries it as `blockSize`. Two peers that chunk a
/// file differently produce maps that cannot be compared.
pub const DEFAULT_BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// Smallest block size we will accept.
///
/// A tiny block size on a large file is pathological — 4-byte blocks over
/// a gigabyte is 268 million blocks, each carrying an Adler-32 and a
/// SHA-256. Since the size arrives from a peer (see below), that is also a
/// cheap denial-of-service, so there has to be a limit somewhere.
///
/// This limit is ours alone: **no other implementation in this protocol
/// clamps or floors the block size.** Verified against the sources, not
/// the docs — `BlockMapService.php`, `ocis/internal/blockmap/blockmap.go`,
/// `client/lib/delta_sync.dart` and `deltasyncutils.cpp` in both desktop
/// forks all use the requested value as-is.
pub const MIN_BLOCK_SIZE: usize = 1024;

/// Validate a requested block size, returning it unchanged.
///
/// # Why this rejects instead of clamping
///
/// Block size is a *negotiated protocol value*, not a local preference.
/// The desktop clients adopt whatever the server advertises:
///
/// ```text
/// // propagateuploaddelta.cpp:108 (ownCloud fork), :128 (Nextcloud fork)
/// qint64 blockSize = _remoteBlockMap.blockSize > 0
///     ? _remoteBlockMap.blockSize : DefaultBlockSize;
/// _localBlockMap = DeltaSyncUtils::computeLocalBlockMap(localPath, blockSize);
/// ```
///
/// That is why the C++ side needs no compatibility check: it computes its
/// local map with the *remote's* size, so the two always align by
/// construction.
///
/// Silently clamping would break exactly that. If a server advertised a
/// size below our floor, every other client would honour it and we would
/// quietly chunk differently — producing a map that claims to describe the
/// file and does not agree with anyone else's. Refusing is worse for that
/// one file and better for every other, because the failure is legible.
pub fn validate_block_size(requested: usize) -> Result<usize> {
    if requested < MIN_BLOCK_SIZE {
        anyhow::bail!(
            "block size {requested} is below our minimum of {MIN_BLOCK_SIZE}; \
             refusing rather than silently using a different size from the peer"
        );
    }
    Ok(requested)
}

// ── Types ────────────────────────────────────────────────────────────────

/// A single block's metadata within a blockmap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// Byte offset from the start of the file.
    pub offset: u64,
    /// Size of this block in bytes (last block may be smaller).
    pub size: u32,
    /// Adler-32 rolling checksum for fast comparison.
    pub weak_hash: u32,
    /// SHA-256 content hash for strong verification.
    pub strong_hash: [u8; 32],
}

/// Complete blockmap for a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blockmap {
    /// Total file size in bytes.
    pub file_size: u64,
    /// Block size used to compute this map.
    pub block_size: u32,
    /// Ordered list of blocks (by offset).
    pub blocks: Vec<Block>,
}

/// A block that differs between local and remote and needs uploading.
#[derive(Debug, Clone)]
pub struct ChangedBlock {
    /// Index into the local blockmap's `blocks` vec.
    pub block_index: usize,
    /// Byte offset in the file.
    pub offset: u64,
    /// Size of the changed block.
    pub size: u32,
}

/// Summary of a delta diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaSummary {
    /// Total file size.
    pub file_size: u64,
    /// Number of blocks in the file.
    pub total_blocks: usize,
    /// Number of blocks that changed.
    pub changed_blocks: usize,
    /// Total bytes that need uploading.
    pub changed_bytes: u64,
    /// Bandwidth savings as a ratio (0.0 = no savings, 1.0 = all identical).
    pub savings_ratio: f64,
}

// ── Adler-32 ─────────────────────────────────────────────────────────────

/// Compute the Adler-32 checksum of a byte slice.
///
/// Implemented inline to avoid an extra dependency — the algorithm is
/// only ~10 lines.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;

    for &byte in data {
        a = (a + byte as u32) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }

    (b << 16) | a
}

// ── Blockmap computation ─────────────────────────────────────────────────

/// Compute a blockmap for a file on disk.
pub fn compute_blockmap(path: &Path, block_size: usize) -> Result<Blockmap> {
    let block_size = validate_block_size(block_size)?;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let file_size = file.metadata()?.len();

    let mut blocks = Vec::new();
    let mut buf = vec![0u8; block_size];
    let mut offset: u64 = 0;

    loop {
        let n = read_full(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        let data = &buf[..n];

        let weak_hash = adler32(data);
        let strong_hash: [u8; 32] = Sha256::digest(data).into();

        blocks.push(Block {
            offset,
            size: n as u32,
            weak_hash,
            strong_hash,
        });

        offset += n as u64;
    }

    Ok(Blockmap {
        file_size,
        block_size: block_size as u32,
        blocks,
    })
}

/// Compute a blockmap from an in-memory byte slice (useful for testing
/// and for small files).
pub fn compute_blockmap_from_bytes(data: &[u8], block_size: usize) -> Result<Blockmap> {
    let block_size = validate_block_size(block_size)?;
    let mut blocks = Vec::new();
    let mut offset: u64 = 0;

    for chunk in data.chunks(block_size) {
        let weak_hash = adler32(chunk);
        let strong_hash: [u8; 32] = Sha256::digest(chunk).into();
        blocks.push(Block {
            offset,
            size: chunk.len() as u32,
            weak_hash,
            strong_hash,
        });
        offset += chunk.len() as u64;
    }

    Ok(Blockmap {
        file_size: data.len() as u64,
        block_size: block_size as u32,
        blocks,
    })
}

// ── Block size negotiation ───────────────────────────────────────────────

/// The block size to use when computing a local map to compare against
/// `remote`.
///
/// The server dictates it and the client adopts it. This mirrors the
/// desktop clients exactly — `propagateuploaddelta.cpp:108` (ownCloud
/// fork), `:128` (Nextcloud fork):
///
/// ```text
/// qint64 blockSize = _remoteBlockMap.blockSize > 0
///     ? _remoteBlockMap.blockSize : DefaultBlockSize;
/// ```
///
/// A map with `block_size == 0` means the peer did not state one, so we
/// fall back to [`DEFAULT_BLOCK_SIZE`] as they do.
pub fn negotiated_block_size(remote: &Blockmap) -> Result<usize> {
    let requested = if remote.block_size > 0 {
        remote.block_size as usize
    } else {
        DEFAULT_BLOCK_SIZE
    };
    validate_block_size(requested).with_context(|| {
        format!(
            "peer advertised a block size of {} which we cannot honour",
            remote.block_size
        )
    })
}

/// Compute a local blockmap laid out to match `remote`.
///
/// **This is the function a sync driver should call**, not
/// [`compute_blockmap`] with [`DEFAULT_BLOCK_SIZE`]. Chunking to our own
/// preferred size and then diffing against a peer that chose differently
/// is the mistake [`diff_blockmaps`] exists to catch — and catching it
/// means refusing to sync. Adopting the peer's size means the diff simply
/// works.
pub fn compute_local_blockmap_against(path: &Path, remote: &Blockmap) -> Result<Blockmap> {
    compute_blockmap(path, negotiated_block_size(remote)?)
}

/// In-memory counterpart of [`compute_local_blockmap_against`].
pub fn compute_local_blockmap_from_bytes_against(
    data: &[u8],
    remote: &Blockmap,
) -> Result<Blockmap> {
    compute_blockmap_from_bytes(data, negotiated_block_size(remote)?)
}

// ── Diff ─────────────────────────────────────────────────────────────────

/// Compare local and remote blockmaps and return the list of blocks that
/// differ.  Blocks that exist in the local map but not in the remote
/// (i.e. the file grew) are also included.
///
/// # Block sizes must match
///
/// Blocks are compared *by index*, so this is only meaningful when both
/// maps chunked the file the same way. Comparing a map of 4 MB blocks
/// against one of 1 KB blocks pairs block 0 with block 0 and calls them
/// different — and the resulting [`ChangedBlock`] carries the *local*
/// offset and size, which would then be applied against a remote file
/// laid out differently. That is not a wasted upload, it is a corrupted
/// file.
///
/// The map carries `block_size` precisely so this can be checked, and
/// peers in this protocol are separate implementations (PHP server, Go
/// sidecar, Dart client, the desktop forks) that must agree on it. So a
/// mismatch is refused rather than guessed at: the caller can fall back
/// to a full upload deliberately, which is a different thing from doing
/// it by accident.
pub fn diff_blockmaps(local: &Blockmap, remote: &Blockmap) -> Result<Vec<ChangedBlock>> {
    if local.block_size != remote.block_size {
        anyhow::bail!(
            "block size mismatch: local map uses {} bytes, remote uses {} — \
             these maps are not comparable",
            local.block_size,
            remote.block_size
        );
    }
    let mut changed = Vec::new();

    for (i, local_block) in local.blocks.iter().enumerate() {
        let is_changed = match remote.blocks.get(i) {
            Some(remote_block) => {
                // Fast path: compare weak hash first.
                if local_block.weak_hash != remote_block.weak_hash {
                    true
                } else {
                    // Weak hash matches — verify with strong hash.
                    local_block.strong_hash != remote_block.strong_hash
                }
            }
            // Block doesn't exist on the remote (file grew).
            None => true,
        };

        if is_changed {
            changed.push(ChangedBlock {
                block_index: i,
                offset: local_block.offset,
                size: local_block.size,
            });
        }
    }

    Ok(changed)
}

/// Compute a summary of the delta between two blockmaps.
pub fn delta_summary(local: &Blockmap, changed: &[ChangedBlock]) -> DeltaSummary {
    let changed_bytes: u64 = changed.iter().map(|c| c.size as u64).sum();
    let total_blocks = local.blocks.len();
    let savings = if local.file_size > 0 {
        1.0 - (changed_bytes as f64 / local.file_size as f64)
    } else {
        1.0
    };

    DeltaSummary {
        file_size: local.file_size,
        total_blocks,
        changed_blocks: changed.len(),
        changed_bytes,
        savings_ratio: savings,
    }
}

/// Read exactly `buf.len()` bytes (or less at EOF).  Returns the number
/// of bytes actually read.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn adler32_known_values() {
        // "Wikipedia" example from the Adler-32 spec.
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
    }

    #[test]
    fn adler32_empty() {
        assert_eq!(adler32(b""), 1); // initial a=1, b=0 → 0x00000001
    }

    #[test]
    fn blockmap_from_bytes_single_block() {
        let data = b"hello world";
        let map = compute_blockmap_from_bytes(data, 1024).unwrap();
        assert_eq!(map.file_size, 11);
        assert_eq!(map.blocks.len(), 1);
        assert_eq!(map.blocks[0].offset, 0);
        assert_eq!(map.blocks[0].size, 11);
    }

    #[test]
    fn blockmap_from_bytes_multiple_blocks() {
        // Sized in KB: block sizes below MIN_BLOCK_SIZE are clamped, so a
        // 30-byte block would collapse the whole thing into one block and
        // stop testing the chunking at all.
        const KB: usize = 1024;
        let data = vec![0u8; 100 * KB];
        let map = compute_blockmap_from_bytes(&data, 30 * KB).unwrap();
        assert_eq!(map.blocks.len(), 4); // 30+30+30+10
        assert_eq!(map.blocks[0].size as usize, 30 * KB);
        assert_eq!(map.blocks[3].size as usize, 10 * KB);
        assert_eq!(map.blocks[3].offset as usize, 90 * KB);
    }

    #[test]
    fn blockmap_from_bytes_empty() {
        let map = compute_blockmap_from_bytes(b"", 1024).unwrap();
        assert_eq!(map.file_size, 0);
        assert!(map.blocks.is_empty());
    }

    #[test]
    fn negotiation_adopts_the_peers_block_size() {
        // The server dictates; we follow. Mirrors propagateuploaddelta.cpp.
        let remote = compute_blockmap_from_bytes(&vec![0u8; 8192], 2048).unwrap();
        assert_eq!(negotiated_block_size(&remote).unwrap(), 2048);
    }

    #[test]
    fn negotiation_falls_back_when_the_peer_states_nothing() {
        // blockSize == 0 is the "not stated" case the C++ ternary handles.
        let remote = Blockmap { file_size: 0, block_size: 0, blocks: Vec::new() };
        assert_eq!(negotiated_block_size(&remote).unwrap(), DEFAULT_BLOCK_SIZE);
    }

    #[test]
    fn negotiation_refuses_a_peer_size_we_cannot_honour() {
        let remote = Blockmap { file_size: 10, block_size: 8, blocks: Vec::new() };
        let err = negotiated_block_size(&remote).unwrap_err().to_string();
        assert!(err.contains("cannot honour"), "{err}");
    }

    #[test]
    fn adopting_the_peers_size_makes_the_diff_succeed() {
        // The whole point: chunking to our own preference and then diffing
        // is what the block-size guard refuses. Adopting theirs works.
        let data = vec![3u8; 16384];
        let remote = compute_blockmap_from_bytes(&data, 2048).unwrap();

        let wrong = compute_blockmap_from_bytes(&data, DEFAULT_BLOCK_SIZE).unwrap();
        assert!(
            diff_blockmaps(&wrong, &remote).is_err(),
            "using our own block size should be refused"
        );

        let right = compute_local_blockmap_from_bytes_against(&data, &remote).unwrap();
        assert_eq!(right.block_size, remote.block_size);
        assert!(diff_blockmaps(&right, &remote).unwrap().is_empty());
    }

    #[test]
    fn adopting_from_a_file_matches_the_peer_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let data = vec![9u8; 12288];
        std::fs::write(&path, &data).unwrap();
        let remote = compute_blockmap_from_bytes(&data, 4096).unwrap();

        let local = compute_local_blockmap_against(&path, &remote).unwrap();
        assert_eq!(local.block_size, 4096);
        assert_eq!(local.blocks.len(), remote.blocks.len());
        assert!(diff_blockmaps(&local, &remote).unwrap().is_empty());
    }

    #[test]
    fn both_constructors_agree_for_every_accepted_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.bin");
        let data = vec![7u8; 8192];
        std::fs::write(&path, &data).unwrap();
        for requested in [MIN_BLOCK_SIZE, 2048, 4096, 8192, DEFAULT_BLOCK_SIZE] {
            let a = compute_blockmap(&path, requested).unwrap();
            let b = compute_blockmap_from_bytes(&data, requested).unwrap();
            assert_eq!(a.block_size, b.block_size, "disagreed at requested={requested}");
            assert_eq!(a.blocks.len(), b.blocks.len(), "disagreed at requested={requested}");
        }
    }

    #[test]
    fn both_constructors_reject_the_same_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.bin");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        for requested in [0usize, 1, 4, 512, MIN_BLOCK_SIZE - 1] {
            assert!(compute_blockmap(&path, requested).is_err(), "file path accepted {requested}");
            assert!(
                compute_blockmap_from_bytes(&[7u8; 4096], requested).is_err(),
                "bytes path accepted {requested}"
            );
        }
    }

    #[test]
    fn an_accepted_block_size_is_returned_untouched() {
        // The peer dictates this value; rounding it would silently
        // desynchronise us from every other implementation.
        assert_eq!(validate_block_size(MIN_BLOCK_SIZE).unwrap(), MIN_BLOCK_SIZE);
        assert_eq!(validate_block_size(1500).unwrap(), 1500);
        assert_eq!(validate_block_size(DEFAULT_BLOCK_SIZE).unwrap(), DEFAULT_BLOCK_SIZE);
    }

    #[test]
    fn a_too_small_block_size_is_refused_not_rounded_up() {
        // Clamping here would produce a map that disagrees with the server
        // while claiming to describe the same file.
        for bad in [0usize, 1, 512, MIN_BLOCK_SIZE - 1] {
            let err = validate_block_size(bad).unwrap_err().to_string();
            assert!(err.contains("below our minimum"), "{err}");
        }
    }

    #[test]
    fn the_recorded_block_size_is_the_one_requested() {
        // A peer reads `block_size` off the wire to decide comparability,
        // so a map must never misreport how it was chunked.
        let map = compute_blockmap_from_bytes(&[0u8; 100], 4096).unwrap();
        assert_eq!(map.block_size, 4096);
        assert_eq!(map.blocks.len(), 1, "100 bytes at a 4 KB block is one block");
    }

    #[test]
    fn diffing_maps_with_different_block_sizes_is_refused() {
        // Blocks are compared by index, so mismatched chunking would pair
        // block 0 with block 0 and hand back a ChangedBlock whose offset
        // and size belong to a different layout. Corruption, not waste.
        let data = vec![1u8; 8192];
        let local = compute_blockmap_from_bytes(&data, 1024).unwrap();
        let remote = compute_blockmap_from_bytes(&data, 2048).unwrap();
        assert_ne!(local.block_size, remote.block_size);
        let err = diff_blockmaps(&local, &remote).unwrap_err().to_string();
        assert!(err.contains("block size mismatch"), "{err}");
    }

    #[test]
    fn matching_block_sizes_still_diff_normally() {
        let data = vec![1u8; 8192];
        let local = compute_blockmap_from_bytes(&data, 2048).unwrap();
        let remote = compute_blockmap_from_bytes(&data, 2048).unwrap();
        assert!(diff_blockmaps(&local, &remote).unwrap().is_empty());
    }

    #[test]
    fn blockmap_from_file_matches_bytes() {
        // Regression: the two constructors clamped the block size
        // differently (1 KB vs 1 byte), so the same file chunked through
        // each produced maps that could not be compared.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let data = b"abcdefghij"; // 10 bytes
        std::fs::write(&path, data).unwrap();

        // 4 KB, not 4 bytes: sizes below MIN_BLOCK_SIZE are now refused
        // outright rather than clamped, so the constructors can only be
        // compared at a size both will accept.
        let from_file = compute_blockmap(&path, 4 * 1024).unwrap();
        let from_bytes = compute_blockmap_from_bytes(data, 4 * 1024).unwrap();

        assert_eq!(
            from_file.block_size, from_bytes.block_size,
            "constructors disagree on the effective block size"
        );

        assert_eq!(from_file.blocks.len(), from_bytes.blocks.len());
        for (f, b) in from_file.blocks.iter().zip(from_bytes.blocks.iter()) {
            assert_eq!(f.offset, b.offset);
            assert_eq!(f.size, b.size);
            assert_eq!(f.weak_hash, b.weak_hash);
            assert_eq!(f.strong_hash, b.strong_hash);
        }
    }

    #[test]
    fn diff_identical_files_is_empty() {
        let data = vec![42u8; 200];
        let map = compute_blockmap_from_bytes(&data, 50 * 1024).unwrap();
        let changed = diff_blockmaps(&map, &map).unwrap();
        assert!(changed.is_empty());
    }

    #[test]
    fn diff_detects_single_block_change() {
        const KB: usize = 1024;
        let mut data = vec![0u8; 200 * KB];
        let remote = compute_blockmap_from_bytes(&data, 50 * KB).unwrap();

        // Modify the 3rd block (offset 100..150 KB).
        data[120 * KB] = 0xFF;
        let local = compute_blockmap_from_bytes(&data, 50 * KB).unwrap();

        let changed = diff_blockmaps(&local, &remote).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].block_index, 2);
        assert_eq!(changed[0].offset as usize, 100 * KB);
        assert_eq!(changed[0].size as usize, 50 * KB);
    }

    #[test]
    fn diff_detects_file_growth() {
        const KB: usize = 1024;
        let small = vec![0u8; 100 * KB];
        let large = vec![0u8; 200 * KB];
        let remote = compute_blockmap_from_bytes(&small, 50 * KB).unwrap();
        let local = compute_blockmap_from_bytes(&large, 50 * KB).unwrap();

        let changed = diff_blockmaps(&local, &remote).unwrap();
        // Blocks 0 and 1 are identical; blocks 2 and 3 are new.
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[0].block_index, 2);
        assert_eq!(changed[1].block_index, 3);
    }

    #[test]
    fn diff_all_changed_when_remote_empty() {
        const KB: usize = 1024;
        let data = vec![1u8; 100 * KB];
        let local = compute_blockmap_from_bytes(&data, 50 * KB).unwrap();
        let remote = Blockmap {
            file_size: 0,
            // Must match the local map's block size, or the diff is
            // refused — which is the point of the guard.
            block_size: (50 * KB) as u32,
            blocks: Vec::new(),
        };

        let changed = diff_blockmaps(&local, &remote).unwrap();
        assert_eq!(changed.len(), 2);
    }

    #[test]
    fn delta_summary_computes_savings() {
        const KB: usize = 1024;
        let data = vec![0u8; 500 * KB];
        let local = compute_blockmap_from_bytes(&data, 100 * KB).unwrap();

        // Simulate: only 1 of 5 blocks changed.
        let changed = vec![ChangedBlock {
            block_index: 2,
            offset: (200 * KB) as u64,
            size: (100 * KB) as u32,
        }];

        let summary = delta_summary(&local, &changed);
        assert_eq!(summary.file_size as usize, 500 * KB);
        assert_eq!(summary.total_blocks, 5);
        assert_eq!(summary.changed_blocks, 1);
        assert_eq!(summary.changed_bytes as usize, 100 * KB);
        assert!((summary.savings_ratio - 0.8).abs() < 0.001);
    }

    #[test]
    fn delta_summary_empty_file() {
        let local = compute_blockmap_from_bytes(b"", 100 * 1024).unwrap();
        let summary = delta_summary(&local, &[]);
        assert_eq!(summary.savings_ratio, 1.0);
    }

    #[test]
    fn blockmap_serde_round_trips() {
        let data = b"test data for serde";
        let map = compute_blockmap_from_bytes(data, 8 * 1024).unwrap();
        let json = serde_json::to_string(&map).unwrap();
        let back: Blockmap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file_size, map.file_size);
        assert_eq!(back.blocks.len(), map.blocks.len());
        for (a, b) in back.blocks.iter().zip(map.blocks.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn file_smaller_than_one_block() {
        let data = b"tiny";
        let map = compute_blockmap_from_bytes(data, DEFAULT_BLOCK_SIZE).unwrap();
        assert_eq!(map.blocks.len(), 1);
        assert_eq!(map.blocks[0].size, 4);
        assert_eq!(map.blocks[0].offset, 0);
    }
}
