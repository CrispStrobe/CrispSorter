//! Cross-mount volume awareness — PLAN P7.6.
//!
//! A document indexed from `/Volumes/Archive/papers/foo.pdf` is "the same
//! file" tomorrow even if `/Volumes/Archive` mounted under a different
//! letter / suffix / drive-mapping. Mount-point paths drift; the
//! filesystem-level **volume id** does not.
//!
//! This module exposes:
//!
//! * [`volume_id_for_path`] — given any path on a mounted volume, return
//!   that volume's stable identifier (UUID on macOS / Linux, hex serial
//!   on Windows). `None` when the path is missing, on a tmpfs / network
//!   share without a UUID, or the platform helper fails.
//! * [`list_mounted_volumes`] — enumerate currently-mounted volumes with
//!   their id, mount point, and human label. Used by the frontend to
//!   show a "Volumes" picker for index filters.
//!
//! Volume ids are stored on each `documents` row's `metadata_json`
//! field next to `mtime_unix` (`{"mtime_unix":N,"volume_id":"…"}`). A
//! follow-up phase will add an `available_volume_ids` filter at search
//! time so unmounted-volume rows can be hidden until the drive is back.
//!
//! ## Why shell-out instead of FFI?
//!
//! Each platform has a perfectly good CLI for this and they ship by
//! default:
//!
//! * macOS — `diskutil info <mount>` (always present)
//! * Linux — `findmnt -no UUID --target <path>` (util-linux, in every
//!   distro since systemd days)
//! * Windows — `wmic logicaldisk get VolumeSerialNumber` (deprecated
//!   but still ships through Win11; PowerShell `Get-Volume` is a
//!   fallback if `wmic` is removed in a future release)
//!
//! FFI alternatives (`libblkid`, IOKit, `GetVolumeInformationW`)
//! buy us microseconds we'll never notice and a dependency
//! footprint we definitely will. Volume detection happens at most
//! once per ingest (so once per file) and the user doesn't notice
//! a 20 ms shell-out hidden behind a 200 ms PDF extract.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountedVolume {
    /// Stable volume identifier. Format depends on platform:
    /// * macOS: `12345678-1234-1234-1234-123456789ABC` (uppercase UUID)
    /// * Linux: `12345678-1234-1234-1234-123456789abc` (lowercase UUID)
    /// * Windows: `ABCD1234` (hex serial, 8 chars)
    pub id: String,
    /// Where the volume is currently mounted (`/Volumes/Archive`,
    /// `/mnt/data`, `D:\`).
    pub mount_point: String,
    /// Human-readable label if the OS reports one (`"Archive"`,
    /// `"Macintosh HD"`). Empty string when no label is set.
    pub name: String,
}

/// Resolve `path` to its volume's stable id. Returns `None` for any
/// failure mode — caller treats `None` as "we don't know which volume,
/// don't filter on it" rather than propagating the error. Most callers
/// (ingest hot path) want best-effort enrichment.
pub fn volume_id_for_path(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        return macos::volume_id_for_path(path);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::volume_id_for_path(path);
    }
    #[cfg(target_os = "windows")]
    {
        return windows::volume_id_for_path(path);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = path;
        None
    }
}

