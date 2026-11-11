//! P11 Pillar 5 — Cloud drive abstraction.
//!
//! `trait CloudDrive` is the uniform interface for anything that can hold
//! files: a local/OS-mounted path (covers SMB / SFTP via mount), a Filen
//! folder, an Internxt drive, etc.
//!
//! First-cut implementations:
//!   * `LocalDrive`  — delegates to `std::fs`; covers local paths, NFS mounts,
//!     SMB-mounted shares (`/Volumes/…` on macOS, `\\server\share` on Windows).
//!
//! Future implementations:
//!   * `SftpDrive`     — direct SSH via `russh` or OS SFTP mount
//!
//! `DriveRegistry` holds the user-configured drives.  Each drive has a
//! stable `id` (UUID), a human label, and a type tag.  The registry is
//! serialised to `{data_dir}/drives.json` so it survives app restarts.

// The Python-CLI drives (`filen.rs` / `internxt.rs`) are gone: every release
// recipe already enabled the native clients, which meant the subprocess
// drives they replace were dead code in every shipped artifact. `desktop`
// now implies both native features, so there is no configuration left in
// which a Filen or Internxt transfer wants a Python interpreter.
#[cfg(feature = "drive-filen-native")]
pub mod filen_native_drive;
pub mod fuse_mount;
pub mod google_drive;
#[cfg(feature = "drive-internxt-native")]
pub mod internxt_native;
#[cfg(feature = "drive-internxt-native")]
pub mod internxt_native_drive;
pub(crate) mod oauth;
pub mod onedrive;
pub(crate) mod secret;
pub mod tauri_commands;
pub mod webdav;
mod mounted_read;

use anyhow::{anyhow, Result};
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;


/// Split out from the `cfg!` above so the wording is assertable on every
/// target. Left inline, only the *polarity* of the guard would be covered —
/// and only on whichever platform happens to be running the tests, which is
/// never the one the guard exists for.
#[cfg_attr(not(feature = "sidecars"), allow(dead_code))]
fn unsupported_drive_message(kind: &str) -> String {
    format!(
        "the {kind} drive needs to run a Python CLI as a subprocess, which \
         this build cannot do — iOS, Android and sandboxed (App Store) \
         builds all deny it. Use a build with the native {kind} client, or \
         reach the same storage over WebDAV"
    )
}

mod filesystem;
pub use filesystem::*;
pub mod smb;

mod registry;
pub use registry::{DriveConfig, DriveRegistry};

impl DriveRegistry {
    /// Instantiate a drive from its config using the default network policy.
    pub fn instantiate(config: &DriveConfig) -> Box<dyn CloudDrive> {
        Self::instantiate_with_proxy(config, &crate::sync::proxy::ProxyConfig::default())
            .expect("default cloud-drive HTTP client must build")
    }

