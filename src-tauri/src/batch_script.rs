//! Generate sorting scripts with literal paths and exclusive destination creation.

pub(crate) struct Script {
    windows: bool,
    content: String,
}

impl Script {
    pub(crate) fn new(windows: bool) -> Self {
        Self {
            windows,
            content: if windows {
                format!("\u{feff}{WINDOWS_HEADER}")
            } else {
                UNIX_HEADER.to_string()
            },
        }
    }

    pub(crate) fn add(
        &mut self,
        source: &str,
        destination: &str,
        moving: bool,
    ) -> Result<(), String> {
        if source.contains('\0') || destination.contains('\0') {
            return Err("Paths containing NUL cannot be included in a script".into());
        }
        if self.windows {
            self.content.push_str(&format!(
                "Sort-File {} {} ${}\n",
                powershell_literal(source),
                powershell_literal(destination),
                if moving { "true" } else { "false" }
            ));
        } else {
            self.content.push_str(&format!(
                "sort_file {} {} {}\n",
                shell_literal(source),
                shell_literal(destination),
                if moving { "move" } else { "copy" }
            ));
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> String {
        self.content.push_str(if self.windows {
            "if ($errorsFound) { exit 1 }\nexit 0\n"
        } else {
            "exit \"$errors_found\"\n"
        });
        self.content
    }
}

fn shell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn powershell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

// Opening the input before the exclusive output prevents creating an empty
// destination when the source is missing. If the copy fails after the output
// is opened, leave the partial file for inspection; never remove a path that
// may have been replaced by another process.
const UNIX_HEADER: &str = r##"#!/bin/bash
errors_found=0
sort_file() {
    local source=$1 destination=$2 operation=$3 parent
    case "$source" in -*) source="./$source";; esac
    case "$destination" in -*) destination="./$destination";; esac
    case "$destination" in */*) parent=${destination%/*}; [ -n "$parent" ] || parent=/;; *) parent=.;; esac
    if ! mkdir -p "$parent"; then
        printf 'ERROR: cannot create destination directory for %s\n' "$destination" >&2
        errors_found=1
        return
    fi
    if (set -C; cat < "$source" > "$destination"); then
        if [ "$operation" = move ] && ! rm "$source"; then
            printf 'ERROR: copied to %s but could not remove source %s\n' "$destination" "$source" >&2
            errors_found=1
        fi
    else
        printf 'ERROR: could not copy %s to %s; source retained. Check for an existing destination or a partial copy.\n' "$source" "$destination" >&2
        errors_found=1
    fi
}
"##;

const WINDOWS_HEADER: &str = r##"$errorsFound = $false
function Sort-File([string]$source, [string]$destination, [bool]$moving) {
    $inputFile = $null
    $outputFile = $null
    try {
        $parent = [System.IO.Path]::GetDirectoryName($destination)
        if ($parent) { [void][System.IO.Directory]::CreateDirectory($parent) }
        $inputFile = [System.IO.File]::Open($source, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
        $outputFile = [System.IO.File]::Open($destination, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
        $inputFile.CopyTo($outputFile)
        $outputFile.Dispose(); $outputFile = $null
        $inputFile.Dispose(); $inputFile = $null
        if ($moving) { [System.IO.File]::Delete($source) }
    } catch {
        [Console]::Error.WriteLine("ERROR: could not complete sort from '{0}' to '{1}': {2}. Check for an existing destination or a partial copy.", $source, $destination, $_.Exception.Message)
        $script:errorsFound = $true
    } finally {
        if ($outputFile) { $outputFile.Dispose() }
        if ($inputFile) { $inputFile.Dispose() }
    }
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_paths_are_single_quoted_and_escaped() {
        let mut script = Script::new(true);
        script
            .add("C:\\odd'$env:TEST`name.pdf", "C:\\out%TEMP%!'.pdf", true)
            .unwrap();
        let text = script.finish();
        assert!(
            text.contains("Sort-File 'C:\\odd''$env:TEST`name.pdf' 'C:\\out%TEMP%!''.pdf' $true")
        );
        assert!(text.contains("[System.IO.FileMode]::CreateNew"));
        assert!(text.contains("exit 1"));
    }

    #[test]
    fn rejects_nul_paths() {
        assert!(Script::new(false).add("a\0b", "dest", true).is_err());
        assert!(Script::new(true).add("src", "a\0b", false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn shell_move_and_missing_source_are_reported_correctly() {
        use std::{
            fs,
            process::Command,
            sync::atomic::{AtomicUsize, Ordering},
        };
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crisp-script-move-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source.pdf");
        let dest = dir.join("dest.pdf");
        let missing_dest = dir.join("missing-dest.pdf");
        fs::write(&source, b"data").unwrap();
        let mut script = Script::new(false);
        script
            .add(source.to_str().unwrap(), dest.to_str().unwrap(), true)
            .unwrap();
        script
            .add(
                dir.join("missing.pdf").to_str().unwrap(),
                missing_dest.to_str().unwrap(),
                false,
            )
            .unwrap();
        let script_path = dir.join("run.sh");
        fs::write(&script_path, script.finish()).unwrap();
        let output = Command::new("bash").arg(&script_path).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("ERROR:"));
        assert!(!source.exists());
        assert_eq!(fs::read(dest).unwrap(), b"data");
        assert!(!missing_dest.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shell_script_handles_literal_paths_and_reports_collisions() {
        use std::{
            fs,
            process::Command,
            sync::atomic::{AtomicUsize, Ordering},
        };
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crisp-script-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source ' $HOME $(touch injected)\n.pdf");
        let destination = dir.join("sub ' $HOME $(touch injected)\n/dest.pdf");
        let collision = dir.join("existing.pdf");
        let other = dir.join("other.pdf");
        fs::write(&source, b"original").unwrap();
        fs::write(&collision, b"keep").unwrap();
        fs::write(&other, b"other").unwrap();
        let mut script = Script::new(false);
        script
            .add(
                source.to_str().unwrap(),
                destination.to_str().unwrap(),
                false,
            )
            .unwrap();
        script
            .add(other.to_str().unwrap(), collision.to_str().unwrap(), true)
            .unwrap();
        let script_path = dir.join("run.sh");
        fs::write(&script_path, script.finish()).unwrap();
        let output = Command::new("bash")
            .arg(&script_path)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("ERROR:"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert_eq!(fs::read(&collision).unwrap(), b"keep");
        assert_eq!(fs::read(&other).unwrap(), b"other");
        assert!(!dir.join("injected").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
