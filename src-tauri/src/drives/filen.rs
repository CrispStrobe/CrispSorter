//! Filen cloud-drive integration via the Python `filen-cli` (`filen-python/cli.py`).
//!
//! Talks to the patched cli.py that supports `--json` on `whoami`, `ls`,
//! `resolve`, `trash`.  All structured output is plain JSON — no
//! emoji-text scraping.  Downloads/uploads stage through a tempfile because
//! the Python CLI works on disk, not stdio.
//!
//! Requirements on the host:
//!   * `python` (Miniconda or system) on PATH with the deps `filen-python`
//!     needs.  Override via `FILEN_CLI_PYTHON` env var if your interpreter
//!     is named `python3` or lives elsewhere.
//!   * The user has run `python cli.py login` once; the CLI persists its
//!     session in `~/.config/filen-cli/`.
//!
//! Configuration: the absolute path to `cli.py` is stored on
//! `DriveConfig.path`.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{CloudDrive, DirEntry, DriveType, FileStat};

/// Wire shape of `cli.py ls --json` output.
#[derive(Debug, Deserialize)]
struct ListPathOutput {
    #[allow(dead_code)]
    current_path: String,
    folders: Vec<NodeInfo>,
    files:   Vec<NodeInfo>,
}

#[derive(Debug, Deserialize, Default)]
struct NodeInfo {
    /// Item name (no path component).
    #[serde(default)]
    name:          Option<String>,
    /// File size in bytes (0 for folders).
    #[serde(default)]
    size:          Option<u64>,
    /// Unix-millis last-modified (filen returns int).  Folders may emit 0.
    #[serde(rename = "lastModified", default)]
    last_modified: Option<i64>,
    /// Server timestamp (creation), unix-seconds.  Fallback when lastModified is 0.
    #[serde(default)]
    timestamp:     Option<i64>,
    /// Filen UUID (for diagnostics; not used by the trait API).
    #[serde(default)]
    #[allow(dead_code)]
    uuid:          Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolveOutput {
    #[serde(rename = "type")]
    kind:     String,            // "file" | "folder"
    #[allow(dead_code)]
    uuid:     String,
    #[allow(dead_code)]
    path:     String,
    #[serde(default)]
    metadata: serde_json::Value,
}

pub struct FilenDrive {
    label:  String,
    cli_py: PathBuf,
    python: String,
}

impl FilenDrive {
    pub fn new(label: impl Into<String>, cli_py: impl Into<PathBuf>) -> Self {
        // Default to `python` (Miniconda's name on this machine).  Set
        // FILEN_CLI_PYTHON to `python3` or an absolute path on hosts where
        // that's the only Python with the required deps.
        let python = std::env::var("FILEN_CLI_PYTHON")
            .unwrap_or_else(|_| "python".to_owned());
        Self { label: label.into(), cli_py: cli_py.into(), python }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        if !self.cli_py.exists() {
            return Err(anyhow!(
                "filen-cli script not found at {} — set DriveConfig.path \
                 to the absolute location of cli.py",
                self.cli_py.display()
            ));
        }
        let mut cmd = Command::new(&self.python);
        cmd.arg(&self.cli_py);
        for a in args { cmd.arg(a); }
        let output = cmd.output()
            .with_context(|| format!("spawning {} {}", self.python, self.cli_py.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Patched CLI emits structured errors as JSON on stderr.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(stderr.trim()) {
                if let Some(err_msg) = v.get("error").and_then(|e| e.as_str()) {
                    return Err(anyhow!("filen-cli: {err_msg}"));
                }
            }
            return Err(anyhow!("filen-cli failed: {}", stderr.trim()));
        }
        Ok(output)
    }

    fn entry_from_node(node: &NodeInfo, is_dir: bool) -> DirEntry {
        DirEntry {
            name:   node.name.clone().unwrap_or_else(|| String::from("?")),
            is_dir,
            size:   if is_dir { None } else { node.size },
        }
    }
}

impl CloudDrive for FilenDrive {
    fn label(&self) -> &str { &self.label }
    fn drive_type(&self) -> DriveType { DriveType::Filen }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let path_str = if path.as_os_str().is_empty() { "/".to_owned() }
                       else { path.to_string_lossy().into_owned() };
        let output = self.run(&["ls", &path_str, "--json"])?;
        let parsed: ListPathOutput = serde_json::from_slice(&output.stdout)
            .with_context(|| format!(
                "parsing filen-cli ls --json output: {}",
                String::from_utf8_lossy(&output.stdout)
            ))?;

        let mut entries = Vec::with_capacity(parsed.folders.len() + parsed.files.len());
        for f in &parsed.folders { entries.push(Self::entry_from_node(f, true)); }
        for f in &parsed.files   { entries.push(Self::entry_from_node(f, false)); }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let path_str = path.to_string_lossy();
        let output = self.run(&["resolve", &path_str, "--json"])?;
        let parsed: ResolveOutput = serde_json::from_slice(&output.stdout)
            .with_context(|| format!(
                "parsing filen-cli resolve --json output: {}",
                String::from_utf8_lossy(&output.stdout)
            ))?;

