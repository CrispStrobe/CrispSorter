use serde::{Deserialize, Serialize};
use std::fmt;
/// File location URI model.
///
/// Every indexed document carries exactly one `location_uri` string. It is a typed URI
/// that encodes where the original file lives and under whose identity.
///
/// Scheme:
///   crisp+local://{user-uuid}@{machine-uuid}/{absolute-path}
///   crisp+vps://{user-uuid}@{host}:{port}/{path}
///   crisp+internxt://{user-uuid}/{cloud-path}
///   crisp+internxt-zip://{user-uuid}/{archive-cloud-path}#{internal-path}
///   crisp+drive://{drive-id}/{remote-path}     ← generic CloudDrive registry entry
///
/// Single-user installs: both UUIDs are auto-populated from config; they never appear in UI.
/// The `#fragment` convention for InternxtZip mirrors standard URL fragments.
use std::path::PathBuf;
use uuid::Uuid;

/// How expensive it is to physically retrieve this file right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetrievalCost {
    /// File is on the local machine.
    Free,
    /// File is on a VPS that is assumed to be reachable.
    Cheap,
    /// File is in cloud storage (Internxt); must be fetched on demand.
    Expensive,
}

impl fmt::Display for RetrievalCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetrievalCost::Free => write!(f, "local"),
            RetrievalCost::Cheap => write!(f, "vps"),
            RetrievalCost::Expensive => write!(f, "cloud"),
        }
    }
}

/// Typed representation of a file's storage location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileLocation {
    Local {
        user_id: Uuid,
        machine_id: Uuid,
        path: PathBuf,
    },
    Vps {
        user_id: Uuid,
        host: String,
        port: u16,
        path: String,
    },
    Internxt {
        user_id: Uuid,
        cloud_path: String,
    },
    /// A file inside an encrypted 7z archive stored in Internxt.
    /// `archive_cloud_path` is the Internxt path to the .7z file.
    /// `internal_path` is the path of the member within the archive.
    InternxtZip {
        user_id: Uuid,
        archive_cloud_path: String,
        internal_path: String,
    },
    /// A file inside a cloud-backup encrypted 7z archive.
    /// `archive_id` matches cloud-backup's `archives.archive_id`.
    /// `file_hash` is the SHA-256 (or other) hash from `file_manifest.file_hash`.
    /// Resolution: spawn `retrieve.py --archive {archive_id} --hash {file_hash}`.
    CbArchive {
        archive_id: i64,
        file_hash: String,
        original_path: String,
    },
    /// A file on a registered CloudDrive (WebDAV / Filen / Internxt / SFTP / …).
    /// `drive_id` is the registry UUID; resolution looks up the
    /// drive via `DriveRegistry::open(...).drives` and calls
    /// `CloudDrive::read_file(remote_path)` on it.  The same row works
    /// regardless of which backend the user picked when adding the drive.
    Drive {
        drive_id: String,
        remote_path: String,
    },
}

impl FileLocation {
    /// Encode to the canonical URI string stored in the index.
    pub fn to_uri(&self) -> String {
        match self {
            FileLocation::Local {
                user_id,
                machine_id,
                path,
            } => {
                // Normalise path separators to forward-slash for cross-platform URIs.
                let path_str = path.to_string_lossy().replace('\\', "/");
                // Ensure the path component starts with a leading slash.
                let path_str = if path_str.starts_with('/') {
                    path_str
                } else {
                    format!("/{}", path_str)
                };
                format!("crisp+local://{}@{}{}", user_id, machine_id, path_str)
            }
            FileLocation::Vps {
                user_id,
                host,
                port,
                path,
            } => {
                let path_str = if path.starts_with('/') {
                    path.clone()
                } else {
                    format!("/{}", path)
                };
                format!("crisp+vps://{}@{}:{}{}", user_id, host, port, path_str)
            }
            FileLocation::Internxt {
                user_id,
                cloud_path,
            } => {
                let path_str = if cloud_path.starts_with('/') {
                    cloud_path.clone()
                } else {
                    format!("/{}", cloud_path)
                };
                format!("crisp+internxt://{}{}", user_id, path_str)
            }
            FileLocation::InternxtZip {
                user_id,
                archive_cloud_path,
                internal_path,
            } => {
                let archive_str = if archive_cloud_path.starts_with('/') {
                    archive_cloud_path.clone()
                } else {
                    format!("/{}", archive_cloud_path)
                };
                format!(
                    "crisp+internxt-zip://{}{}#{}",
                    user_id, archive_str, internal_path
                )
            }
            FileLocation::CbArchive {
                archive_id,
                file_hash,
                original_path,
            } => {
                let path_enc = original_path.replace(' ', "%20");
                format!("crisp+cb-archive://{}/{}#{}", archive_id, file_hash, path_enc)
            }
            FileLocation::Drive { drive_id, remote_path } => {
                let path_str = if remote_path.starts_with('/') {
                    remote_path.clone()
                } else {
                    format!("/{}", remote_path)
                };
                // Encode spaces only (RFC 3986 unreserved is wider, but the
                // round-trip pair just needs to survive `from_uri`).
                let path_enc = path_str.replace(' ', "%20");
                format!("crisp+drive://{}{}", drive_id, path_enc)
            }
        }
    }

