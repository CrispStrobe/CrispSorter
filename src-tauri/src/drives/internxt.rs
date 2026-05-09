//! Internxt cloud-drive integration via the Python `internxt-cli`.
//!
//! Talks to the patched cli.py that supports `--json` on the read-side
//! commands (`whoami`, `list-path`, `resolve`).  All structured output is
//! plain JSON — no emoji-text scraping.  Downloads/uploads stage through
//! a tempfile because the Python CLI works on disk, not stdio.
//!
//! Requirements on the host:
//!   * `python` (Miniconda or system) on PATH with the deps
//!     `internxt-cli` needs (`click`, `cryptography`, `mnemonic`, `tqdm`,
//!     `requests`).  Override via `INTERNXT_CLI_PYTHON` env var if your
//!     interpreter is named `python3` or lives elsewhere.
//!   * The user has run `python3 cli.py login` once; the CLI persists its
//!     session token in `~/.config/internxt-cli/`.  We don't proxy login.
//!
//! Configuration: the absolute path to `cli.py` is stored on
//! `DriveConfig.path`.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{CloudDrive, DirEntry, DriveType, FileStat};

/// Wire shape of `cli.py list-path --json` output.
/// Mirrors `drive_service.list_folder_with_paths`'s return value.
#[derive(Debug, Deserialize)]
struct ListPathOutput {
    #[allow(dead_code)]
    current_path: String,
    folders: Vec<NodeInfo>,
    files:   Vec<NodeInfo>,
}

#[derive(Debug, Deserialize, Default)]
struct NodeInfo {
    /// Display name (with extension, no path).
    display_name:    Option<String>,
    /// Plain name (without extension for files, same as display for folders).
    #[serde(rename = "plainName", default)]
    plain_name:      Option<String>,
    /// File size in bytes (only present on files).
    #[serde(default)]
    size:            Option<u64>,
    /// ISO-8601 modification time (driveinet uses `modificationTime`,
    /// falls back to `updatedAt`).
    #[serde(rename = "modificationTime", default)]
    modification_time: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at:        Option<String>,
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

pub struct InternxtDrive {
    label:  String,
    cli_py: PathBuf,
    python: String,
}

impl InternxtDrive {
    pub fn new(label: impl Into<String>, cli_py: impl Into<PathBuf>) -> Self {
        // Default to `python` (Miniconda's name on this machine).  Set
        // INTERNXT_CLI_PYTHON to `python3` or an absolute interpreter path
        // on hosts where that's the only Python with the required deps.
        let python = std::env::var("INTERNXT_CLI_PYTHON")
            .unwrap_or_else(|_| "python".to_owned());
        Self { label: label.into(), cli_py: cli_py.into(), python }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        if !self.cli_py.exists() {
            return Err(anyhow!(
                "internxt-cli script not found at {} — set DriveConfig.path \
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
            // The patched CLI emits structured errors as JSON on err=stderr;
            // try to parse them for a cleaner message, otherwise show stderr verbatim.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(stderr.trim()) {
                if let Some(err_msg) = v.get("error").and_then(|e| e.as_str()) {
                    return Err(anyhow!("internxt-cli: {err_msg}"));
                }
            }
            return Err(anyhow!("internxt-cli failed: {}", stderr.trim()));
        }
        Ok(output)
    }
}

/// Parse the ISO-8601 modification timestamp into a unix-second integer.
/// Tolerant of trailing 'Z' (UTC) and ms precision.  Returns `None` if the
/// value is missing or malformed — callers treat that as "unknown mtime".
fn parse_iso(s: &str) -> Option<i64> {
    // Minimal ISO-8601 parser: "YYYY-MM-DDTHH:MM:SS[.fff][Z]" or with space.
    let s = s.trim().trim_end_matches('Z');
    let (date_part, time_part) = s.split_once(['T', ' '])?;
    let mut date = date_part.split('-');
    let y: i64 = date.next()?.parse().ok()?;
    let mo: i64 = date.next()?.parse().ok()?;
    let d:  i64 = date.next()?.parse().ok()?;
    let mut tparts = time_part.split(':');
    let h:  i64 = tparts.next()?.parse().ok()?;
    let mi: i64 = tparts.next()?.parse().ok()?;
    let s_part = tparts.next()?;
    let s_int: i64 = s_part.split('.').next()?.parse().ok()?;

    // days-from-civil (Howard Hinnant) — same algorithm as
    // crisp-index-server's iso_from_ms (inverse).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = (yy - era * 400) as u64;
    let m_norm = if mo > 2 { (mo - 3) as u64 } else { (mo + 9) as u64 };
    let doy = (153 * m_norm + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s_int)
}

impl CloudDrive for InternxtDrive {
    fn label(&self) -> &str { &self.label }
    fn drive_type(&self) -> DriveType { DriveType::Internxt }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let path_str = if path.as_os_str().is_empty() { "/".to_owned() }
                       else { path.to_string_lossy().into_owned() };
        let output = self.run(&["list-path", &path_str, "--json"])?;
        let parsed: ListPathOutput = serde_json::from_slice(&output.stdout)
            .with_context(|| format!(
                "parsing internxt-cli list-path output: {}",
                String::from_utf8_lossy(&output.stdout)
            ))?;