    /// Instantiate a drive while applying the app-wide proxy policy.
    /// Providers without a native HTTP client (Local/Filen/Internxt CLI)
    /// retain their existing construction path.
    pub fn instantiate_with_proxy(
        config: &DriveConfig,
        proxy: &crate::sync::proxy::ProxyConfig,
    ) -> Result<Box<dyn CloudDrive>> {
        match config.kind {
            DriveType::Local | DriveType::Sftp => {
                // Local + raw SFTP both go through `LocalDrive` for now;
                // SFTP relies on the user mounting the share via OS/FUSE.
                Ok(Box::new(LocalDrive::new(
                    config.label.clone(),
                    PathBuf::from(&config.path),
                )))
            }
            DriveType::Smb => {
                let settings = config.smb.clone().unwrap_or_default();
                settings.validate(&config.path)?;
                let credentials = if settings.backend == crisp_smb_source::Backend::Rust {
                    secret::get_credentials(&config.id)?.unwrap_or_default()
                } else { secret::DriveCredentials::default() };
                Ok(Box::new(smb::SmbDrive::new(
                    config.label.clone(), &config.path, settings,
                    credentials.username.as_deref().or(config.username.as_deref()).unwrap_or(""),
                    credentials.password.as_deref().or(config.password.as_deref()).unwrap_or(""),
                    crate::app_log,
                )?))
            },
            DriveType::Filen => {
                #[cfg(feature = "drive-filen-native")]
                {
                    Ok(Box::new(
                        filen_native_drive::NativeFilenDrive::from_keychain_with_proxy(
                            config.label.clone(),
                            &config.id,
                            proxy,
                        ),
                    ))
                }
                #[cfg(not(feature = "drive-filen-native"))]
                {
                    Err(anyhow!(unsupported_drive_message("Filen")))
                }
            }
            DriveType::Internxt => {
                #[cfg(feature = "drive-internxt-native")]
                {
                    Ok(Box::new(
                        internxt_native_drive::NativeInternxtDrive::from_keychain_with_proxy(
                            config.label.clone(),
                            &config.id,
                            proxy,
                        ),
                    ))
                }
                #[cfg(not(feature = "drive-internxt-native"))]
                {
                    Err(anyhow!(unsupported_drive_message("Internxt")))
                }
            }
            DriveType::WebDav => {
                let credentials = crate::drives::secret::get_credentials(&config.id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Ok(Box::new(webdav::WebDavDrive::new_with_proxy(
                    config.label.clone(),
                    config.path.clone(),
                    credentials.username.or_else(|| config.username.clone()),
                    credentials.password.or_else(|| config.password.clone()),
                    config.insecure_tls.unwrap_or(false),
                    proxy,
                )?))
            }
            DriveType::OneDrive => {
                let credentials = crate::drives::secret::get_credentials(&config.id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Ok(Box::new(onedrive::OneDriveDrive::new_with_proxy(
                    config.label.clone(),
                    credentials
                        .access_token
                        .or_else(|| config.access_token.clone())
                        .unwrap_or_default(),
                    credentials
                        .refresh_token
                        .or_else(|| config.refresh_token.clone()),
                    credentials.client_id.or_else(|| config.client_id.clone()),
                    None,
                    proxy,
                )?))
            }
            DriveType::GoogleDrive => {
                let credentials = crate::drives::secret::get_credentials(&config.id)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Ok(Box::new(google_drive::GoogleDriveDrive::new_with_proxy(
                    config.label.clone(),
                    credentials
                        .access_token
                        .or_else(|| config.access_token.clone())
                        .unwrap_or_default(),
                    credentials
                        .refresh_token
                        .or_else(|| config.refresh_token.clone()),
                    credentials.client_id.or_else(|| config.client_id.clone()),
                    None,
                    proxy,
                )?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture() -> (tempfile::TempDir, LocalDrive) {
        let tmp = tempfile::tempdir().unwrap();
        let drive = LocalDrive::new("test", tmp.path().to_owned());
        (tmp, drive)
    }

    #[test]
    fn limited_walk_stops_before_enumerating_the_entire_tree() {
        let (_tmp, drive) = fixture();
        for name in ["a.txt", "b.txt", "c.txt"] {
            drive.write_file(Path::new(name), name.as_bytes()).unwrap();
        }
        let mut errors = |_path: &Path, _error: anyhow::Error| {};
        let rows = walk_limited(&drive, Path::new(""), Some(2), Some(2), &mut errors);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn content_search_matching_is_case_insensitive_and_bounded_by_query() {
        assert!(content_search_matches(b"Invoice TOTAL: 42", "total"));
        assert!(!content_search_matches(b"Invoice TOTAL: 42", "receipt"));
        assert!(!content_search_matches(b"anything", ""));
        assert!(content_search_matches(&[b'A', 0xff, b'B'], "a"));
    }

    struct CapabilityStub;

    impl CloudDrive for CapabilityStub {
        fn label(&self) -> &str {
            "capability-stub"
        }

        fn list_dir(&self, _path: &Path) -> Result<Vec<DirEntry>> {
            Ok(Vec::new())
        }

        fn read_file(&self, _path: &Path) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<()> {
            Ok(())
        }

        fn delete(&self, _path: &Path) -> Result<()> {
            Ok(())
        }

        fn stat(&self, _path: &Path) -> Result<FileStat> {
            Ok(FileStat {
                size: 0,
                is_dir: false,
                mtime_unix: None,
            })
        }

        fn drive_type(&self) -> DriveType {
            DriveType::WebDav
        }
    }

    #[test]
    fn local_drive_label_and_type() {
        let (_tmp, drive) = fixture();
        assert_eq!(drive.label(), "test");
        assert_eq!(drive.drive_type(), DriveType::Local);
    }

    #[test]
    fn drive_config_serialization_redacts_all_auth_fields() {
        let config = DriveConfig {
            id: "redaction".into(), label: "Redaction".into(), kind: DriveType::WebDav,
            path: "https://example.invalid/dav/".into(), username: Some("alice".into()),
            password: Some("password".into()), insecure_tls: Some(false), smb: None,
            access_token: Some("access-token".into()), refresh_token: Some("refresh-token".into()),
            client_id: Some("public-client".into()), client_secret: Some("must-not-ship".into()),
        };
        let json = serde_json::to_string(&config).unwrap();
        for secret in ["alice", "password", "access-token", "refresh-token", "public-client", "must-not-ship"] {
            assert!(!json.contains(secret), "serialized drive config leaked {secret}");
        }
        assert!(json.contains("redaction"));
    }

    #[test]
    fn local_drive_write_then_read_round_trips() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("hello.txt"), b"world").unwrap();
        let bytes = drive.read_file(Path::new("hello.txt")).unwrap();
        assert_eq!(bytes, b"world");
    }

    #[test]
    fn local_drive_streams_relative_and_absolute_paths() {
        let (tmp, drive) = fixture();
        drive.write_file(Path::new("mounted.bin"), b"share bytes").unwrap();
        for path in [PathBuf::from("mounted.bin"), tmp.path().join("mounted.bin")] {
            let mut output = Vec::new();
            assert_eq!(drive.read_file_to_writer(&path, &mut output).unwrap(), 11);
            assert_eq!(output, b"share bytes");
        }
    }

    #[test]
    fn streaming_facade_round_trips_with_legacy_provider_fallback() {
        let (_tmp, drive) = fixture();
        let mut input = Cursor::new(b"streamed".to_vec());
        drive
            .write_file_from_reader(Path::new("stream.txt"), &mut input, 8)
            .unwrap();
        let mut output = Vec::new();
        assert_eq!(
            drive
                .read_file_to_writer(Path::new("stream.txt"), &mut output)
                .unwrap(),
            8
        );
        assert_eq!(output, b"streamed");
    }

    #[test]
    fn legacy_resume_operations_fail_explicitly_and_do_not_probe_provider() {
        let (_tmp, drive) = fixture();
        let error = drive
            .upload_file_resumable(
                Path::new("local.bin"),
                Path::new("remote.bin"),
                Path::new("state.json"),
                1,
            )
            .expect_err("legacy local drive must reject resumable uploads");
        assert!(error
            .to_string()
            .contains("does not support durable resumable uploads"));

        let error = drive
            .download_file_resumable(
                Path::new("remote.bin"),
                Path::new("local.bin"),
                Path::new("state.json"),
            )
            .expect_err("legacy local drive must reject resumable downloads");
        assert!(error
            .to_string()
            .contains("does not support durable resumable downloads"));
    }

    #[test]
    fn default_unsupported_operations_match_the_capability_contract() {
        let drive = CapabilityStub;
        let capabilities = drive.capabilities();
        assert!(!capabilities.create_dir);
        assert!(!capabilities.rename);
        assert!(!capabilities.move_path);
        assert!(!capabilities.copy);
        assert!(!capabilities.share_links);
        assert!(!capabilities.versions);
        assert!(!capabilities.reversible_trash);

        assert!(drive.create_dir(Path::new("new")).is_err());
        assert!(drive
            .move_path(Path::new("old"), Path::new("new"))
            .is_err());
        assert!(drive
            .copy_path(Path::new("old"), Path::new("new"))
            .is_err());
        assert_eq!(drive.share_link(Path::new("file")).unwrap(), None);
        assert!(drive.list_versions(Path::new("file")).unwrap().is_empty());
        assert!(drive.restore_version(Path::new("file"), "v1").is_err());
        assert!(drive
            .restore_deleted(Path::new("trash/file"), None)
            .is_err());
    }

    #[test]
    fn streaming_contract_rejects_short_and_overlong_readers() {
        let (_tmp, drive) = fixture();
        let mut short = Cursor::new(b"short".to_vec());
        assert!(drive
            .write_file_from_reader(Path::new("short.txt"), &mut short, 6)
            .is_err());

        let mut overlong = Cursor::new(b"overlong".to_vec());
        assert!(drive
            .write_file_from_reader(Path::new("overlong.txt"), &mut overlong, 4)
            .is_err());
    }

    #[test]
    fn local_drive_creates_parent_dirs_on_write() {
        let (_tmp, drive) = fixture();
        drive
            .write_file(Path::new("nested/deep/file.txt"), b"x")
            .unwrap();
        assert_eq!(
            drive.read_file(Path::new("nested/deep/file.txt")).unwrap(),
            b"x"
        );
    }

    #[test]
    fn local_drive_list_dir_sorted() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("zzz.txt"), b"z").unwrap();
        drive.write_file(Path::new("aaa.txt"), b"a").unwrap();
        drive.write_file(Path::new("mmm/inner.txt"), b"m").unwrap();
        let entries = drive.list_dir(Path::new(".")).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "aaa.txt");
        assert_eq!(entries[1].name, "mmm");
        assert_eq!(entries[2].name, "zzz.txt");
        assert!(entries[1].is_dir);
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, Some(1));
        assert_eq!(entries[1].size, None); // dirs report no size
    }

    #[test]
    fn local_drive_stat_file_and_dir() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("a.txt"), b"hello").unwrap();
        let s = drive.stat(Path::new("a.txt")).unwrap();
        assert_eq!(s.size, 5);
        assert!(!s.is_dir);
        assert!(s.mtime_unix.is_some());

        drive.write_file(Path::new("subdir/x.txt"), b"x").unwrap();
        let d = drive.stat(Path::new("subdir")).unwrap();
        assert!(d.is_dir);
    }

