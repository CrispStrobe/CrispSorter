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
//! ## How each platform answers
//!
//! * **macOS — FFI.** `getattrlist(2)` for `ATTR_VOL_UUID` and
//!   `getfsstat(2)` for the mount table. No process, no PATH, nothing to
//!   install.
//! * Linux — `findmnt -no UUID --target <path>` (util-linux, in every
//!   distro since systemd days)
//! * Windows — `wmic logicaldisk get VolumeSerialNumber` (deprecated
//!   but still ships through Win11; PowerShell `Get-Volume` is a
//!   fallback if `wmic` is removed in a future release)
//!
//! The two shell-out platforms are behind `feature = "sidecars"` and
//! degrade to "we don't know which volume" without it, which is a state
//! every caller already handles. macOS does not need the flag at all,
//! which is the point: it is the platform with a sandboxed SKU.
//!
//! **This used to be shell-out everywhere**, on the reasoning that a 20 ms
//! `diskutil` call hidden behind a 200 ms PDF extract costs nothing and
//! saves a dependency. Two things changed it (PLAN P36.2): App Sandbox
//! forbids the spawn outright, so on macOS the "cheap" option became the
//! impossible one; and the FFI turned out to be ~40 lines against `libc`,
//! which was already in the tree transitively. The migration is
//! id-compatible — `ATTR_VOL_UUID` returns byte-for-byte what `diskutil
//! info` printed, verified against APFS system, Data and external
//! volumes — so ids persisted in `metadata_json` by older builds still
//! resolve.

use std::path::Path;
#[cfg(all(feature = "sidecars", any(target_os = "linux", target_os = "windows")))]
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
    #[cfg(all(feature = "sidecars", target_os = "linux"))]
    {
        return linux::volume_id_for_path(path);
    }
    #[cfg(all(feature = "sidecars", target_os = "windows"))]
    {
        return windows::volume_id_for_path(path);
    }
    #[cfg(not(any(
        target_os = "macos",
        all(feature = "sidecars", any(target_os = "linux", target_os = "windows"))
    )))]
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
    #[cfg(all(feature = "sidecars", target_os = "linux"))]
    {
        return linux::list_mounted_volumes();
    }
    #[cfg(all(feature = "sidecars", target_os = "windows"))]
    {
        return windows::list_mounted_volumes();
    }
    #[cfg(not(any(
        target_os = "macos",
        all(feature = "sidecars", any(target_os = "linux", target_os = "windows"))
    )))]
    {
        Vec::new()
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────────

#[cfg(all(feature = "sidecars", any(target_os = "linux", target_os = "windows")))]
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
    use super::MountedVolume;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// The `attrlist` request shape and the packed reply buffer, both from
    /// `getattrlist(2)`.
    ///
    /// Requesting `ATTR_VOL_INFO | ATTR_VOL_UUID` and nothing else keeps the
    /// reply fixed-size: a `u32` total length followed by the 16 raw UUID
    /// bytes. Adding `ATTR_VOL_NAME` would make it variable-length (names
    /// come back as an `attrreference_t` offset+length pair pointing into
    /// the tail of the same buffer), and the name is already available for
    /// free from the mount point's last component — which is exactly what
    /// the `diskutil` implementation this replaced used.
    const UUID_REPLY_LEN: usize = 4 + 16;

    /// Ask the filesystem for the UUID of the volume containing `path`.
    ///
    /// `path` need not be a mount point: volume attributes are answered for
    /// the volume the path lives on, which is precisely the `df -P` step
    /// the previous implementation needed a second process for.
    fn volume_uuid(path: &Path) -> Option<String> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;

        let mut request = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let mut reply = [0u8; UUID_REPLY_LEN];

        // SAFETY: `c_path` is a NUL-terminated path we own for the duration
        // of the call; `request` and `reply` are live, correctly-sized
        // stack objects, and `reply.len()` is what we tell the kernel it
        // is. `getattrlist` writes at most that many bytes.
        let rc = unsafe {
            libc::getattrlist(
                c_path.as_ptr(),
                &mut request as *mut _ as *mut libc::c_void,
                reply.as_mut_ptr() as *mut libc::c_void,
                reply.len(),
                0,
            )
        };
        if rc != 0 {
            // Every failure here is a legitimate "we don't know": a volume
            // whose filesystem has no UUID (tmpfs, some network mounts), a
            // path that vanished between the caller's check and this call,
            // or a sandbox denial on a path we were not granted.
            return None;
        }

        // The leading u32 is how much the kernel actually wrote. A short
        // reply means the volume answered ATTR_VOL_INFO but not the UUID,
        // and the remaining bytes are whatever was on the stack — reading
        // them would invent an id.
        let written = u32::from_ne_bytes(reply[..4].try_into().ok()?) as usize;
        if written < UUID_REPLY_LEN {
            return None;
        }
        let uuid: [u8; 16] = reply[4..UUID_REPLY_LEN].try_into().ok()?;
        // An all-zero UUID is the filesystem saying "none", not an id.
        if uuid.iter().all(|&b| b == 0) {
            return None;
        }
        Some(format_uuid(&uuid))
    }

    /// Render 16 raw bytes as macOS spells a volume UUID: uppercase,
    /// hyphenated 8-4-4-4-12.
    ///
    /// The exact format matters. Volume ids are persisted in each
    /// document's `metadata_json`, so this has to reproduce byte-for-byte
    /// what `diskutil info` printed before P36.2 — otherwise every row
    /// written by an older build stops matching. Verified equal on APFS
    /// system, Data and external volumes.
    pub(super) fn format_uuid(bytes: &[u8; 16]) -> String {
        let hex = |slice: &[u8]| -> String {
            slice.iter().map(|b| format!("{b:02X}")).collect()
        };
        format!(
            "{}-{}-{}-{}-{}",
            hex(&bytes[0..4]),
            hex(&bytes[4..6]),
            hex(&bytes[6..8]),
            hex(&bytes[8..10]),
            hex(&bytes[10..16])
        )
    }

    pub fn volume_id_for_path(path: &Path) -> Option<String> {
        volume_uuid(path)
    }

    pub fn list_mounted_volumes() -> Vec<MountedVolume> {
        let mut out = Vec::new();
        for mount_point in mount_points() {
            let Some(id) = volume_uuid(Path::new(&mount_point)) else {
                // No UUID → nothing stable to filter on. Skipping matches
                // what the `mount` + `diskutil` pair did.
                continue;
            };
            let name = Path::new(&mount_point)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(MountedVolume { id, mount_point, name });
        }
        out
    }

    /// Every currently-mounted filesystem's mount point, via `getfsstat(2)`.
    ///
    /// `getfsstat` rather than the more obvious `getmntinfo(3)`: the latter
    /// returns a pointer into a static buffer it reallocates, so two
    /// threads enumerating volumes at once race on it. `getfsstat` writes
    /// into a buffer we own. Called with a null buffer first, it returns
    /// only the count, which is how we size that buffer.
    fn mount_points() -> Vec<String> {
        // SAFETY: passing a null buffer with size 0 is the documented way
        // to ask for the count without writing anything.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Vec::new();
        }

        // Mounts can appear between the counting call and the filling one.
        // Over-allocate a little so the common case does not silently
        // truncate; the kernel caps what it writes at the size we pass and
        // returns how many entries it actually filled.
        let capacity = count as usize + 8;
        let mut buffer: Vec<libc::statfs> = Vec::with_capacity(capacity);
        let bufsize = std::mem::size_of::<libc::statfs>() * capacity;

        // SAFETY: `buffer` has room for `capacity` entries and we declare
        // exactly that many bytes. `filled` is the number the kernel
        // initialised, so only that many are read below.
        let filled = unsafe {
            libc::getfsstat(
                buffer.as_mut_ptr(),
                bufsize as libc::c_int,
                libc::MNT_NOWAIT,
            )
        };
        if filled <= 0 {
            return Vec::new();
        }
        let filled = (filled as usize).min(capacity);
        // SAFETY: the kernel initialised `filled` entries.
        unsafe { buffer.set_len(filled) };

        buffer
            .iter()
            .filter_map(|fs| c_str_field(&fs.f_mntonname))
            .collect()
    }

    /// Read a NUL-terminated, fixed-size `c_char` array as a `String`.
    /// Returns `None` for an empty field or non-UTF-8 bytes — a mount
    /// point we cannot name is one we cannot hand to the frontend anyway.
    fn c_str_field(field: &[libc::c_char]) -> Option<String> {
        let bytes: Vec<u8> = field
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        if bytes.is_empty() {
            return None;
        }
        String::from_utf8(bytes).ok()
    }
}