        let mut entries = Vec::with_capacity(parsed.folders.len() + parsed.files.len());
        for f in &parsed.folders {
            let name = f.display_name.clone()
                .or_else(|| f.plain_name.clone())
                .unwrap_or_else(|| String::from("?"));
            entries.push(DirEntry { name, is_dir: true, size: None });
        }
        for f in &parsed.files {
            let name = f.display_name.clone()
                .or_else(|| f.plain_name.clone())
                .unwrap_or_else(|| String::from("?"));
            entries.push(DirEntry { name, is_dir: false, size: f.size });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let path_str = path.to_string_lossy();
        let output = self.run(&["resolve", &path_str, "--json"])?;
        let parsed: ResolveOutput = serde_json::from_slice(&output.stdout)
            .with_context(|| format!(
                "parsing internxt-cli resolve output: {}",
                String::from_utf8_lossy(&output.stdout)
            ))?;

        let is_dir = parsed.kind == "folder";
        let size = parsed.metadata.get("size")
            .and_then(|v| v.as_u64()).unwrap_or(0);
        let mtime_unix = parsed.metadata.get("modificationTime")
            .and_then(|v| v.as_str())
            .or_else(|| parsed.metadata.get("updatedAt").and_then(|v| v.as_str()))
            .and_then(parse_iso);

        Ok(FileStat { size, is_dir, mtime_unix })
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let path_str = path.to_string_lossy();
        let tmp = tempfile::tempdir().context("temp dir for internxt download")?;
        let basename = path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("path has no filename component: {}", path.display()))?;
        let dest = tmp.path().to_string_lossy().into_owned();

        self.run(&[
            "download-path", &path_str,
            "--destination", &dest,
            "--on-conflict", "overwrite",
        ])?;

        let out_file = tmp.path().join(&basename);
        if !out_file.exists() {
            return Err(anyhow!(
                "internxt-cli reported success but file is missing at {}",
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
        self.run(&["trash-path", &path.to_string_lossy(), "--force"])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_metadata_correct() {
        let drive = InternxtDrive::new("My Internxt", "/nonexistent/cli.py");
        assert_eq!(drive.label(), "My Internxt");
        assert_eq!(drive.drive_type(), DriveType::Internxt);
    }

    #[test]
    fn missing_cli_returns_clear_error() {
        let drive = InternxtDrive::new("test", "/definitely/does/not/exist.py");
        let err = drive.read_file(Path::new("/some/cloud/file.pdf"))
            .expect_err("should fail when cli.py missing");
        let msg = format!("{:#}", err);
        assert!(msg.contains("internxt-cli script not found"),
            "expected helpful error, got: {msg}");
    }

    #[test]
    fn parse_iso_unix_epoch() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("1970-01-01 00:00:00"),  Some(0));
    }

    #[test]
    fn parse_iso_known_date() {
        // 2024-01-01T00:00:00Z = 1_704_067_200
        assert_eq!(parse_iso("2024-01-01T00:00:00Z"), Some(1_704_067_200));
        assert_eq!(parse_iso("2024-01-01T12:34:56Z"),
                   Some(1_704_067_200 + 12*3600 + 34*60 + 56));
    }

    #[test]
    fn parse_iso_handles_malformed() {
        assert!(parse_iso("not a date").is_none());
        assert!(parse_iso("").is_none());
        assert!(parse_iso("2024-13-01T00:00:00").is_some()); // we don't validate ranges, just shape
    }

    #[test]
    fn parse_iso_tolerates_milliseconds() {
        // ".789" sub-second portion truncated; second-precision is enough.
        assert_eq!(parse_iso("2024-01-01T00:00:00.789Z"), Some(1_704_067_200));
    }

    #[test]
    fn list_path_json_shape_deserialises() {
        // Match the wire shape that cli.py list-path --json emits.
        let json = r#"{
            "current_path": "/Documents",
            "folders": [
                {"display_name": "Subfolder", "uuid": "abc"}
            ],
            "files": [
                {"display_name": "report.pdf", "size": 1234, "modificationTime": "2024-01-01T12:00:00Z"}
            ]
        }"#;
        let parsed: ListPathOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.folders.len(), 1);
        assert_eq!(parsed.folders[0].display_name.as_deref(), Some("Subfolder"));
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].size, Some(1234));
    }

    #[test]
    fn resolve_json_shape_deserialises() {
        let json = r#"{
            "type": "file",
            "uuid": "abc-123",
            "path": "/Documents/report.pdf",
            "metadata": { "size": 5678, "modificationTime": "2024-01-01T00:00:00Z" }
        }"#;
        let parsed: ResolveOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.kind, "file");
        assert_eq!(parsed.metadata.get("size").and_then(|v| v.as_u64()), Some(5678));
    }
}