    /// Parse a URI string back into a typed `FileLocation`.
    pub fn from_uri(s: &str) -> anyhow::Result<Self> {
        if let Some(rest) = s.strip_prefix("crisp+local://") {
            return parse_local(rest);
        }
        if let Some(rest) = s.strip_prefix("crisp+vps://") {
            return parse_vps(rest);
        }
        if let Some(rest) = s.strip_prefix("crisp+internxt-zip://") {
            return parse_internxt_zip(rest);
        }
        if let Some(rest) = s.strip_prefix("crisp+internxt://") {
            return parse_internxt(rest);
        }
        if let Some(rest) = s.strip_prefix("crisp+drive://") {
            return parse_drive(rest);
        }
        anyhow::bail!("Unknown crisp URI scheme: {}", s)
    }

    pub fn user_id(&self) -> Uuid {
        match self {
            FileLocation::Local { user_id, .. } => *user_id,
            FileLocation::Vps { user_id, .. } => *user_id,
            FileLocation::Internxt { user_id, .. } => *user_id,
            FileLocation::InternxtZip { user_id, .. } => *user_id,
            FileLocation::CbArchive { .. } => Uuid::nil(),
            FileLocation::Drive { .. } => Uuid::nil(),
        }
    }

    pub fn retrieval_cost(&self) -> RetrievalCost {
        match self {
            FileLocation::Local { .. } => RetrievalCost::Free,
            FileLocation::Vps { .. } => RetrievalCost::Cheap,
            FileLocation::Internxt { .. } => RetrievalCost::Expensive,
            FileLocation::InternxtZip { .. } => RetrievalCost::Expensive,
            FileLocation::CbArchive { .. } => RetrievalCost::Expensive,
            // Conservative: `Drive` rows can be local (LocalDrive on a USB
            // disk) or cloud-bandwidth-bound (Filen).  Treat as Expensive
            // until we plumb a per-drive cost annotation through the
            // registry.  The UI can still show "Lokal" if the drive
            // happens to be DriveType::Local.
            FileLocation::Drive { .. } => RetrievalCost::Expensive,
        }
    }

    /// Returns the filename component (last path segment), if available.
    pub fn filename(&self) -> Option<String> {
        match self {
            FileLocation::Local { path, .. } => {
                path.file_name().map(|n| n.to_string_lossy().into_owned())
            }
            FileLocation::Vps { path, .. } => path.split('/').next_back().map(str::to_owned),
            FileLocation::Internxt { cloud_path, .. } => {
                cloud_path.split('/').next_back().map(str::to_owned)
            }
            FileLocation::InternxtZip { internal_path, .. } => {
                internal_path.split('/').next_back().map(str::to_owned)
            }
            FileLocation::CbArchive { original_path, .. } => {
                original_path.split('/').next_back().map(str::to_owned)
            }
            FileLocation::Drive { remote_path, .. } => {
                remote_path.split('/').next_back().map(str::to_owned)
            }
        }
    }

    /// Convenience: build a Local location from an absolute path and the current
    /// machine's identity (user_id and machine_id come from app config).
    pub fn local(user_id: Uuid, machine_id: Uuid, path: impl Into<PathBuf>) -> Self {
        FileLocation::Local {
            user_id,
            machine_id,
            path: path.into(),
        }
    }
}