/// Enumerate currently-mounted volumes. Empty vec when the platform
/// helper fails — frontend then falls back to "no per-volume filter".
pub fn list_mounted_volumes() -> Vec<MountedVolume> {
    #[cfg(target_os = "macos")]
    {
        return macos::list_mounted_volumes();
    }
    #[cfg(target_os = "linux")]
    {
        return linux::list_mounted_volumes();
    }
    #[cfg(target_os = "windows")]
    {
        return windows::list_mounted_volumes();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────────

#[allow(dead_code)] // used per-platform behind cfg
fn run_capturing(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

// ── macOS ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use super::{run_capturing, MountedVolume};
    use std::path::Path;
    use std::process::Command;

    pub fn volume_id_for_path(path: &Path) -> Option<String> {
        let mount = mount_point_for(path)?;
        diskutil_volume_uuid(&mount)
    }

    pub fn list_mounted_volumes() -> Vec<MountedVolume> {
        // `mount` lines look like:
        //   /dev/disk3s5 on / (apfs, local, ...)
        //   /dev/disk5s1 on /Volumes/Archive (apfs, local, ...)
        let raw = match run_capturing(&mut Command::new("mount")) {
            Some(s) => s,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        for line in raw.lines() {
            if let Some(mount) = parse_mount_line(line) {
                if let Some(uuid) = diskutil_volume_uuid(&mount) {
                    let name = std::path::Path::new(&mount)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    out.push(MountedVolume {
                        id: uuid,
                        mount_point: mount,
                        name,
                    });
                }
            }
        }
        out
    }

    /// Resolve the mount point a given path lives on via `df -P`.
    /// Output line we care about (POSIX format guaranteed by `-P`):
    ///   /dev/disk3s5  500000000  100000000  ...  /Volumes/Archive
    fn mount_point_for(path: &Path) -> Option<String> {
        let raw = run_capturing(Command::new("df").arg("-P").arg(path))?;
        let last = raw.lines().last()?;
        // The mount point is the LAST whitespace-separated token. Mount
        // points can contain spaces on macOS (`Macintosh HD`), so we
        // can't just `split_whitespace` and take `[5]` — the path then
        // gets cut off mid-name. Find the start of the last column by
        // skipping the first 5 columns (Filesystem, 1024-blocks, Used,
        // Available, Capacity).
        let mut chars = last.char_indices();
        let mut col_starts = vec![0];
        let mut in_ws = last.starts_with(char::is_whitespace);
        for (i, c) in chars.by_ref() {
            if c.is_whitespace() {
                in_ws = true;
            } else if in_ws {
                col_starts.push(i);
                in_ws = false;
            }
        }
        if col_starts.len() < 6 {
            return None;
        }
        Some(last[col_starts[5]..].trim().to_owned())
    }

    fn parse_mount_line(line: &str) -> Option<String> {
        // `<device> on <mount-point> (<opts>)`
        // mount points may contain spaces ("/Volumes/My Drive") so we
        // search for the trailing " (" rather than splitting on
        // whitespace and taking [2].
        let on_idx = line.find(" on ")?;
        let after = &line[on_idx + 4..];
        let opts_idx = after.rfind(" (")?;
        Some(after[..opts_idx].to_owned())
    }

    fn diskutil_volume_uuid(mount: &str) -> Option<String> {
        let raw = run_capturing(Command::new("diskutil").arg("info").arg(mount))?;
        parse_diskutil_uuid(&raw)
    }

    pub(super) fn parse_diskutil_uuid(raw: &str) -> Option<String> {
        for line in raw.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Volume UUID:") {
                let value = rest.trim();
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
        None
    }
}

// ── Linux ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::{run_capturing, MountedVolume};
    use std::path::Path;
    use std::process::Command;

    pub fn volume_id_for_path(path: &Path) -> Option<String> {
        // `findmnt -no UUID --target <path>` resolves the path to its
        // mount and prints just the UUID, blank if none (e.g. tmpfs,
        // network mount).
        let raw = run_capturing(
            Command::new("findmnt")
                .arg("-no")
                .arg("UUID")
                .arg("--target")
                .arg(path),
        )?;
        let uuid = raw.trim();
        if uuid.is_empty() {
            None
        } else {
            Some(uuid.to_owned())
        }
    }

    pub fn list_mounted_volumes() -> Vec<MountedVolume> {
        // -P : print canonical paths (resolved symlinks)
        // -n : no header
        // -l : list, not tree
        // -o : pick columns
        let raw = match run_capturing(
            Command::new("findmnt")
                .arg("-Pnlo")
                .arg("UUID,TARGET,LABEL"),
        ) {
            Some(s) => s,
            None => return Vec::new(),
        };
        parse_findmnt_pairs(&raw)
    }

    /// Parses findmnt's `-P` (key="value") output:
    ///   UUID="abc" TARGET="/mnt/data" LABEL="Archive"
    pub(super) fn parse_findmnt_pairs(raw: &str) -> Vec<MountedVolume> {
        let mut out = Vec::new();
        for line in raw.lines() {
            let mut id = String::new();
            let mut mount_point = String::new();
            let mut name = String::new();
            for (k, v) in iter_kv_pairs(line) {
                match k {
                    "UUID" => id = v,
                    "TARGET" => mount_point = v,
                    "LABEL" => name = v,
                    _ => {}
                }
            }
            if !id.is_empty() && !mount_point.is_empty() {
                out.push(MountedVolume {
                    id,
                    mount_point,
                    name,
                });
            }
        }
        out
    }

    fn iter_kv_pairs(line: &str) -> Vec<(&str, String)> {
        let mut out = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // skip whitespace
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let key_start = i;
            while i < bytes.len() && bytes[i] != b'=' {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let key = &line[key_start..i];
            i += 1; // skip '='
            if i >= bytes.len() || bytes[i] != b'"' {
                break;
            }
            i += 1; // opening quote
            let val_start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let value = line[val_start..i].to_owned();
            out.push((key, value));
            if i < bytes.len() {
                i += 1; // closing quote
            }
        }
        out
    }
}

// ── Windows ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use super::{run_capturing, MountedVolume};
    use std::path::Path;
    use std::process::Command;

    pub fn volume_id_for_path(path: &Path) -> Option<String> {
        let drive = drive_letter_for(path)?;
        wmic_serial(&drive)
    }

    pub fn list_mounted_volumes() -> Vec<MountedVolume> {
        // Single wmic call returns all logical disks.
        // /value format gives `Key=Value` pairs separated by blank lines.
        let raw = match run_capturing(
            Command::new("wmic")
                .arg("logicaldisk")
                .arg("get")
                .arg("DeviceID,VolumeName,VolumeSerialNumber")
                .arg("/value"),
        ) {
            Some(s) => s,
            None => return Vec::new(),
        };
        parse_wmic_value(&raw)
    }

    fn drive_letter_for(path: &Path) -> Option<String> {
        // Path components on Windows: "C:" then "\\" then ...
        let mut comps = path.components();
        match comps.next()? {
            std::path::Component::Prefix(p) => match p.kind() {
                std::path::Prefix::Disk(letter) | std::path::Prefix::VerbatimDisk(letter) => {
                    Some(format!("{}:", letter as char))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn wmic_serial(drive: &str) -> Option<String> {
        let raw = run_capturing(
            Command::new("wmic")
                .arg("logicaldisk")
                .arg("where")
                .arg(format!("DeviceID='{}'", drive))
                .arg("get")
                .arg("VolumeSerialNumber")
                .arg("/value"),
        )?;
        for line in raw.lines() {
            if let Some(rest) = line.trim().strip_prefix("VolumeSerialNumber=") {
                let v = rest.trim();
                if !v.is_empty() {
                    return Some(v.to_owned());
                }
            }
        }
        None
    }

    pub(super) fn parse_wmic_value(raw: &str) -> Vec<MountedVolume> {
        // wmic /value emits records separated by blank lines, each line
        // is `Key=Value`. A record without DeviceID is junk (the BOM-
        // prefixed first record is empty on some Windows builds).
        let mut out = Vec::new();
        let mut device_id = String::new();
        let mut name = String::new();
        let mut serial = String::new();
        for line in raw.lines() {
            let line = line.trim_end_matches(['\r']).trim();
            if line.is_empty() {
                if !device_id.is_empty() && !serial.is_empty() {
                    out.push(MountedVolume {
                        id: serial.clone(),
                        mount_point: format!("{}\\", device_id),
                        name: name.clone(),
                    });
                }
                device_id.clear();
                name.clear();
                serial.clear();
                continue;
            }
            if let Some(rest) = line.strip_prefix("DeviceID=") {
                device_id = rest.to_owned();
            } else if let Some(rest) = line.strip_prefix("VolumeName=") {
                name = rest.to_owned();
            } else if let Some(rest) = line.strip_prefix("VolumeSerialNumber=") {
                serial = rest.to_owned();
            }
        }
        if !device_id.is_empty() && !serial.is_empty() {
            out.push(MountedVolume {
                id: serial,
                mount_point: format!("{}\\", device_id),
                name,
            });
        }
        out
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_diskutil_uuid_line() {
        let sample = r#"
   Device Identifier:        disk5s1
   Volume Name:              Archive
   Mounted:                  Yes
   Volume UUID:              12345678-1234-1234-1234-123456789ABC
   Disk Size:                1.0 TB
"#;
        assert_eq!(
            macos::parse_diskutil_uuid(sample),
            Some("12345678-1234-1234-1234-123456789ABC".to_owned())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn diskutil_uuid_missing_returns_none() {
        let sample = "Device Identifier: disk0\nMounted: Yes\n";
        assert_eq!(macos::parse_diskutil_uuid(sample), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_findmnt_pairs() {
        let sample = r#"UUID="abc-1" TARGET="/" LABEL="root"
UUID="def-2" TARGET="/mnt/data" LABEL="Archive"
"#;
        let v = linux::parse_findmnt_pairs(sample);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "abc-1");
        assert_eq!(v[0].mount_point, "/");
        assert_eq!(v[1].name, "Archive");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn skips_findmnt_rows_without_uuid() {
        // tmpfs has no UUID → findmnt prints empty UUID="".
        let sample = r#"UUID="" TARGET="/tmp" LABEL=""
UUID="real" TARGET="/mnt" LABEL=""
"#;
        let v = linux::parse_findmnt_pairs(sample);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "real");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parses_wmic_value_records() {
        let sample = "DeviceID=C:\r\nVolumeName=System\r\nVolumeSerialNumber=ABCD1234\r\n\r\nDeviceID=D:\r\nVolumeName=Archive\r\nVolumeSerialNumber=DEAD\r\n";
        let v = windows::parse_wmic_value(sample);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "ABCD1234");
        assert_eq!(v[0].mount_point, "C:\\");
        assert_eq!(v[1].name, "Archive");
    }

    #[test]
    fn id_for_missing_path_is_none() {
        let p = std::path::Path::new("/this/path/does/not/exist/ever/p7p6");
        assert!(volume_id_for_path(p).is_none());
    }
}