        let is_dir = parsed.kind == "folder";
        let size = parsed.metadata.get("size")
            .and_then(|v| v.as_u64()).unwrap_or(0);
        // lastModified is unix-millis; timestamp is unix-seconds.
        let mtime_unix = parsed.metadata.get("lastModified")
            .and_then(|v| v.as_i64())
            .filter(|&v| v > 0)
            .map(|ms| ms / 1000)
            .or_else(|| {
                parsed.metadata.get("timestamp")
                    .and_then(|v| v.as_i64())
                    .filter(|&v| v > 0)
            });

        Ok(FileStat { size, is_dir, mtime_unix })
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let path_str = path.to_string_lossy();
        let tmp = tempfile::tempdir().context("temp dir for filen download")?;
        let basename = path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("path has no filename component: {}", path.display()))?;
        let dest = tmp.path().to_string_lossy().into_owned();

        // download-path overwrites by default if --on-conflict=overwrite.
        self.run(&[
            "download-path", &path_str, &dest,
            "--on-conflict", "overwrite",
        ])?;

        let out_file = tmp.path().join(&basename);
        if !out_file.exists() {
            return Err(anyhow!(
                "filen-cli reported success but file is missing at {}",
                out_file.display()
            ));
        }
        std::fs::read(&out_file)
            .with_context(|| format!("reading downloaded file {}", out_file.display()))
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        let tmp = tempfile::NamedTempFile::new().context("temp file for upload")?;
        std::fs::write(tmp.path(), data).context("staging upload bytes")?;
        let target_dir = path.parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_owned());
        self.run(&[
            "upload", &tmp.path().to_string_lossy(),
            "-t", &target_dir,
            "--on-conflict", "overwrite",
        ])?;
        Ok(())
    }

    fn delete(&self, path: &Path) -> Result<()> {
        // Use `trash` (recoverable) to mirror InternxtDrive's semantics.
        // Pass -r so folders work too; -f bypasses the interactive confirmation
        // (delete-path's `DELETE` prompt would deadlock on a non-tty subprocess).
        self.run(&[
            "-f", "trash", &path.to_string_lossy(), "-r", "--json",
        ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_metadata_correct() {
        let drive = FilenDrive::new("My Filen", "/nonexistent/cli.py");
        assert_eq!(drive.label(), "My Filen");
        assert_eq!(drive.drive_type(), DriveType::Filen);
    }

    #[test]
    fn missing_cli_returns_clear_error() {
        let drive = FilenDrive::new("test", "/definitely/does/not/exist.py");
        let err = drive.read_file(Path::new("/some/cloud/file.pdf"))
            .expect_err("should fail when cli.py missing");
        let msg = format!("{:#}", err);
        assert!(msg.contains("filen-cli script not found"),
            "expected helpful error, got: {msg}");
    }

    #[test]
    fn list_path_json_shape_deserialises() {
        let json = r#"{
            "current_path": "/",
            "folders": [
                {"type": "folder", "name": "code", "uuid": "0a91-...", "parent": "abc",
                 "timestamp": 1764455631, "lastModified": 0, "size": 0}
            ],
            "files": [
                {"type": "file", "name": "report.pdf", "uuid": "x-1",
                 "size": 12345, "lastModified": 1700000000000}
            ]
        }"#;
        let parsed: ListPathOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.folders.len(), 1);
        assert_eq!(parsed.folders[0].name.as_deref(), Some("code"));
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].size, Some(12345));
        assert_eq!(parsed.files[0].last_modified, Some(1_700_000_000_000));
    }

    #[test]
    fn resolve_json_shape_deserialises() {
        let json = r#"{
            "type": "file",
            "uuid": "abc-123",
            "path": "/Documents/report.pdf",
            "metadata": { "size": 5678, "lastModified": 1700000000000, "name": "report.pdf" },
            "parent": "p-1"
        }"#;
        let parsed: ResolveOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.kind, "file");
        assert_eq!(parsed.metadata.get("size").and_then(|v| v.as_u64()), Some(5678));
    }

    #[test]
    fn folder_entries_have_no_size() {
        let n = NodeInfo { name: Some("d".into()), size: Some(0), ..Default::default() };
        let e = FilenDrive::entry_from_node(&n, true);
        assert!(e.is_dir);
        assert_eq!(e.size, None, "folders must report no size");
    }

    #[test]
    fn file_entries_preserve_size() {
        let n = NodeInfo { name: Some("f.pdf".into()), size: Some(1234), ..Default::default() };
        let e = FilenDrive::entry_from_node(&n, false);
        assert!(!e.is_dir);
        assert_eq!(e.size, Some(1234));
    }
}
