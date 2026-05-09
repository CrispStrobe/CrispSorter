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
        anyhow::bail!("Unknown crisp URI scheme: {}", s)
    }

    pub fn user_id(&self) -> Uuid {
        match self {
            FileLocation::Local { user_id, .. } => *user_id,
            FileLocation::Vps { user_id, .. } => *user_id,
            FileLocation::Internxt { user_id, .. } => *user_id,
            FileLocation::InternxtZip { user_id, .. } => *user_id,
            FileLocation::CbArchive { .. } => Uuid::nil(),
        }
    }

    pub fn retrieval_cost(&self) -> RetrievalCost {
        match self {
            FileLocation::Local { .. } => RetrievalCost::Free,
            FileLocation::Vps { .. } => RetrievalCost::Cheap,
            FileLocation::Internxt { .. } => RetrievalCost::Expensive,
            FileLocation::InternxtZip { .. } => RetrievalCost::Expensive,
            FileLocation::CbArchive { .. } => RetrievalCost::Expensive,
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
        PathBuf::from(stripped.replace('/', "\\"))
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
}
