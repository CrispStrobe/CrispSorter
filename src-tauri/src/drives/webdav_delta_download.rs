//! Stage and verify all WebDAV delta bytes before writing the open destination.
//! This is not atomic: local commit failures and concurrent writers can still
//! leave partial changes. No pathname replacement or metadata copying occurs.
use crate::sync::delta::{Block, Blockmap, MIN_BLOCK_SIZE};
use anyhow::{anyhow, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const BUFFER: usize = 64 * 1024;
const RANGE_CHUNK: u64 = 4 * 1024 * 1024;

fn validate_map(map: &Blockmap) -> Result<()> {
    ensure!(
        map.block_size as usize >= MIN_BLOCK_SIZE,
        "invalid delta block size"
    );
    let mut offset = 0u64;
    for block in &map.blocks {
        ensure!(
            block.offset == offset && offset < map.file_size,
            "invalid delta block offset"
        );
        let expected = u64::from(map.block_size).min(map.file_size - offset);
        ensure!(
            u64::from(block.size) == expected && block.size > 0,
            "invalid delta block length"
        );
        offset = offset
            .checked_add(u64::from(block.size))
            .ok_or_else(|| anyhow!("delta offset overflow"))?;
    }
    ensure!(offset == map.file_size, "incomplete delta block map");
    Ok(())
}

fn hashes(reader: &mut impl Read, limit: u64) -> Result<(u64, [u8; 32], u32)> {
    hashes_observe(reader, limit, |_| {})
}

fn hashes_observe(
    reader: &mut impl Read,
    limit: u64,
    mut observe: impl FnMut(&[u8]),
) -> Result<(u64, [u8; 32], u32)> {
    let mut reader = reader.take(limit);
    let mut buffer = [0u8; BUFFER];
    let (mut count, mut a, mut b) = (0u64, 1u32, 0u32);
    let mut hash = Sha256::new();
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        count += size as u64;
        hash.update(&buffer[..size]);
        observe(&buffer[..size]);
        // Adler-32's standard 5552-byte bound prevents u32 overflow while
        // avoiding two divisions for every input byte.
        for chunk in buffer[..size].chunks(5552) {
            for byte in chunk {
                a += u32::from(*byte);
                b += a;
            }
            a %= 65521;
            b %= 65521;
        }
    }
    Ok((count, hash.finalize().into(), (b << 16) | a))
}

fn matches(file: &mut File, block: &Block, local_length: u64, offset: u64) -> Result<bool> {
    file.seek(SeekFrom::Start(offset))?;
    let (length, strong, weak) = hashes(file, local_length)?;
    Ok(length == u64::from(block.size) && strong == block.strong_hash && weak == block.weak_hash)
}

fn regular(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::metadata(path)?;
    ensure!(
        metadata.is_file(),
        "delta destination must be a regular file"
    );
    ensure!(
        !metadata.permissions().readonly(),
        "delta destination is readonly"
    );
    Ok(metadata)
}

