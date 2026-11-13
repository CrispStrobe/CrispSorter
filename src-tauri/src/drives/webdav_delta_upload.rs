//! Freeze one changed block before making its request. In particular, local
//! short reads and read errors must still fail before that block's POST.
use anyhow::{ensure, Context, Result};
use std::io::{Read, Seek, SeekFrom};

use crate::sync::delta::{read_block_signature, Block};

/// Preserve replayable requests for the normal protocol block size.
pub(super) const REPLAYABLE_BODY_LIMIT: usize = crate::sync::delta::DEFAULT_BLOCK_SIZE;

pub(super) fn stage_block(
    reader: &mut impl Read,
    expected: &Block,
) -> Result<tempfile::NamedTempFile> {
    let mut staged =
        tempfile::NamedTempFile::new().context("creating delta upload staging file")?;
    let copied = std::io::copy(&mut reader.take(u64::from(expected.size)), &mut staged)
        .context("staging delta upload block")?;
    ensure!(
        copied == u64::from(expected.size),
        "local delta upload block ended early"
    );
    staged.seek(SeekFrom::Start(0))?;
    let actual = read_block_signature(&mut staged, expected.size as usize, expected.offset)?;
    ensure!(
        actual.as_ref() == Some(expected),
        "local delta upload block changed since hashing"
    );
    staged.seek(SeekFrom::Start(0))?;
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::delta::{compute_blockmap_from_bytes, HASH_BUFFER_SIZE};
    use std::io::{self, Cursor};

    #[test]
    fn staged_block_is_exact_and_owned() {
        let data = vec![b'a'; 2 * HASH_BUFFER_SIZE + 17];
        let expected = compute_blockmap_from_bytes(&data, data.len())
            .unwrap()
            .blocks
            .remove(0);
        let mut reader = Cursor::new([data.as_slice(), b"following block"].concat());
        let mut staged = stage_block(&mut reader, &expected).unwrap();
        assert_eq!(reader.position(), data.len() as u64);
        reader.get_mut().fill(b'b');
        let mut actual = Vec::new();
        staged.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, data);
        let path = staged.path().to_path_buf();
        drop(staged);
        assert!(!path.exists());
    }

    #[test]
    fn staging_rejects_truncation_mutation_and_read_errors() {
        let expected = compute_blockmap_from_bytes(&[b'a'; 1024], 1024)
            .unwrap()
            .blocks
            .remove(0);
        assert!(stage_block(&mut Cursor::new([b'a'; 1023]), &expected).is_err());
        assert!(stage_block(&mut Cursor::new([b'b'; 1024]), &expected).is_err());
        struct Fails(bool);
        impl Read for Fails {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("source read failed after partial block"));
                }
                self.0 = true;
                let n = 512.min(buf.len());
                buf[..n].fill(b'a');
                Ok(n)
            }
        }
        assert!(stage_block(&mut Fails(false), &expected).is_err());
    }
}