impl fmt::Display for FileLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_uri())
    }
}

// ── Internal URI parsers ────────────────────────────────────────────────────

/// Parse the part after `crisp+local://`
/// Format: `{user-uuid}@{machine-uuid}/{absolute-path}`
fn parse_local(rest: &str) -> anyhow::Result<FileLocation> {
    let (user_machine, path_str) = split_authority_path(rest)?;
    let (user_str, machine_str) = split_at(user_machine, '@')
        .ok_or_else(|| anyhow::anyhow!("crisp+local URI missing '@': {}", rest))?;
    let user_id = Uuid::parse_str(user_str)?;
    let machine_id = Uuid::parse_str(machine_str)?;
    let path = decode_path(path_str);
    Ok(FileLocation::Local {
        user_id,
        machine_id,
        path,
    })
}

/// Parse the part after `crisp+vps://`
/// Format: `{user-uuid}@{host}:{port}/{path}`
fn parse_vps(rest: &str) -> anyhow::Result<FileLocation> {
    let (user_hostport, path_str) = split_authority_path(rest)?;
    let (user_str, hostport) = split_at(user_hostport, '@')
        .ok_or_else(|| anyhow::anyhow!("crisp+vps URI missing '@': {}", rest))?;
    let user_id = Uuid::parse_str(user_str)?;
    // hostport = "host:port"
    let (host, port_str) = split_at(hostport, ':')
        .ok_or_else(|| anyhow::anyhow!("crisp+vps URI missing port: {}", rest))?;
    let port: u16 = port_str.parse()?;
    let path = format!("/{}", path_str);
    Ok(FileLocation::Vps {
        user_id,
        host: host.to_owned(),
        port,
        path,
    })
}

/// Parse the part after `crisp+internxt://`
/// Format: `{user-uuid}/{cloud-path}`
fn parse_internxt(rest: &str) -> anyhow::Result<FileLocation> {
    let (user_str, path_str) = split_authority_path(rest)?;
    let user_id = Uuid::parse_str(user_str)?;
    Ok(FileLocation::Internxt {
        user_id,
        cloud_path: format!("/{}", path_str),
    })
}

/// Parse the part after `crisp+drive://`
/// Format: `{drive-id}/{remote-path}` — the drive_id is opaque (typically
/// a UUID written by `DriveRegistry`); the remote_path is whatever the
/// drive's `CloudDrive::read_file` accepts.  Spaces are %20-decoded.
fn parse_drive(rest: &str) -> anyhow::Result<FileLocation> {
    let (drive_id, path_str) = split_authority_path(rest)?;
    Ok(FileLocation::Drive {
        drive_id: drive_id.to_owned(),
        remote_path: format!("/{}", path_str.replace("%20", " ")),
    })
}

/// Parse the part after `crisp+internxt-zip://`
/// Format: `{user-uuid}/{archive-cloud-path}#{internal-path}`
fn parse_internxt_zip(rest: &str) -> anyhow::Result<FileLocation> {
    // Fragment separator '#' appears somewhere after the authority.
    let (before_hash, internal_path) = rest
        .split_once('#')
        .ok_or_else(|| anyhow::anyhow!("crisp+internxt-zip URI missing '#': {}", rest))?;
    let (user_str, archive_path_str) = split_authority_path(before_hash)?;
    let user_id = Uuid::parse_str(user_str)?;
    Ok(FileLocation::InternxtZip {
        user_id,
        archive_cloud_path: format!("/{}", archive_path_str),
        internal_path: internal_path.to_owned(),
    })
}

// ── Small helpers ──────────────────────────────────────────────────────────

/// Split `{authority}/{path}` on the first `/` after the authority.
/// Returns `(authority, rest_without_leading_slash)`.
fn split_authority_path(s: &str) -> anyhow::Result<(&str, &str)> {
    s.split_once('/')
        .ok_or_else(|| anyhow::anyhow!("URI missing path component: {}", s))
}

/// Split a string on the first occurrence of `sep`.
fn split_at(s: &str, sep: char) -> Option<(&str, &str)> {
    s.split_once(sep)
}

