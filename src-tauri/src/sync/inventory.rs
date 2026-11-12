//! Read-only sync inventory. Provider names must remain single safe components.
const MAX_INVENTORY_DEPTH: usize = 64;
const MAX_INVENTORY_DIRECTORIES: usize = 10_000;
const MAX_INVENTORY_ENTRIES: usize = 100_000;

#[derive(Clone, Copy)]
struct InventoryLimits {
    depth: usize,
    directories: usize,
    entries: usize,
}

pub(crate) fn inventory_remote(
    drive: &dyn crate::drives::CloudDrive,
    directory: &std::path::Path,
    relative_prefix: &str,
    includes: &[String],
    excludes: &[String],
    out: &mut Vec<crate::sync::pairs::SyncRemoteEntry>,
) -> anyhow::Result<()> {
    inventory_with_limits(
        drive,
        directory,
        relative_prefix,
        includes,
        excludes,
        out,
        InventoryLimits {
            depth: MAX_INVENTORY_DEPTH,
            directories: MAX_INVENTORY_DIRECTORIES,
            entries: MAX_INVENTORY_ENTRIES,
        },
    )
}

/// Keep recursive provider structures off the call stack. The pending tasks are
/// pushed in reverse listing order to preserve depth-first output/stat order.
/// Publish the new rows only after every task succeeds, never a partial plan.
fn inventory_with_limits(
    drive: &dyn crate::drives::CloudDrive,
    directory: &std::path::Path,
    relative_prefix: &str,
    includes: &[String],
    excludes: &[String],
    out: &mut Vec<crate::sync::pairs::SyncRemoteEntry>,
    limits: InventoryLimits,
) -> anyhow::Result<()> {
    use std::path::PathBuf;
    enum Task {
        Directory {
            path: PathBuf,
            relative: String,
            depth: usize,
        },
        File {
            path: PathBuf,
            relative: String,
            size: u64,
        },
    }
    anyhow::ensure!(
        limits.directories > 0,
        "sync inventory directory budget exceeded"
    );
    let mut pending = vec![Task::Directory {
        path: directory.to_owned(),
        relative: relative_prefix.into(),
        depth: 0,
    }];
    let mut scheduled_directories = 1usize;
    let mut scanned_entries = 0usize;
    let mut planned = Vec::new();
    while let Some(task) = pending.pop() {
        match task {
            Task::File {
                path,
                relative,
                size,
            } => {
                let mtime_unix = drive.stat(&path)?.mtime_unix;
                planned.push(crate::sync::pairs::SyncRemoteEntry {
                    relative_path: relative,
                    size,
                    mtime_unix,
                });
            }
            Task::Directory {
                path,
                relative,
                depth,
            } => {
                let entries = drive.list_dir(&path)?;
                anyhow::ensure!(
                    entries.len() <= limits.entries.saturating_sub(scanned_entries),
                    "sync inventory entry budget exceeded"
                );
                scanned_entries += entries.len();
                let children = entries.iter().filter(|entry| entry.is_dir).count();
                anyhow::ensure!(
                    children <= limits.directories.saturating_sub(scheduled_directories),
                    "sync inventory directory budget exceeded"
                );
                anyhow::ensure!(
                    children == 0 || depth < limits.depth,
                    "sync inventory depth budget exceeded"
                );
                scheduled_directories += children;
                let mut names = std::collections::HashSet::new();
                for entry in &entries {
                    crate::sync::paths::validate_component(&entry.name)?;
                    anyhow::ensure!(
                        names.insert(entry.name.clone()),
                        "provider returned ambiguous duplicate filenames"
                    );
                }
                for entry in entries.into_iter().rev() {
                    let relative = if relative.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{relative}/{}", entry.name)
                    };
                    let child = path.join(&entry.name);
                    if entry.is_dir {
                        pending.push(Task::Directory {
                            path: child,
                            relative,
                            depth: depth + 1,
                        });
                    } else if crate::sync::pairs::filter_matches(&relative, includes, excludes) {
                        pending.push(Task::File {
                            path: child,
                            relative,
                            size: entry.size.unwrap_or(0),
                        });
                    }
                }
            }
        }
    }
    out.extend(planned);
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

    struct ScriptedDrive {
        listings: std::collections::BTreeMap<&'static str, Vec<DirEntry>>,
        cycle: bool,
        fail_list: Option<&'static str>,
        fail_stat: Option<&'static str>,
        calls: std::sync::Mutex<Vec<String>>,
    }
    impl ScriptedDrive {
        fn tree(listings: &[(&'static str, Vec<DirEntry>)]) -> Self {
            Self {
                listings: listings.iter().cloned().collect(),
                cycle: false,
                fail_list: None,
                fail_stat: None,
                calls: Default::default(),
            }
        }
        fn relative(path: &Path) -> String {
            path.strip_prefix("/remote")
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        }
    }
    impl CloudDrive for ScriptedDrive {
        fn label(&self) -> &str {
            "scripted"
        }
        fn drive_type(&self) -> DriveType {
            DriveType::Local
        }
        fn list_dir(&self, path: &Path) -> anyhow::Result<Vec<DirEntry>> {
            let relative = Self::relative(path);
            self.calls.lock().unwrap().push(format!("list:{relative}"));
            anyhow::ensure!(self.fail_list != Some(relative.as_str()), "listing failed");
            if self.cycle {
                return Ok(vec![entry("again", true)]);
            }
            Ok(self
                .listings
                .get(relative.as_str())
                .expect("unexpected directory")
                .clone())
        }
        fn stat(&self, path: &Path) -> anyhow::Result<FileStat> {
            let relative = Self::relative(path);
            self.calls.lock().unwrap().push(format!("stat:{relative}"));
            anyhow::ensure!(self.fail_stat != Some(relative.as_str()), "stat failed");
            Ok(FileStat {
                size: 1,
                is_dir: false,
                mtime_unix: Some(10),
            })
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
    fn marker() -> crate::sync::pairs::SyncRemoteEntry {
        crate::sync::pairs::SyncRemoteEntry {
            relative_path: "existing-output".into(),
            size: 0,
            mtime_unix: None,
        }
    }
    fn limits() -> InventoryLimits {
        InventoryLimits {
            depth: 10,
            directories: 20,
            entries: 30,
        }
    }

    #[test]
    fn cyclic_provider_is_stopped_by_the_production_depth_budget() {
        let mut drive = ScriptedDrive::tree(&[]);
        drive.cycle = true;
        let mut output = vec![marker()];
        let error =
            inventory_remote(&drive, Path::new("/remote"), "", &[], &[], &mut output).unwrap_err();
        assert!(error.to_string().contains("depth budget"));
        assert_eq!(drive.calls.lock().unwrap().len(), MAX_INVENTORY_DEPTH + 1);
        assert_eq!(output, [marker()]);
    }

    #[test]
    fn directory_budget_stops_before_scheduling_excess_provider_requests() {
        let drive = ScriptedDrive::tree(&[("", vec![entry("a", true), entry("b", true)])]);
        let mut output = vec![marker()];
        let error = inventory_with_limits(
            &drive,
            Path::new("/remote"),
            "",
            &[],
            &[],
            &mut output,
            InventoryLimits {
                directories: 2,
                ..limits()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("directory budget"));
        assert_eq!(*drive.calls.lock().unwrap(), ["list:"]);
        assert_eq!(output, [marker()]);
    }

    #[test]
    fn excluded_entries_count_toward_the_budget_and_no_partial_plan_is_published() {
        let drive = ScriptedDrive::tree(&[
            ("", vec![entry("first.pdf", false), entry("nested", true)]),
            ("nested", vec![entry("excluded.txt", false)]),
        ]);
        let mut output = vec![marker()];
        let error = inventory_with_limits(
            &drive,
            Path::new("/remote"),
            "",
            &["**/*.pdf".into()],
            &[],
            &mut output,
            InventoryLimits {
                entries: 2,
                ..limits()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("entry budget"));
        assert!(drive
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "stat:first.pdf"));
        assert_eq!(output, [marker()]);
    }

    #[test]
    fn listing_and_stat_errors_preserve_the_callers_existing_output() {
        for fail_list in [true, false] {
            let mut drive = ScriptedDrive::tree(&[
                ("", vec![entry("first", false), entry("nested", true)]),
                ("nested", vec![entry("second", false)]),
            ]);
            if fail_list {
                drive.fail_list = Some("nested");
            } else {
                drive.fail_stat = Some("nested/second");
            }
            let mut output = vec![marker()];
            assert!(
                inventory_remote(&drive, Path::new("/remote"), "", &[], &[], &mut output).is_err()
            );
            assert!(drive
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call == "stat:first"));
            assert_eq!(output, [marker()]);
        }
    }

    #[test]
    fn iterative_tasks_preserve_depth_first_listing_filter_and_stat_order() {
        let drive = ScriptedDrive::tree(&[
            (
                "",
                vec![
                    entry("a.txt", false),
                    entry("d", true),
                    entry("z.txt", false),
                ],
            ),
            (
                "d",
                vec![
                    entry("b.txt", false),
                    entry("skip.bin", false),
                    entry("e", true),
                    entry("c.txt", false),
                ],
            ),
            ("d/e", vec![entry("x.txt", false)]),
        ]);
        let mut output = vec![marker()];
        // Eight scanned entries (including directories and excluded files).
        inventory_with_limits(
            &drive,
            Path::new("/remote"),
            "",
            &["**/*.txt".into()],
            &["**/c.txt".into()],
            &mut output,
            InventoryLimits {
                entries: 8,
                directories: 3,
                depth: 2,
            },
        )
        .unwrap();
        assert_eq!(
            output
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["existing-output", "a.txt", "d/b.txt", "d/e/x.txt", "z.txt"]
        );
        assert_eq!(
            *drive.calls.lock().unwrap(),
            [
                "list:",
                "stat:a.txt",
                "list:d",
                "stat:d/b.txt",
                "list:d/e",
                "stat:d/e/x.txt",
                "stat:z.txt"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_followed_directory_symlink_cycle_fails_without_recursion_or_a_partial_plan() {
        // LocalDrive's DirEntry metadata does not follow symlinks. Model a
        // provider that exposes followed directory aliases, using real paths.
        struct FollowingProvider(LocalDrive);
        impl CloudDrive for FollowingProvider {
            fn label(&self) -> &str {
                "following-fixture"
            }
            fn drive_type(&self) -> DriveType {
                DriveType::Local
            }
            fn list_dir(&self, path: &Path) -> anyhow::Result<Vec<DirEntry>> {
                let mut entries = self.0.list_dir(path)?;
                for entry in &mut entries {
                    entry.is_dir = std::fs::metadata(path.join(&entry.name))?.is_dir();
                }
                Ok(entries)
            }
            fn stat(&self, path: &Path) -> anyhow::Result<FileStat> {
                self.0.stat(path)
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
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("again")).unwrap();
        let drive = FollowingProvider(LocalDrive::new("cycle", root.path()));
        let mut output = vec![marker()];
        let error = inventory_with_limits(
            &drive,
            root.path(),
            "",
            &[],
            &[],
            &mut output,
            InventoryLimits {
                depth: 3,
                ..limits()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("depth budget"));
        assert_eq!(output, [marker()]);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
