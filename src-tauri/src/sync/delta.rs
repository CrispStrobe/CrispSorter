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
pub const DEFAULT_BLOCK_SIZE: usize = 4 * 1024 * 1024;

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
    let block_size = block_size.max(1024); // minimum 1 KB
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
pub fn compute_blockmap_from_bytes(data: &[u8], block_size: usize) -> Blockmap {
    let block_size = block_size.max(1);
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

    Blockmap {
        file_size: data.len() as u64,
        block_size: block_size as u32,
        blocks,
    }
}

// ── Diff ─────────────────────────────────────────────────────────────────

/// Compare local and remote blockmaps and return the list of blocks that
/// differ.  Blocks that exist in the local map but not in the remote
/// (i.e. the file grew) are also included.
pub fn diff_blockmaps(local: &Blockmap, remote: &Blockmap) -> Vec<ChangedBlock> {
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

    changed
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
        let map = compute_blockmap_from_bytes(data, 1024);
        assert_eq!(map.file_size, 11);
        assert_eq!(map.blocks.len(), 1);
        assert_eq!(map.blocks[0].offset, 0);
        assert_eq!(map.blocks[0].size, 11);
    }

    #[test]
    fn blockmap_from_bytes_multiple_blocks() {
        let data = vec![0u8; 100];
        let map = compute_blockmap_from_bytes(&data, 30);
        assert_eq!(map.blocks.len(), 4); // 30+30+30+10
        assert_eq!(map.blocks[0].size, 30);
        assert_eq!(map.blocks[3].size, 10);
        assert_eq!(map.blocks[3].offset, 90);
    }

    #[test]
    fn blockmap_from_bytes_empty() {
        let map = compute_blockmap_from_bytes(b"", 1024);
        assert_eq!(map.file_size, 0);
        assert!(map.blocks.is_empty());
    }

    #[test]
    fn blockmap_from_file_matches_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let data = b"abcdefghij"; // 10 bytes
        std::fs::write(&path, data).unwrap();

        let from_file = compute_blockmap(&path, 4).unwrap();
        let from_bytes = compute_blockmap_from_bytes(data, 4);

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
        let map = compute_blockmap_from_bytes(&data, 50);
        let changed = diff_blockmaps(&map, &map);
        assert!(changed.is_empty());
    }

    #[test]
    fn diff_detects_single_block_change() {
        let mut data = vec![0u8; 200];
        let remote = compute_blockmap_from_bytes(&data, 50);

        // Modify the 3rd block (offset 100..150).
        data[120] = 0xFF;
        let local = compute_blockmap_from_bytes(&data, 50);

        let changed = diff_blockmaps(&local, &remote);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].block_index, 2);
        assert_eq!(changed[0].offset, 100);
        assert_eq!(changed[0].size, 50);
    }

    #[test]
    fn diff_detects_file_growth() {
        let small = vec![0u8; 100];
        let large = vec![0u8; 200];
        let remote = compute_blockmap_from_bytes(&small, 50);
        let local = compute_blockmap_from_bytes(&large, 50);

        let changed = diff_blockmaps(&local, &remote);
        // Blocks 0 and 1 are identical; blocks 2 and 3 are new.
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[0].block_index, 2);
        assert_eq!(changed[1].block_index, 3);
    }

    #[test]
    fn diff_all_changed_when_remote_empty() {
        let data = vec![1u8; 100];
        let local = compute_blockmap_from_bytes(&data, 50);
        let remote = Blockmap {
            file_size: 0,
            block_size: 50,
            blocks: Vec::new(),
        };

        let changed = diff_blockmaps(&local, &remote);
        assert_eq!(changed.len(), 2);
    }

    #[test]
    fn delta_summary_computes_savings() {
        let data = vec![0u8; 500];
        let local = compute_blockmap_from_bytes(&data, 100);

        // Simulate: only 1 of 5 blocks changed.
        let changed = vec![ChangedBlock {
            block_index: 2,
            offset: 200,
            size: 100,
        }];

        let summary = delta_summary(&local, &changed);
        assert_eq!(summary.file_size, 500);
        assert_eq!(summary.total_blocks, 5);
        assert_eq!(summary.changed_blocks, 1);
        assert_eq!(summary.changed_bytes, 100);
        assert!((summary.savings_ratio - 0.8).abs() < 0.001);
    }

    #[test]
    fn delta_summary_empty_file() {
        let local = compute_blockmap_from_bytes(b"", 100);
        let summary = delta_summary(&local, &[]);
        assert_eq!(summary.savings_ratio, 1.0);
    }

    #[test]
    fn blockmap_serde_round_trips() {
        let data = b"test data for serde";
        let map = compute_blockmap_from_bytes(data, 8);
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
        let map = compute_blockmap_from_bytes(data, DEFAULT_BLOCK_SIZE);
        assert_eq!(map.blocks.len(), 1);
        assert_eq!(map.blocks[0].size, 4);
        assert_eq!(map.blocks[0].offset, 0);
    }
}
