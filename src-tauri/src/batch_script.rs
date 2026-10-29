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
