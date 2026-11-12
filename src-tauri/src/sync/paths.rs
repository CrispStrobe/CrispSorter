//! Validate provider names before mapping them into the configured local tree.
use anyhow::{ensure, Context, Result};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub(crate) fn validate_component(name: &str) -> Result<()> {
    validate_component_for_platform(name, cfg!(windows))
}

fn validate_component_for_platform(name: &str, windows: bool) -> Result<()> {
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']),
        "provider entry is not a single safe filename"
    );
    if windows {
        ensure!(
            !name
                .chars()
                .any(|c| c.is_ascii_control() || "<>:\"|?*".contains(c))
                && !name.ends_with(['.', ' ']),
            "provider filename cannot be represented safely on Windows"
        );
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        let numbered_device = stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
        ensure!(
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) && !numbered_device,
            "provider filename is a reserved Windows device"
        );
    }
    Ok(())
}

/// Resolve a pull target under an existing, explicitly configured directory.
/// Refuse symlink descendants, including the leaf. This is a filesystem
/// preflight, not descriptor-based protection against concurrent path swaps.
pub(crate) fn local_pull_destination(root: &Path, relative: &str) -> Result<PathBuf> {
    let parts: Vec<_> = relative.split('/').collect();
    for part in &parts {
        validate_component(part)?;
    }
    let root = root
        .canonicalize()
        .context("resolve configured sync root")?;
    ensure!(root.is_dir(), "configured sync root is not a directory");
    let mut parent = root.clone();
    for part in &parts[..parts.len() - 1] {
        let next = parent.join(part);
        if let Err(error) = std::fs::symlink_metadata(&next) {
            if error.kind() != ErrorKind::NotFound {
                return Err(error).context("inspect sync destination directory");
            }
            match std::fs::create_dir(&next) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("create sync destination directory"),
            }
        }
        let metadata =
            std::fs::symlink_metadata(&next).context("inspect sync destination directory")?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "sync destination directory is not a real directory"
        );
        parent = next
            .canonicalize()
            .context("resolve sync destination directory")?;
        ensure!(
            parent.starts_with(&root),
            "sync destination escaped its configured root"
        );
    }
    let destination = parent.join(parts.last().unwrap());
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "sync destination is not a regular file"
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect sync destination"),
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_components_reject_separators_traversal_and_nuls() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "nested/file",
            "nested\\file",
            "/absolute",
            "C:\\escape",
            "a\0b",
        ] {
            assert!(validate_component(name).is_err(), "accepted {name:?}");
        }
        for name in [
            "Überprüfung_日本_📄.pdf",
            "spaced name.txt",
            "literal%2Fname.txt",
            "a..b",
        ] {
            assert!(validate_component(name).is_ok());
        }
    }

    #[test]
    fn windows_device_ads_and_alias_names_fail_without_rewriting_valid_names() {
        for name in [
            "CON",
            "con.txt",
            "CON .txt",
            "NUL.log",
            "COM1.pdf",
            "LPT²",
            "CONOUT$",
            "file:stream",
            "C:relative",
            "trailing.",
            "trailing ",
            "wild*card",
            "a\nb",
        ] {
            assert!(
                validate_component_for_platform(name, true).is_err(),
                "accepted {name:?}"
            );
        }
        for name in ["file:stream", "trailing.", "wild*card", "CON"] {
            assert!(validate_component_for_platform(name, false).is_ok());
        }
        assert!(validate_component_for_platform("Überprüfung_日本_📄.pdf", true).is_ok());
    }

    #[test]
    fn nested_targets_stay_inside_root_and_existing_bytes_are_untouched() {
        let root = tempfile::tempdir().unwrap();
        let expected = root
            .path()
            .canonicalize()
            .unwrap()
            .join("nested/Überprüfung_日本.pdf");
        assert_eq!(
            local_pull_destination(root.path(), "nested/Überprüfung_日本.pdf").unwrap(),
            expected
        );
        std::fs::write(&expected, b"previous").unwrap();
        assert_eq!(
            local_pull_destination(root.path(), "nested/Überprüfung_日本.pdf").unwrap(),
            expected
        );
        assert_eq!(std::fs::read(expected).unwrap(), b"previous");
        assert!(local_pull_destination(root.path(), "nested").is_err());
        assert!(local_pull_destination(&root.path().join("missing-root"), "file").is_err());
        for relative in [
            "",
            "../escape",
            "/absolute",
            "a/../escape",
            "a//b",
            "a/",
            "a\\b",
        ] {
            assert!(local_pull_destination(root.path(), relative).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_parent_and_leaf_escapes_are_rejected_without_touching_outside_bytes() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let protected = outside.path().join("protected");
        std::fs::write(&protected, b"keep").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        symlink(&protected, root.path().join("leaf")).unwrap();
        symlink(outside.path().join("missing"), root.path().join("dangling")).unwrap();
        for relative in [
            "escape/protected",
            "escape/new/sub/file",
            "leaf",
            "dangling",
        ] {
            assert!(local_pull_destination(root.path(), relative).is_err());
        }
        assert_eq!(std::fs::read(protected).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn explicitly_configured_root_alias_is_resolved_once_as_the_authority() {
        let root = tempfile::tempdir().unwrap();
        let alias_parent = tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("configured-root");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        assert_eq!(
            local_pull_destination(&alias, "file").unwrap(),
            root.path().canonicalize().unwrap().join("file")
        );
    }
}
