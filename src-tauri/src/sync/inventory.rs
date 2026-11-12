//! Read-only sync inventory. Provider names must remain single safe components.
pub(crate) fn inventory_remote(
    drive: &dyn crate::drives::CloudDrive,
    directory: &std::path::Path,
    relative_prefix: &str,
    includes: &[String],
    excludes: &[String],
    out: &mut Vec<crate::sync::pairs::SyncRemoteEntry>,
) -> anyhow::Result<()> {
    let entries = drive.list_dir(directory)?;
    let mut names = std::collections::HashSet::new();
    for entry in &entries {
        crate::sync::paths::validate_component(&entry.name)?;
        anyhow::ensure!(
            names.insert(entry.name.clone()),
            "provider returned ambiguous duplicate filenames"
        );
    }
    for entry in entries {
        let relative = if relative_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{relative_prefix}/{}", entry.name)
        };
        let child = directory.join(&entry.name);
        if entry.is_dir {
            inventory_remote(drive, &child, &relative, includes, excludes, out)?;
        } else if crate::sync::pairs::filter_matches(&relative, includes, excludes) {
            let mtime_unix = drive.stat(&child)?.mtime_unix;
            out.push(crate::sync::pairs::SyncRemoteEntry {
                relative_path: relative,
                size: entry.size.unwrap_or(0),
                mtime_unix,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drives::{CloudDrive, DirEntry, DriveType, FileStat, LocalDrive};
    use std::path::Path;

    struct NamesDrive(Vec<DirEntry>);
    impl CloudDrive for NamesDrive {
        fn label(&self) -> &str {
            "names-only"
        }
        fn drive_type(&self) -> DriveType {
            DriveType::Local
        }
        fn list_dir(&self, path: &Path) -> anyhow::Result<Vec<DirEntry>> {
            assert_eq!(
                path,
                Path::new("/remote"),
                "inventory recursed into an unvalidated name"
            );
            Ok(self.0.clone())
        }
        fn stat(&self, _: &Path) -> anyhow::Result<FileStat> {
            panic!("inventory inspected files before validating the listing")
        }
        fn read_file(&self, _: &Path) -> anyhow::Result<Vec<u8>> {
            panic!("inventory downloaded bytes")
        }
        fn write_file(&self, _: &Path, _: &[u8]) -> anyhow::Result<()> {
            panic!("inventory wrote bytes")
        }
        fn delete(&self, _: &Path) -> anyhow::Result<()> {
            panic!("inventory deleted bytes")
        }
    }
    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name.into(),
            is_dir,
            size: Some(1),
        }
    }

    #[test]
    fn remote_names_are_checked_before_stat_recursion_or_filtering() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "/absolute",
            "C:\\absolute",
            "nul\0name",
        ] {
            for is_dir in [false, true] {
                let drive = NamesDrive(vec![entry("valid.pdf", false), entry(name, is_dir)]);
                let mut output = Vec::new();
                assert!(inventory_remote(
                    &drive,
                    Path::new("/remote"),
                    "",
                    &["*.pdf".into()],
                    &[],
                    &mut output
                )
                .is_err());
                assert!(output.is_empty());
            }
        }
    }

    #[test]
    fn duplicate_provider_names_are_ambiguous_even_when_types_differ() {
        let drive = NamesDrive(vec![entry("same.pdf", false), entry("same.pdf", true)]);
        assert!(
            inventory_remote(&drive, Path::new("/remote"), "", &[], &[], &mut Vec::new()).is_err()
        );
    }

    #[test]
    fn real_nested_inventory_preserves_unicode_spaces_percent_names_and_filters() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("日本語 folder")).unwrap();
        std::fs::write(
            root.path().join("日本語 folder/Prüfung%2F.pdf"),
            b"document",
        )
        .unwrap();
        std::fs::write(root.path().join("日本語 folder/skip.txt"), b"skip").unwrap();
        let drive = LocalDrive::new("inventory", root.path());
        let mut output = Vec::new();
        inventory_remote(
            &drive,
            root.path(),
            "",
            &["**/*.pdf".into()],
            &[],
            &mut output,
        )
        .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].relative_path, "日本語 folder/Prüfung%2F.pdf");
        assert_eq!(output[0].size, 8);
    }
}