pub(super) fn download(
    path: &Path,
    remote: &Blockmap,
    mut fetch: impl FnMut(u64, u64) -> Result<Vec<u8>>,
) -> Result<Option<(usize, u64)>> {
    validate_map(remote)?;
    match fs::metadata(path) {
        Ok(_) => {
            regular(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    // Preserve the previous read+write access requirement. A writable parent
    // alone must not authorize replacing a file whose ACL forbids writes.
    let mut source = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .context("opening delta source for read and write")?;
    // Base all content/metadata comparisons on the actual opened destination,
    // including when its pathname changed during open.
    let before = source.metadata()?;
    ensure!(
        before.is_file() && !before.permissions().readonly(),
        "delta destination must be a writable regular file"
    );
    let mut stage = tempfile::tempfile().context("creating private delta stage")?;
    let mut original = Sha256::new();
    let mut buffer = [0u8; BUFFER];
    // Capture block equality and the whole-file digest from the same reads.
    // Comparing the live source later could skip a transient edit that is then
    // restored before the whole-file recheck (an ABA change).
    let mut copied = 0u64;
    let mut equal = Vec::with_capacity(remote.blocks.len());
    for block in &remote.blocks {
        let local_length = before
            .len()
            .saturating_sub(block.offset)
            .min(u64::from(remote.block_size));
        let (length, strong, weak) =
            hashes_observe(&mut source, local_length, |bytes| original.update(bytes))?;
        ensure!(
            length == local_length,
            "delta source became shorter during baseline scan"
        );
        copied += length;
        equal.push(
            length == u64::from(block.size)
                && strong == block.strong_hash
                && weak == block.weak_hash,
        );
    }
    // Include any original tail beyond the remote map, including truncation to empty.
    let (tail, _, _) = hashes_observe(&mut source, before.len().saturating_sub(copied), |bytes| {
        original.update(bytes)
    })?;
    copied += tail;
    ensure!(
        copied == before.len()
            && source.read(&mut buffer[..1])? == 0
            && source.metadata()?.modified()? == before.modified()?,
        "delta source changed during staging"
    );
    let original: [u8; 32] = original.finalize().into();
    let (mut changed, mut transferred) = (Vec::new(), 0u64);
    for (block, equal) in remote.blocks.iter().zip(equal) {
        if equal {
            continue;
        }
        let stage_offset = transferred;
        changed.push((block, stage_offset));
        let block_end = block
            .offset
            .checked_add(u64::from(block.size))
            .ok_or_else(|| anyhow!("delta offset overflow"))?;
        let mut offset = block.offset;
        while offset < block_end {
            let length = (block_end - offset).min(RANGE_CHUNK);
            let data = fetch(offset, offset + length - 1)?;
            ensure!(
                data.len() as u64 == length,
                "delta range response size mismatch"
            );
            let write_offset = stage_offset
                .checked_add(offset - block.offset)
                .ok_or_else(|| anyhow!("delta staging offset overflow"))?;
            stage.seek(SeekFrom::Start(write_offset))?;
            stage.write_all(&data)?;
            transferred = transferred
                .checked_add(length)
                .ok_or_else(|| anyhow!("delta transferred size overflow"))?;
            offset += length;
        }
        ensure!(
            matches(&mut stage, block, u64::from(block.size), stage_offset)?,
            "delta block checksum mismatch"
        );
    }
    stage.flush()?;
    stage.sync_all()?;
    // Check the original open inode, never reopen or replace the pathname.
    // This detects edits already visible here, not a lock against later writers.
    let now = source.metadata()?;
    ensure!(
        now.permissions() == before.permissions(),
        "delta destination permissions changed"
    );
    source.seek(SeekFrom::Start(0))?;
    let (length, strong, _) = hashes(&mut source, copied.saturating_add(1))?;
    ensure!(
        length == copied && strong == original,
        "delta destination changed before commit"
    );
    if changed.is_empty() && copied == remote.file_size {
        return Ok(Some((0, 0)));
    }
    // All HTTP requests, size checks and checksums succeeded before the first
    // destination write. Keep its inode, ACLs and hard-link relationships.
    // Local I/O errors or concurrent writers during this phase can still leave
    // partial changes; there is deliberately no atomic-publication claim.
    let commit = (|| -> Result<()> {
        for (block, stage_offset) in &changed {
            stage.seek(SeekFrom::Start(*stage_offset))?;
            source.seek(SeekFrom::Start(block.offset))?;
            let mut remaining = u64::from(block.size);
            while remaining > 0 {
                let size = remaining.min(BUFFER as u64) as usize;
                stage.read_exact(&mut buffer[..size])?;
                source.write_all(&buffer[..size])?;
                remaining -= size as u64;
            }
        }
        source.set_len(remote.file_size)?;
        source.sync_all()?;
        Ok(())
    })();
    commit.context("committing delta in place; destination may contain partial changes")?;
    Ok(Some((changed.len(), transferred)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::delta::compute_blockmap_from_bytes;

    fn map(bytes: &[u8]) -> Blockmap {
        compute_blockmap_from_bytes(bytes, 1024).unwrap()
    }
    fn fixture(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        fs::write(&path, bytes).unwrap();
        (dir, path)
    }
    fn assert_only_target(dir: &Path) {
        assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
    }

    #[test]
    fn later_range_failure_or_wrong_bytes_preserves_original_and_cleans_stage() {
        for failure in ["http", "short", "checksum"] {
            let original = vec![b'a'; 3072];
            let remote = vec![b'b'; 3072];
            let (dir, path) = fixture(&original);
            let mut calls = 0;
            let result = download(&path, &map(&remote), |start, end| {
                calls += 1;
                if calls == 2 {
                    return match failure {
                        "http" => Err(anyhow!("HTTP 500")),
                        "short" => Ok(vec![]),
                        _ => Ok(vec![b'c'; (end - start + 1) as usize]),
                    };
                }
                Ok(remote[start as usize..=end as usize].to_vec())
            });
            assert!(result.is_err());
            assert_eq!(calls, 2);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_only_target(dir.path());
        }
    }

    #[test]
    fn successful_changes_growth_shrink_empty_and_unchanged_are_exact() {
        for (old, new) in [
            (
                vec![b'a'; 3072],
                [vec![b'a'; 1024], vec![b'b'; 2048]].concat(),
            ),
            (
                vec![b'a'; 3072],
                [vec![b'b'; 1024], vec![b'a'; 1024], vec![b'c'; 1024]].concat(),
            ),
            (vec![b'a'; 1024], vec![b'b'; 2500]),
            (vec![b'a'; 3000], vec![b'a'; 1500]),
            (vec![b'a'; 1024], vec![]),
            (vec![b'a'; 1024], vec![b'a'; 1024]),
        ] {
            let (dir, path) = fixture(&old);
            let mut bytes = 0;
            let result = download(&path, &map(&new), |start, end| {
                bytes += end - start + 1;
                Ok(new[start as usize..=end as usize].to_vec())
            })
            .unwrap()
            .unwrap();
            assert_eq!(result.1, bytes);
            assert_eq!(fs::read(&path).unwrap(), new);
            assert_only_target(dir.path());
            if old == new {
                assert_eq!(result, (0, 0));
            }
        }
    }

    #[test]
    fn observed_concurrent_edits_are_not_overwritten() {
        let (dir, path) = fixture(&vec![b'a'; 2048]);
        let new = vec![b'b'; 2048];
        let result = download(&path, &map(&new), |start, end| {
            fs::write(&path, b"user edit").unwrap();
            Ok(new[start as usize..=end as usize].to_vec())
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("changed before commit"));
        assert_eq!(fs::read(&path).unwrap(), b"user edit");
        assert_only_target(dir.path());
    }

    #[test]
    fn readonly_directory_and_missing_targets_are_safe() {
        let (dir, path) = fixture(b"original");
        let remote = map(b"remote");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        assert!(download(&path, &remote, |_, _| panic!("must not fetch")).is_err());
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(download(dir.path(), &remote, |_, _| panic!("must not fetch")).is_err());
        assert!(
            download(&dir.path().join("missing"), &remote, |_, _| panic!(
                "must not fetch"
            ))
            .unwrap()
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn in_place_commit_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = fixture(b"old");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        download(&path, &map(b"new"), |_, _| Ok(b"new".to_vec())).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn malformed_maps_fail_before_fetch_or_staging() {
        let (dir, path) = fixture(b"original");
        let valid = map(&vec![b'x'; 2048]);
        for variant in 0..6 {
            let mut invalid = valid.clone();
            match variant {
                0 => invalid.blocks[0].offset = u64::MAX,
                1 => invalid.blocks[0].size = 0,
                2 => invalid.blocks[0].size = 1023,
                3 => invalid.blocks[1].offset = 0,
                4 => invalid.file_size = u64::MAX,
                _ => invalid.block_size = 1,
            }
            assert!(download(&path, &invalid, |_, _| panic!("must not fetch")).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"original");
            assert_only_target(dir.path());
        }
    }

    #[test]
    fn huge_peer_block_size_never_controls_memory_or_range_allocation() {
        let (dir, path) = fixture(b"");
        let remote = Blockmap {
            file_size: u64::from(u32::MAX),
            block_size: u32::MAX,
            blocks: vec![Block {
                offset: 0,
                size: u32::MAX,
                strong_hash: [0; 32],
                weak_hash: 0,
            }],
        };
        let mut calls = 0;
        assert!(download(&path, &remote, |start, end| {
            calls += 1;
            assert_eq!(start, 0);
            assert_eq!(end - start + 1, RANGE_CHUNK);
            Err(anyhow!("stop before transfer"))
        })
        .is_err());
        assert_eq!(calls, 1);
        assert!(fs::read(&path).unwrap().is_empty());
        assert_only_target(dir.path());
    }
    #[cfg(unix)]
    #[test]
    fn commit_keeps_inode_owner_permissions_and_hardlink_relationship() {
        use std::os::unix::fs::MetadataExt;
        let (dir, path) = fixture(b"old");
        let link = dir.path().join("hardlink");
        fs::hard_link(&path, &link).unwrap();
        let before = fs::metadata(&path).unwrap();
        download(&path, &map(b"new"), |_, _| Ok(b"new".to_vec())).unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.uid(),
                before.gid(),
                before.mode(),
                before.nlink()
            ),
            (
                after.dev(),
                after.ino(),
                after.uid(),
                after.gid(),
                after.mode(),
                after.nlink()
            )
        );
        assert_eq!(fs::read(link).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn pathname_replacement_during_fetch_does_not_redirect_commit() {
        let (dir, path) = fixture(b"old");
        let moved = dir.path().join("moved");
        download(&path, &map(b"new"), |_, _| {
            fs::rename(&path, &moved).unwrap();
            fs::write(&path, b"replacement").unwrap();
            Ok(b"new".to_vec())
        })
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), b"replacement");
        assert_eq!(fs::read(moved).unwrap(), b"new");
    }
    #[test]
    fn streaming_hashes_match_existing_blockmap_across_buffer_boundaries() {
        let bytes: Vec<u8> = (0..200_000).map(|n| (n % 251) as u8).collect();
        let (length, strong, weak) = hashes(&mut &bytes[..], bytes.len() as u64).unwrap();
        assert_eq!(length, bytes.len() as u64);
        assert_eq!(weak, crate::sync::delta::adler32(&bytes));
        assert_eq!(strong, <[u8; 32]>::from(Sha256::digest(&bytes)));
    }
    #[test]
    fn transient_source_edit_cannot_make_a_changed_block_skip_staging() {
        let (_dir, path) = fixture(&vec![b'a'; 3072]);
        let remote = vec![b'b'; 3072];
        let mut calls = 0;
        download(&path, &map(&remote), |start, end| {
            calls += 1;
            let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(1024)).unwrap();
            file.write_all(&vec![if start == 0 { b'b' } else { b'a' }; 1024])
                .unwrap();
            Ok(remote[start as usize..=end as usize].to_vec())
        })
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(fs::read(path).unwrap(), remote);
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_to_regular_file_keeps_previous_target_semantics() {
        let (dir, path) = fixture(b"old");
        let link = dir.path().join("alias");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        download(&link, &map(b"new"), |_, _| Ok(b"new".to_vec())).unwrap();
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(path).unwrap(), b"new");
    }
}