/// Convert a URI path component back to a platform `PathBuf`.
/// On Windows we strip the leading `/` for drive-letter paths like `/C:/…`.
fn decode_path(s: &str) -> PathBuf {
    #[cfg(windows)]
    {
        // URI: /C:/Users/… → Windows path: C:\Users\…
        let stripped = s.strip_prefix('/').unwrap_or(s);
        // …but only when it *is* a Windows path. A `location_uri` written on
        // Linux or macOS carries a POSIX path, and those travel: a .cidx
        // archive or a cloud-backup manifest built there gets opened here.
        // Rewriting `/home/stc/docs/x.pdf` to `home\stc\docs\x.pdf` turned an
        // absolute path into a relative one pointing nowhere, silently.
        // A leading drive letter is what distinguishes the two.
        let b = stripped.as_bytes();
        let has_drive_letter = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
        if has_drive_letter {
            PathBuf::from(stripped.replace('/', "\\"))
        } else {
            PathBuf::from(format!("/{stripped}"))
        }
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(format!("/{}", s))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn uid() -> Uuid {
        Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap()
    }
    fn mid() -> Uuid {
        Uuid::parse_str("b2c3d4e5-f6a7-8901-bcde-f01234567891").unwrap()
    }

    #[test]
    fn local_roundtrip_unix() {
        let loc = FileLocation::Local {
            user_id: uid(),
            machine_id: mid(),
            path: PathBuf::from("/home/stc/docs/rahner.pdf"),
        };
        let uri = loc.to_uri();
        assert!(uri.starts_with("crisp+local://"));
        let parsed = FileLocation::from_uri(&uri).unwrap();
        assert_eq!(loc, parsed);
    }

    #[test]
    fn local_roundtrip_windows_drive_path() {
        // The other half of the pair: a drive-letter path must still come
        // back as a Windows path on Windows. Asserted on every platform via
        // the URI text, so the encode side cannot drift either.
        let loc = FileLocation::Local {
            user_id: uid(),
            machine_id: mid(),
            path: PathBuf::from(if cfg!(windows) {
                r"C:\Users\stc\docs\rahner.pdf"
            } else {
                "/C:/Users/stc/docs/rahner.pdf"
            }),
        };
        let uri = loc.to_uri();
        assert!(
            uri.ends_with("/C:/Users/stc/docs/rahner.pdf"),
            "drive path should encode with forward slashes: {uri}"
        );
        assert_eq!(loc, FileLocation::from_uri(&uri).unwrap());
    }

    #[test]
    fn vps_roundtrip() {
        let loc = FileLocation::Vps {
            user_id: uid(),
            host: "myserver.example.com".to_owned(),
            port: 8080,
            path: "/data/papers/doc.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.starts_with("crisp+vps://"));
        let parsed = FileLocation::from_uri(&uri).unwrap();
        assert_eq!(loc, parsed);
    }

    #[test]
    fn internxt_roundtrip() {
        let loc = FileLocation::Internxt {
            user_id: uid(),
            cloud_path: "/Backups/papers/rahner.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.starts_with("crisp+internxt://"));
        assert!(!uri.starts_with("crisp+internxt-zip://"));
        let parsed = FileLocation::from_uri(&uri).unwrap();
        assert_eq!(loc, parsed);
    }

    #[test]
    fn internxt_zip_roundtrip() {
        let loc = FileLocation::InternxtZip {
            user_id: uid(),
            archive_cloud_path: "/backups/zips/2024-01.7z".to_owned(),
            internal_path: "docs/rahner_geist.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.starts_with("crisp+internxt-zip://"));
        assert!(uri.contains('#'));
        let parsed = FileLocation::from_uri(&uri).unwrap();
        assert_eq!(loc, parsed);
    }

    #[test]
    fn retrieval_cost() {
        assert_eq!(
            FileLocation::Local {
                user_id: uid(),
                machine_id: mid(),
                path: PathBuf::from("/tmp/f")
            }
            .retrieval_cost(),
            RetrievalCost::Free
        );
        assert_eq!(
            FileLocation::Internxt {
                user_id: uid(),
                cloud_path: "/x".to_owned()
            }
            .retrieval_cost(),
            RetrievalCost::Expensive
        );
    }

    #[test]
    fn unknown_scheme_errors() {
        assert!(FileLocation::from_uri("http://example.com/file.pdf").is_err());
    }

    // ── P12 — crisp+cb-archive:// URI ─────────────────────────────────────

    #[test]
    fn cb_archive_uri_format() {
        let loc = FileLocation::CbArchive {
            archive_id: 42,
            file_hash: "deadbeefcafe1234".to_owned(),
            original_path: "/Users/me/docs/chapter.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.starts_with("crisp+cb-archive://"),
            "expected crisp+cb-archive scheme, got: {uri}");
        assert!(uri.contains("/42/"),               "archive_id missing: {uri}");
        assert!(uri.contains("deadbeefcafe1234"),   "hash missing: {uri}");
        assert!(uri.contains("#"),                  "fragment separator missing: {uri}");
    }

    #[test]
    fn cb_archive_filename_extracted_from_path() {
        let loc = FileLocation::CbArchive {
            archive_id: 1,
            file_hash: "abc".to_owned(),
            original_path: "/some/folder/Chapter 5.pdf".to_owned(),
        };
        assert_eq!(loc.filename().as_deref(), Some("Chapter 5.pdf"));
    }

    #[test]
    fn cb_archive_retrieval_cost_is_expensive() {
        let loc = FileLocation::CbArchive {
            archive_id: 1,
            file_hash: "x".to_owned(),
            original_path: "/x".to_owned(),
        };
        assert_eq!(loc.retrieval_cost(), RetrievalCost::Expensive);
    }

    #[test]
    fn cb_archive_user_id_is_nil() {
        // CbArchive doesn't carry user_id; it returns Uuid::nil() per design.
        let loc = FileLocation::CbArchive {
            archive_id: 1,
            file_hash: "x".to_owned(),
            original_path: "/x".to_owned(),
        };
        assert_eq!(loc.user_id(), Uuid::nil());
    }

    #[test]
    fn cb_archive_uri_encodes_spaces_in_path() {
        let loc = FileLocation::CbArchive {
            archive_id: 7,
            file_hash: "h".to_owned(),
            original_path: "/path with spaces/file.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.contains("%20"), "spaces must be encoded in URI: {uri}");
    }

    // ── crisp+drive:// — generic registry-backed file location ──────────────

    #[test]
    fn drive_uri_roundtrip_basic() {
        let loc = FileLocation::Drive {
            drive_id: "abc-123".to_owned(),
            remote_path: "/Documents/report.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.starts_with("crisp+drive://"), "wrong scheme: {uri}");
        assert!(uri.contains("/abc-123/"), "drive_id missing: {uri}");
        let parsed = FileLocation::from_uri(&uri).unwrap();
        assert_eq!(loc, parsed);
    }

    #[test]
    fn drive_uri_encodes_spaces() {
        let loc = FileLocation::Drive {
            drive_id: "d".to_owned(),
            remote_path: "/Photos/2024 holiday/img.jpg".to_owned(),
        };
        let uri = loc.to_uri();
        assert!(uri.contains("%20"), "spaces must be encoded: {uri}");
        // Round-trip survives decoding.
        let parsed = FileLocation::from_uri(&uri).unwrap();
        assert_eq!(parsed, loc);
    }

    #[test]
    fn drive_filename_extracted() {
        let loc = FileLocation::Drive {
            drive_id: "d".to_owned(),
            remote_path: "/folder/Chapter 5.pdf".to_owned(),
        };
        assert_eq!(loc.filename().as_deref(), Some("Chapter 5.pdf"));
    }

    #[test]
    fn drive_user_id_is_nil() {
        let loc = FileLocation::Drive {
            drive_id: "d".to_owned(),
            remote_path: "/x".to_owned(),
        };
        assert_eq!(loc.user_id(), Uuid::nil());
    }

    #[test]
    fn drive_retrieval_cost_conservative() {
        let loc = FileLocation::Drive {
            drive_id: "d".to_owned(),
            remote_path: "/x".to_owned(),
        };
        assert_eq!(loc.retrieval_cost(), RetrievalCost::Expensive);
    }

    #[test]
    fn drive_uri_normalises_leading_slash() {
        // Producer omitted the leading slash on remote_path; we normalise.
        let loc = FileLocation::Drive {
            drive_id: "d".to_owned(),
            remote_path: "Documents/file.pdf".to_owned(),
        };
        let uri = loc.to_uri();
        assert_eq!(uri, "crisp+drive://d/Documents/file.pdf");
    }
}