    #[test]
    fn local_drive_delete_file_and_dir() {
        let (_tmp, drive) = fixture();
        drive.write_file(Path::new("doomed.txt"), b"!").unwrap();
        drive.delete(Path::new("doomed.txt")).unwrap();
        assert!(drive.stat(Path::new("doomed.txt")).is_err());
    }

    #[test]
    fn local_drive_read_missing_file_errors() {
        let (_tmp, drive) = fixture();
        assert!(drive.read_file(Path::new("does/not/exist")).is_err());
    }

    #[test]
    fn local_drive_capabilities_include_safe_mutations() {
        let (_tmp, drive) = fixture();
        let caps = drive.capabilities();
        assert!(caps.create_dir);
        assert!(caps.rename && caps.move_path && caps.copy);
        assert!(!caps.share_links);
    }

    #[test]
    fn provider_capability_matrix_is_explicit_without_network_or_keychain() {
        // Instantiate every non-native provider with inert configuration and
        // inspect only its declaration.  This is deliberately a pure contract
        // test: capability discovery must not spawn a CLI, make HTTP calls,
        // or consult credentials/keychain state.
        let cases = [
            (
                DriveType::Local,
                true,
                true,
                true,
                true,
                false,
                false,
                false,
                false,
            ),
            (
                DriveType::Filen,
                true,
                true,
                true,
                true,
                false,
                false,
                cfg!(feature = "drive-filen-native"),
                cfg!(feature = "drive-filen-native"),
            ),
            (
                DriveType::Internxt,
                true,
                true,
                true,
                // copy: NOT native-only. `InternxtDrive::copy_path` shells out
                // to `internxt cp` on the CLI path, and `capabilities()`
                // declares `copy: true` unconditionally — so gating the
                // expectation on the native feature made this row disagree with
                // the driver in exactly the default build. Same stale-matrix bug
                // as the OneDrive row below; streaming and resumable transfers
                // below genuinely are native-only and stay gated.
                true,
                false,
                false,
                cfg!(feature = "drive-internxt-native"),
                cfg!(feature = "drive-internxt-native"),
            ),
            (
                DriveType::WebDav,
                true,
                true,
                true,
                true,
                false,
                false,
                true,
                false,
            ),
            (
                DriveType::OneDrive,
                true,
                true,
                true,
                // copy: Graph exposes an async copy endpoint and
                // `OneDriveDrive::copy_path` implements it, so the driver
                // reports `copy: true` — as does its own unit test
                // `capabilities_include_graph_mutations_and_copy`. This row said
                // `false` and had been failing since the driver gained copy;
                // nobody saw it because the lib test target did not compile.
                true,
                true,
                true,
                false,
                false,
            ),
            (
                DriveType::GoogleDrive,
                true,
                true,
                true,
                true,
                true,
                true,
                false,
                false,
            ),
        ];

        for (
            kind,
            create_dir,
            rename,
            move_path,
            copy,
            share_links,
            versions,
            streaming,
            resumable,
        ) in cases
        {
            let config = DriveConfig {
                id: format!("capability-{kind:?}"),
                label: format!("capability-{kind:?}"),
                kind: kind.clone(),
                path: "https://example.invalid/drive/".to_owned(),
                username: None,
                password: None,
                insecure_tls: None, smb: None,
                access_token: Some("unit-test-token".to_owned()),
                refresh_token: None,
                client_id: None,
                client_secret: None,
            };
            let result = DriveRegistry::instantiate_with_proxy(&config, &Default::default());
            let unavailable = (matches!(kind, DriveType::Filen) && !cfg!(feature = "drive-filen-native"))
                || (matches!(kind, DriveType::Internxt) && !cfg!(feature = "drive-internxt-native"));
            if unavailable {
                assert!(result.is_err(), "{kind:?} must report its missing native provider");
                continue;
            }
            let caps = result.expect("compiled provider must instantiate").capabilities();
            assert_eq!(caps.create_dir, create_dir, "{kind:?} create_dir");
            assert_eq!(caps.rename, rename, "{kind:?} rename");
            assert_eq!(caps.move_path, move_path, "{kind:?} move_path");
            assert_eq!(caps.copy, copy, "{kind:?} copy");
            assert_eq!(caps.share_links, share_links, "{kind:?} share_links");
            assert_eq!(caps.versions, versions, "{kind:?} versions");
            assert_eq!(caps.streaming, streaming, "{kind:?} streaming");
            assert_eq!(
                caps.resumable_upload, resumable,
                "{kind:?} resumable upload"
            );
            assert_eq!(
                caps.resumable_download, resumable,
                "{kind:?} resumable download"
            );
        }
    }

