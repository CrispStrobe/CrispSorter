//! Collision-safe file operations for batch sorting.
//!
//! `create_new` and `hard_link` claim the destination atomically. A plain
//! existence check followed by `copy` or `rename` would still overwrite a
//! file created by another process between those calls.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub(crate) struct Outcome {
    pub success: bool,
    pub error: Option<String>,
}

fn failure(message: String) -> Outcome {
    Outcome {
        success: false,
        error: Some(message),
    }
}

fn destination_error(path: &Path, error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::AlreadyExists {
        format!(
            "DESTINATION_EXISTS: {} — choose another destination or rename the existing file",
            path.display()
        )
    } else {
        format!("DESTINATION_WRITE_FAILED: {} — {}", path.display(), error)
    }
}

// Create the destination exclusively, including when it is a dangling symlink.
// On a failed write remove our incomplete file, leaving the source untouched.
fn copy_new(src: &Path, dest: &Path) -> io::Result<()> {
    let mut source = fs::File::open(src)?;
    let mut target = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let result = (|| {
        io::copy(&mut source, &mut target)?;
        target.set_permissions(source.metadata()?.permissions())?;
        Ok(())
    })();
    drop(target);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

/// Sort one file. If requested, the sidecar must be created successfully
/// before the file is moved; on failure the source is left in place.
pub(crate) fn execute(src: &Path, dest: &Path, text: Option<&str>, copy: bool) -> Outcome {
    let sidecar = text.map(|_| dest.with_extension("txt"));
    if sidecar.as_deref() == Some(dest) {
        return failure(format!(
            "SIDECAR_CONFLICT: {} is also the file destination",
            dest.display()
        ));
    }
    if let (Some(path), Some(text)) = (&sidecar, text) {
        let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(e) => return failure(format!("SIDECAR_{}", destination_error(path, &e))),
        };
        if let Err(e) = file.write_all(text.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(path);
            return failure(format!("SIDECAR_WRITE_FAILED: {} — {}", path.display(), e));
        }
    }

    // A hard link gives same-filesystem moves an atomic no-replace claim while
    // preserving the inode. If linking is unsupported or crosses filesystems,
    // fall back to an exclusive copy. The source is removed only afterwards.
    let operation = if copy {
        copy_new(src, dest)
    } else {
        match fs::hard_link(src, dest) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            Err(_) => copy_new(src, dest),
        }
    };
    if let Err(e) = operation {
        let mut message = destination_error(dest, &e);
        if let Some(path) = &sidecar {
            if let Err(cleanup) = fs::remove_file(path) {
                message.push_str(&format!(
                    "; could not remove created sidecar {}: {}",
                    path.display(),
                    cleanup
                ));
            }
        }
        return failure(message);
    }
    if !copy {
        if let Err(e) = fs::remove_file(src) {
            return Outcome {
                success: true,
                error: Some(format!(
                    "COPY_FALLBACK_NO_DELETE: source {} could not be removed: {}",
                    src.display(),
                    e
                )),
            };
        }
    }
    Outcome {
        success: true,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "crisp-batch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn existing_destination_preserves_source_and_destination_for_copy_and_move() {
        for copy in [true, false] {
            let dir = Dir::new();
            let src = dir.path("source.pdf");
            let dst = dir.path("dest.pdf");
            fs::write(&src, b"source").unwrap();
            fs::write(&dst, b"existing").unwrap();
            let result = execute(&src, &dst, Some("text"), copy);
            assert!(!result.success);
            assert!(result.error.unwrap().contains("DESTINATION_EXISTS"));
            assert_eq!(fs::read(&src).unwrap(), b"source");
            assert_eq!(fs::read(&dst).unwrap(), b"existing");
            assert!(!dir.path("dest.txt").exists());
        }
    }

    #[test]
    fn existing_sidecar_blocks_move_and_stays_intact() {
        let dir = Dir::new();
        let src = dir.path("source.pdf");
        let dst = dir.path("dest.pdf");
        fs::write(&src, b"source").unwrap();
        fs::write(dir.path("dest.txt"), b"existing text").unwrap();
        let result = execute(&src, &dst, Some("new text"), false);
        assert!(!result.success);
        assert!(result.error.unwrap().contains("SIDECAR_DESTINATION_EXISTS"));
        assert_eq!(fs::read(&src).unwrap(), b"source");
        assert_eq!(fs::read(dir.path("dest.txt")).unwrap(), b"existing text");
        assert!(!dst.exists());
    }

    #[test]
    fn copy_and_move_preserve_contents_and_create_sidecar() {
        for copy in [true, false] {
            let dir = Dir::new();
            let src = dir.path("source.pdf");
            let dst = dir.path("dest.pdf");
            fs::write(&src, b"source").unwrap();
            assert!(execute(&src, &dst, Some("text"), copy).success);
            assert_eq!(fs::read(&dst).unwrap(), b"source");
            assert_eq!(fs::read(dir.path("dest.txt")).unwrap(), b"text");
            assert_eq!(src.exists(), copy);
        }
    }

    #[test]
    fn concurrent_writers_do_not_replace_each_other() {
        let dir = Dir::new();
        let dst = dir.path("dest.pdf");
        let barrier = Arc::new(Barrier::new(2));
        let mut threads = Vec::new();
        for n in 0..2 {
            let src = dir.path(&format!("source{n}.pdf"));
            fs::write(&src, [n as u8]).unwrap();
            let dst = dst.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                (src.clone(), execute(&src, &dst, None, false))
            }));
        }
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|(_, r)| r.success).count(), 1);
        let winner = results.iter().find(|(_, r)| r.success).unwrap();
        let loser = results.iter().find(|(_, r)| !r.success).unwrap();
        assert!(!winner.0.exists());
        assert!(loser.0.exists());
        assert_ne!(fs::read(&dst).unwrap(), fs::read(&loser.0).unwrap());
    }

    #[test]
    fn failed_copy_removes_created_sidecar() {
        let dir = Dir::new();
        let dst = dir.path("dest.pdf");
        let result = execute(&dir.path("missing.pdf"), &dst, Some("text"), true);
        assert!(!result.success);
        assert!(!dst.exists());
        assert!(!dir.path("dest.txt").exists());
    }

    #[test]
    fn sidecar_cannot_be_the_destination() {
        let dir = Dir::new();
        let src = dir.path("source.pdf");
        let dst = dir.path("dest.txt");
        fs::write(&src, b"source").unwrap();
        let result = execute(&src, &dst, Some("text"), false);
        assert!(!result.success);
        assert!(result.error.unwrap().contains("SIDECAR_CONFLICT"));
        assert!(src.exists());
        assert!(!dst.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn move_across_filesystems_copies_and_removes_source() {
        use std::os::unix::fs::MetadataExt;
        let dir = Dir::new();
        let shared_memory = Path::new("/dev/shm");
        let Ok(metadata) = fs::metadata(shared_memory) else {
            return;
        };
        if metadata.dev() == fs::metadata(&dir.0).unwrap().dev() {
            return;
        }
        let dest_dir = shared_memory.join(dir.0.file_name().unwrap());
        if fs::create_dir(&dest_dir).is_err() {
            return;
        }
        let _cleanup = Dir(dest_dir);
        let src = dir.path("source.pdf");
        let dst = _cleanup.path("dest.pdf");
        fs::write(&src, b"cross-device").unwrap();
        assert!(execute(&src, &dst, None, false).success);
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"cross-device");
    }

    #[cfg(unix)]
    #[test]
    fn dangling_destination_symlink_is_a_conflict() {
        let dir = Dir::new();
        let src = dir.path("source.pdf");
        let dst = dir.path("dest.pdf");
        fs::write(&src, b"source").unwrap();
        std::os::unix::fs::symlink(dir.path("missing"), &dst).unwrap();
        let result = execute(&src, &dst, None, false);
        assert!(!result.success);
        assert!(result.error.unwrap().contains("DESTINATION_EXISTS"));
        assert!(src.exists());
        assert!(fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
    }
}
