//! Guarded deletion of one duplicate while retaining a distinct local file.
//! The caller supplies the streaming SHA-256 implementation so these filesystem
//! invariants can be tested independently of the application and model runtime.
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 500 * 1024 * 1024;

fn regular_path(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "{} is not a regular file. Use the original file rather than a symbolic link.",
            path.display()
        ));
    }
    if metadata.len() > MAX_BYTES {
        return Err("File exceeds the 500 MiB duplicate verification limit.".into());
    }
    fs::canonicalize(path).map_err(|error| format!("Cannot resolve {}: {error}", path.display()))
}

fn same_snapshot(before: &Metadata, after: &Metadata) -> bool {
    if !after.is_file() || before.len() != after.len() {
        return false;
    }
    match (before.modified(), after.modified()) {
        (Ok(before), Ok(after)) if before == after => {}
        _ => return false,
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return false;
        }
    }
    true
}

fn still_matches(path: &Path, canonical: &Path, before: &Metadata) -> Result<(), String> {
    if regular_path(path)? != canonical {
        return Err("The file path changed during verification. Review the duplicates again.".into());
    }
    let after = fs::metadata(canonical).map_err(|error| error.to_string())?;
    if !same_snapshot(before, &after) {
        return Err("The file changed during verification. Review the duplicates again.".into());
    }
    Ok(())
}

pub(crate) fn delete_verified_copy(
    keep: &Path,
    target: &Path,
    expected_sha256: &str,
    mut hash_file: impl FnMut(&mut File) -> io::Result<String>,
) -> Result<(), String> {
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("A valid SHA-256 from duplicate review is required.".into());
    }
    let keep_canonical = regular_path(keep)?;
    let target_canonical = regular_path(target)?;
    // Covers relative paths, case-normalized paths, and symlinked parent
    // directories that lead to the same directory entry.
    if keep_canonical == target_canonical {
        return Err("These entries refer to the same file. No copy was deleted.".into());
    }
    let mut keep_file = File::open(&keep_canonical).map_err(|error| error.to_string())?;
    let mut target_file = File::open(&target_canonical).map_err(|error| error.to_string())?;
    let keep_metadata = keep_file.metadata().map_err(|error| error.to_string())?;
    let target_metadata = target_file.metadata().map_err(|error| error.to_string())?;
    let keep_hash = hash_file(&mut keep_file).map_err(|error| error.to_string())?;
    let target_hash = hash_file(&mut target_file).map_err(|error| error.to_string())?;
    if !keep_hash.eq_ignore_ascii_case(expected_sha256)
        || !target_hash.eq_ignore_ascii_case(expected_sha256)
    {
        return Err("File contents changed since duplicate review. No copy was deleted.".into());
    }
    still_matches(keep, &keep_canonical, &keep_metadata)?;
    still_matches(target, &target_canonical, &target_metadata)?;
    // Use the resolved target rather than following a parent alias again.
    // These checks are not an atomic lock against external filesystem writers.
    fs::remove_file(&target_canonical)
        .map_err(|error| format!("Could not delete {}: {error}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "crisp-duplicate-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, body: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, body).unwrap();
            path
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }
    fn hash(file: &mut File) -> io::Result<String> {
        let mut body = Vec::new();
        file.read_to_end(&mut body)?;
        Ok(if body == b"same" { "a" } else { "b" }.repeat(64))
    }

    #[test]
    fn deletes_only_the_verified_copy() {
        let dir = Dir::new();
        let keep = dir.file("keep.pdf", b"same");
        let copy = dir.file("copy.pdf", b"same");
        delete_verified_copy(&keep, &copy, &"a".repeat(64), hash).unwrap();
        assert_eq!(fs::read(keep).unwrap(), b"same");
        assert!(!copy.exists());
    }

    #[test]
    fn rejects_different_contents_and_invalid_expected_hashes() {
        let dir = Dir::new();
        let keep = dir.file("keep.pdf", b"same");
        let copy = dir.file("copy.pdf", b"different");
        assert!(delete_verified_copy(&keep, &copy, &"a".repeat(64), hash).is_err());
        assert!(delete_verified_copy(&keep, &copy, "", hash).is_err());
        assert!(keep.exists() && copy.exists());
    }

    #[test]
    fn rejects_aliases_for_the_same_file() {
        let dir = Dir::new();
        let keep = dir.file("keep.pdf", b"same");
        fs::create_dir(dir.0.join("sub")).unwrap();
        let alias = dir.0.join("sub/../keep.pdf");
        assert!(delete_verified_copy(&keep, &alias, &"a".repeat(64), hash).is_err());
        assert!(keep.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_keeper_symlink_and_a_parent_directory_alias() {
        let dir = Dir::new();
        let copy = dir.file("copy.pdf", b"same");
        let keep = dir.0.join("keep.pdf");
        std::os::unix::fs::symlink(&copy, &keep).unwrap();
        assert!(delete_verified_copy(&keep, &copy, &"a".repeat(64), hash).is_err());
        let alias = dir.0.join("alias");
        std::os::unix::fs::symlink(&dir.0, &alias).unwrap();
        assert!(delete_verified_copy(&alias.join("copy.pdf"), &copy, &"a".repeat(64), hash).is_err());
        assert_eq!(fs::read(keep).unwrap(), b"same");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_replacement_during_hashing() {
        let dir = Dir::new();
        let keep = dir.file("keep.pdf", b"same");
        let copy = dir.file("copy.pdf", b"same");
        let replacement = dir.file("replacement.pdf", b"same");
        let mut reads = 0;
        let result = delete_verified_copy(&keep, &copy, &"a".repeat(64), |file| {
            let value = hash(file)?;
            reads += 1;
            if reads == 2 { fs::rename(&replacement, &copy)?; }
            Ok(value)
        });
        assert!(result.is_err());
        assert!(keep.exists() && copy.exists());
    }
}