    #[test]
    fn local_drive_mutations_cover_directory_move_and_recursive_copy() {
        let (_tmp, drive) = fixture();
        drive.create_dir(Path::new("source/nested")).unwrap();
        drive
            .write_file(Path::new("source/nested/file.txt"), b"payload")
            .unwrap();
        drive
            .copy_path(Path::new("source"), Path::new("copied"))
            .unwrap();
        assert_eq!(
            drive
                .read_file(Path::new("copied/nested/file.txt"))
                .unwrap(),
            b"payload"
        );
        drive
            .move_path(Path::new("copied"), Path::new("moved"))
            .unwrap();
        assert_eq!(
            drive.read_file(Path::new("moved/nested/file.txt")).unwrap(),
            b"payload"
        );
        assert!(drive.stat(Path::new("copied")).is_err());
    }

    #[test]
    fn drive_registry_persists_across_open_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = DriveConfig {
            id: "abc-123".to_owned(),
            label: "Backup SSD".to_owned(),
            kind: DriveType::Local,
            path: "/Volumes/Backup".to_owned(),
            username: None,
            password: None,
            insecure_tls: None, smb: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        };
        {
            let mut reg = DriveRegistry::open(tmp.path()).unwrap();
            assert_eq!(reg.drives.len(), 0);
            reg.add(cfg.clone()).unwrap();
        }
        // Re-open and verify persistence.
        let reg = DriveRegistry::open(tmp.path()).unwrap();
        assert_eq!(reg.drives.len(), 1);
        assert_eq!(reg.drives[0].id, "abc-123");
        assert_eq!(reg.drives[0].label, "Backup SSD");
        assert_eq!(reg.drives[0].kind, DriveType::Local);
    }

    #[test]
    fn drive_registry_dedupes_by_id_on_add() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = DriveRegistry::open(tmp.path()).unwrap();
        reg.add(DriveConfig {
            id: "x".into(),
            label: "v1".into(),
            kind: DriveType::Local,
            path: "/a".into(),
            username: None,
            password: None,
            insecure_tls: None, smb: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        })
        .unwrap();
        // Same id, different label → must replace not duplicate.
        reg.add(DriveConfig {
            id: "x".into(),
            label: "v2".into(),
            kind: DriveType::Local,
            path: "/b".into(),
            username: None,
            password: None,
            insecure_tls: None, smb: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        })
        .unwrap();
        assert_eq!(reg.drives.len(), 1);
        assert_eq!(reg.drives[0].label, "v2");
        assert_eq!(reg.drives[0].path, "/b");
    }

    #[test]
    fn drive_registry_remove_returns_found_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = DriveRegistry::open(tmp.path()).unwrap();
        reg.add(DriveConfig {
            id: "abc".into(),
            label: "a".into(),
            kind: DriveType::Local,
            path: "/a".into(),
            username: None,
            password: None,
            insecure_tls: None, smb: None,
            access_token: None,
            refresh_token: None,
            client_id: None,
            client_secret: None,
        })
        .unwrap();
        assert!(reg.remove("abc").unwrap());
        assert!(!reg.remove("abc").unwrap()); // already gone
        assert!(!reg.remove("never-existed").unwrap());
        assert_eq!(reg.drives.len(), 0);
    }

    #[test]
    fn drive_type_serializes_snake_case() {
        let json = serde_json::to_string(&DriveType::Local).unwrap();
        assert_eq!(json, "\"local\"");
        let json = serde_json::to_string(&DriveType::Filen).unwrap();
        assert_eq!(json, "\"filen\"");
        // Round-trips.
        let back: DriveType = serde_json::from_str("\"internxt\"").unwrap();
        assert_eq!(back, DriveType::Internxt);
    }

    #[test]
    fn the_unsupported_drive_message_is_actionable() {
        // Runs on every target, unlike the polarity check below: names the
        // drive rather than leaving the user with a raw EPERM, says which
        // platforms are affected, and points at the way out.
        let msg = unsupported_drive_message("Filen");
        assert!(msg.contains("Filen"), "should name the drive: {msg}");
        assert!(
            msg.contains("iOS") && msg.contains("Android"),
            "should say which platforms cannot do this: {msg}"
        );
        assert!(
            msg.contains("WebDAV"),
            "should offer the alternative: {msg}"
        );
    }


    #[test]
    fn registry_instantiate_routes_each_kind_correctly() {
        // Local + Sftp both go through LocalDrive for now (Sftp relies on
        // OS-level mounts).  Filen / Internxt route to their own subprocess
        // drives.  This guards against future refactors silently swapping
        // the dispatch back to LocalDrive.
        for (kind, expected) in [
            (DriveType::Local, DriveType::Local),
            // Sftp piggybacks on LocalDrive (which only knows DriveType::Local),
            // so the instance reports Local even though the config said Sftp.
            (DriveType::Sftp, DriveType::Local),
            (DriveType::Filen, DriveType::Filen),
            (DriveType::Internxt, DriveType::Internxt),
            (DriveType::WebDav, DriveType::WebDav),
            (DriveType::OneDrive, DriveType::OneDrive),
            (DriveType::GoogleDrive, DriveType::GoogleDrive),
        ] {
            let path = if matches!(kind, DriveType::WebDav) {
                "https://example.com/dav/".to_owned()
            } else {
                "/tmp".to_owned()
            };
            let cfg = DriveConfig {
                id: "x".into(),
                label: "lbl".into(),
                kind: kind.clone(),
                path,
                username: None,
                password: None,
                insecure_tls: None, smb: None,
                access_token: None,
                refresh_token: None,
                client_id: None,
                client_secret: None,
            };
            let result = DriveRegistry::instantiate_with_proxy(&cfg, &Default::default());
            let unavailable = (matches!(kind, DriveType::Filen) && !cfg!(feature = "drive-filen-native"))
                || (matches!(kind, DriveType::Internxt) && !cfg!(feature = "drive-internxt-native"));
            if unavailable {
                assert!(result.is_err(), "{kind:?} must report its missing native provider");
                continue;
            }
            let drive = result.expect("compiled provider must instantiate");
            assert_eq!(
                drive.drive_type(),
                expected,
                "DriveType::{:?} should instantiate to a drive that reports drive_type() == {:?}",
                kind,
                expected
            );
            assert_eq!(drive.label(), "lbl");
        }
    }

    #[test]
    fn drive_type_label_covers_all_variants() {
        let labels = vec![
            (DriveType::Local, "Local"),
            (DriveType::Filen, "Filen"),
            (DriveType::Internxt, "Internxt"),
            (DriveType::Sftp, "SFTP"),
            (DriveType::WebDav, "WebDAV"),
            (DriveType::OneDrive, "OneDrive"),
            (DriveType::GoogleDrive, "Google Drive"),
        ];
        for (dt, expected) in labels {
            assert_eq!(dt.label(), expected);
        }
    }

    #[test]
    fn file_version_serde_round_trips() {
        let v = FileVersion {
            id: "v123".into(),
            modified_at: Some(1720000000),
            size: Some(12345),
            modifier_name: Some("Alice".into()),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: FileVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "v123");
        assert_eq!(back.modified_at, Some(1720000000));
        assert_eq!(back.size, Some(12345));
        assert_eq!(back.modifier_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn local_drive_default_version_methods() {
        let (_tmp, drive) = fixture();
        // Default implementations should return empty/error.
        let versions = drive.list_versions(Path::new("any")).unwrap();
        assert!(versions.is_empty());
        assert!(drive.restore_version(Path::new("any"), "v1").is_err());
        assert!(drive.share_link(Path::new("any")).unwrap().is_none());
    }
}
