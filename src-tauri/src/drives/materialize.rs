//! Stage a bounded provider stream beside its destination before publication.
//!
//! The temporary file owns cleanup on every read, validation, sync, or rename
//! failure. Callers can check cancellation immediately before `publish` without
//! exposing a partial destination or replacing an existing valid file.

use anyhow::{Context, Result};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub(crate) struct StagedFile {
    temporary: NamedTempFile,
    destination: PathBuf,
    bytes: u64,
}

impl StagedFile {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn publish(self) -> Result<u64> {
        self.temporary
            .persist(&self.destination)
            .map_err(|error| error.error)
            .context("publish completed storage transfer")?;
        Ok(self.bytes)
    }
}

/// Prepare a complete file without changing an existing destination.
/// `expected_size` is a verified source size, not a reservation hint. Both a
/// truncated source and a source that grows during the transfer are rejected.
pub(crate) fn stage_file(
    destination: &Path,
    expected_size: Option<u64>,
    write: impl FnOnce(&mut dyn Write) -> Result<u64>,
) -> Result<StagedFile> {
    anyhow::ensure!(destination.file_name().is_some(), "destination requires a filename");
    let permissions = match std::fs::metadata(destination) {
        Ok(metadata) => {
            anyhow::ensure!(metadata.is_file(), "destination is not a regular file");
            anyhow::ensure!(!metadata.permissions().readonly(), "destination is read-only");
            Some(metadata.permissions())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("inspect storage destination"),
    };
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).context("create storage destination directory")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".crispsorter-transfer-")
        .tempfile_in(parent)
        .context("create private transfer staging file")?;
    let mut writer = MeteredWriter {
        inner: temporary.as_file_mut(),
        expected_size,
        bytes: 0,
    };
    let reported = write(&mut writer).context("stream storage source")?;
    let bytes = writer.bytes;
    anyhow::ensure!(reported == bytes, "provider byte count does not match transferred data");
    if let Some(expected) = expected_size {
        anyhow::ensure!(bytes == expected, "source size changed during storage transfer");
    }
    writer.flush().context("flush transfer staging file")?;
    temporary.as_file().sync_all().context("sync transfer staging file")?;
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)
            .context("preserve destination permissions")?;
    }
    Ok(StagedFile { temporary, destination: destination.to_owned(), bytes })
}

/// Enforce the declared size before publishing a local streaming upload.
pub(crate) fn write_from_reader(destination: &Path, reader: &mut dyn Read, size: u64) -> Result<()> {
    stage_file(destination, Some(size), |writer| {
        let copied = io::copy(&mut (&mut *reader).take(size), writer)?;
        anyhow::ensure!(copied == size, "reader ended before declared size");
        let mut extra = [0u8; 1];
        anyhow::ensure!(reader.read(&mut extra)? == 0, "reader has data beyond declared size");
        Ok(copied)
    })?.publish()?;
    Ok(())
}

struct MeteredWriter<'a> {
    inner: &'a mut std::fs::File,
    expected_size: Option<u64>,
    bytes: u64,
}

impl Write for MeteredWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.bytes.checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "transfer size overflow"))?;
        if self.expected_size.is_some_and(|expected| next > expected) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "source exceeded verified transfer size"));
        }
        let written = self.inner.write(bytes)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn assert_only_destination(directory: &Path, expected: &[u8]) {
        assert_eq!(std::fs::read(directory.join("result.bin")).unwrap(), expected);
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1, "staging file leaked");
    }

    #[test]
    fn keeps_existing_destination_until_complete_publication() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.bin");
        std::fs::write(&destination, b"previous").unwrap();
        let prepared = stage_file(&destination, Some(3), |writer| {
            writer.write_all(b"new")?;
            assert_eq!(std::fs::read(&destination)?, b"previous");
            Ok(3)
        }).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous");
        assert_eq!(prepared.bytes(), 3);
        assert_eq!(prepared.publish().unwrap(), 3);
        assert_only_destination(directory.path(), b"new");
    }

    #[test]
    fn partial_provider_error_preserves_destination_and_cleans_staging() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.bin");
        std::fs::write(&destination, b"previous").unwrap();
        let error = stage_file(&destination, None, |writer| {
            writer.write_all(b"partial")?;
            anyhow::bail!("source disconnected")
        }).err().unwrap();
        assert!(format!("{error:#}").contains("source disconnected"));
        assert_only_destination(directory.path(), b"previous");
    }

    #[test]
    fn truncated_overlong_and_false_provider_counts_never_publish() {
        for (expected, data, reported) in [(4, &b"abc"[..], 3), (2, &b"abc"[..], 3), (3, &b"abc"[..], 2)] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("result.bin");
            std::fs::write(&destination, b"previous").unwrap();
            assert!(stage_file(&destination, Some(expected), |writer| {
                writer.write_all(data)?;
                Ok(reported)
            }).is_err());
            assert_only_destination(directory.path(), b"previous");
        }
    }

    #[test]
    fn dropping_prepared_file_cancels_without_publication() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.bin");
        let prepared = stage_file(&destination, Some(4), |writer| {
            writer.write_all(b"done")?;
            Ok(4)
        }).unwrap();
        drop(prepared);
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn local_upload_rejects_short_and_overlong_readers_without_changing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.bin");
        std::fs::write(&destination, b"previous").unwrap();
        for size in [2, 4] {
            assert!(write_from_reader(&destination, &mut Cursor::new(b"abc"), size).is_err());
            assert_only_destination(directory.path(), b"previous");
        }
        write_from_reader(&destination, &mut Cursor::new(b"abc"), 3).unwrap();
        assert_only_destination(directory.path(), b"abc");
    }

    #[test]
    fn publication_failure_cleans_private_stage() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.bin");
        let prepared = stage_file(&destination, Some(3), |writer| {
            writer.write_all(b"abc")?;
            Ok(3)
        }).unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert!(prepared.publish().is_err());
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
