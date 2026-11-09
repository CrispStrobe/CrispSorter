//! Same-directory staged output shared by project saves and streamed exports.
//!
//! A renderer can write progressively into the private temporary file. The
//! destination is published only after rendering, flushing and file sync succeed.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Sync directory: {error}"))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn already_exists(path: &Path) -> String {
    format!(
        "Output already exists: {}. Choose another name or explicitly confirm replacement.",
        path.display()
    )
}

/// Write and publish one complete output. The closure must finish and flush any
/// buffer it creates before returning. A returned error or panic drops staging
/// without replacing the destination. The no-clobber commit also protects a
/// destination created by another writer after the initial existence check.
pub(super) fn atomic_write_with<T>(
    path: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut File) -> Result<T, String>,
) -> Result<T, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Choose a file in an existing output directory.".to_string())?;
    if !parent.is_dir() {
        return Err("The output directory does not exist.".into());
    }
    if !overwrite {
        match fs::symlink_metadata(path) {
            Ok(_) => return Err(already_exists(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Inspect output ({}): {error}", path.display())),
        }
    }
    let mut staging = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("Create output staging file: {error}"))?;
    let result = write(staging.as_file_mut())?;
    staging.flush().map_err(|error| format!("Flush output: {error}"))?;
    staging.as_file().sync_all().map_err(|error| format!("Sync output: {error}"))?;
    let persisted = if overwrite {
        staging.persist(path)
    } else {
        staging.persist_noclobber(path)
    };
    persisted.map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            already_exists(path)
        } else {
            format!("Commit output ({}): {}", path.display(), error.error)
        }
    })?;
    sync_directory(parent)?;
    Ok(result)
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    atomic_write_with(path, overwrite, |file| {
        file.write_all(bytes).map_err(|error| format!("Write output: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn byte_output_requires_explicit_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reviewed.txt");
        atomic_write(&path, b"first", false).unwrap();
        assert!(atomic_write(&path, b"second", false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        atomic_write(&path, b"second", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_stream_preserves_old_output_and_removes_new_staging() {
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("existing.pdf");
        let absent = root.path().join("new.pdf");
        fs::write(&existing, b"previous complete PDF").unwrap();
        for path in [&existing, &absent] {
            let error = atomic_write_with(path, true, |file| {
                file.write_all(b"%PDF-partial").unwrap();
                Err::<(), _>("native renderer or output device failed".to_string())
            }).unwrap_err();
            assert_eq!(error, "native renderer or output device failed");
            assert_eq!(fs::read(&existing).unwrap(), b"previous complete PDF");
            assert!(!absent.exists());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn existing_unconfirmed_output_does_not_start_rendering() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("existing.pdf");
        fs::write(&path, b"existing").unwrap();
        let invoked = Cell::new(false);
        let result = atomic_write_with(&path, false, |_| {
            invoked.set(true);
            Ok(())
        });
        assert!(result.unwrap_err().contains("already exists"));
        assert!(!invoked.get());
        assert_eq!(fs::read(&path).unwrap(), b"existing");
    }

    #[test]
    fn destination_created_during_render_is_not_overwritten_without_consent() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("racing.pdf");
        let result = atomic_write_with(&path, false, |file| {
            file.write_all(b"this renderer").unwrap();
            fs::write(&path, b"another writer").unwrap();
            Ok(())
        });
        assert!(result.unwrap_err().contains("already exists"));
        assert_eq!(fs::read(&path).unwrap(), b"another writer");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn completed_stream_is_published_only_after_the_writer_finishes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reviewed.pdf");
        fs::write(&path, b"previous").unwrap();
        let parts: [&[u8]; 3] = [b"%PDF-", b"complete content", b"\n%%EOF\n"];
        let length = atomic_write_with(&path, true, |file| {
            let mut count = 0;
            for part in parts {
                file.write_all(part).unwrap();
                count += part.len();
                assert_eq!(fs::read(&path).unwrap(), b"previous");
            }
            Ok(count)
        }).unwrap();
        let expected = parts.concat();
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(length, expected.len());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn renderer_unwind_cleans_staging_without_publishing_partial_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reviewed.pdf");
        fs::write(&path, b"previous").unwrap();
        let result = std::panic::catch_unwind(|| {
            let _: Result<(), String> = atomic_write_with(&path, true, |file| {
                file.write_all(b"partial").unwrap();
                panic!("simulated rendering panic");
            });
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
