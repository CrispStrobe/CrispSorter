//! Integrity checks for immutable OCR page snapshots.
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn directory(root: &Path) -> io::Result<PathBuf> {
    let assets = fs::canonicalize(root)?.join("assets");
    fs::create_dir_all(&assets)?;
    if fs::canonicalize(&assets)? != assets {
        return Err(invalid("The OCR page directory must not be a symbolic link."));
    }
    Ok(assets)
}

/// An existing hash-shaped filename is not proof that its bytes are intact.
/// Compare with the complete newly encoded snapshot without changing either
/// file or retaining the whole encoded image in memory.
pub(super) fn verify_existing(path: &Path, expected: &mut (impl Read + Seek)) -> io::Result<()> {
    let expected_length = expected.seek(SeekFrom::End(0))?;
    expected.seek(SeekFrom::Start(0))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || fs::canonicalize(path)? != path {
        return Err(invalid("The existing OCR page must be a regular file in the private page directory."));
    }
    if metadata.len() != expected_length {
        return Err(invalid("The existing OCR page has an unexpected length."));
    }
    let mut existing = File::open(path)?;
    let mut expected_bytes = [0u8; 64 * 1024];
    let mut existing_bytes = [0u8; 64 * 1024];
    let mut remaining = expected_length;
    while remaining > 0 {
        let count = remaining.min(expected_bytes.len() as u64) as usize;
        expected.read_exact(&mut expected_bytes[..count])?;
        existing.read_exact(&mut existing_bytes[..count])?;
        if expected_bytes[..count] != existing_bytes[..count] {
            return Err(invalid("The existing OCR page does not match its content hash."));
        }
        remaining -= count as u64;
    }
    if existing.read(&mut existing_bytes[..1])? != 0 || expected.read(&mut expected_bytes[..1])? != 0 {
        return Err(invalid("The OCR page changed during verification."));
    }
    Ok(())
}