// ── Linux ───────────────────────────────────────────────────────────────────

#[cfg(all(feature = "sidecars", target_os = "linux"))]
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

#[cfg(all(feature = "sidecars", target_os = "windows"))]
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

    /// The id format is a compatibility contract, not a detail.
    ///
    /// Volume ids are persisted in each document's `metadata_json`, so
    /// P36.2's move from `diskutil info` to `getattrlist(2)` is only safe
    /// while the FFI path renders the same string `diskutil` printed:
    /// uppercase, hyphenated 8-4-4-4-12. The sample below is the exact
    /// shape the old parser used to return.
    #[cfg(target_os = "macos")]
    #[test]
    fn uuid_bytes_render_the_way_diskutil_printed_them() {
        let bytes: [u8; 16] = [
            0x12, 0x34, 0x56, 0x78, 0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x56, 0x78,
            0x9A, 0xBC,
        ];
        assert_eq!(
            macos::format_uuid(&bytes),
            "12345678-1234-1234-1234-123456789ABC"
        );
        // Zero-padding, not width-trimming: a leading zero byte must stay
        // two hex digits or every subsequent group shifts.
        assert_eq!(
            macos::format_uuid(&[0u8; 16]),
            "00000000-0000-0000-0000-000000000000"
        );
    }

    /// The FFI path has to actually answer on the machine running the
    /// tests — a `None` for every path would make `volume_id_for_path`
    /// silently useless, and the "best-effort, None is fine" contract
    /// means nothing else would ever complain.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_ffi_path_resolves_a_real_volume() {
        let id = volume_id_for_path(std::path::Path::new("/"))
            .expect("the root volume must have a UUID");
        assert_eq!(id.len(), 36, "expected a hyphenated UUID, got {id:?}");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "got {id:?}"
        );
        // A path deep inside a volume must resolve to that volume, not
        // fail — this is what replaced the `df -P` mount-point lookup.
        let nested = volume_id_for_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("a nested path must resolve to its containing volume");
        assert_eq!(nested.len(), 36);

        // And enumeration must find at least the root volume.
        let mounted = list_mounted_volumes();
        assert!(
            mounted.iter().any(|v| v.mount_point == "/"),
            "getfsstat did not report the root mount: {mounted:?}"
        );
    }

    #[cfg(all(feature = "sidecars", target_os = "linux"))]
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

    #[cfg(all(feature = "sidecars", target_os = "linux"))]
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

    #[cfg(all(feature = "sidecars", target_os = "windows"))]
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
