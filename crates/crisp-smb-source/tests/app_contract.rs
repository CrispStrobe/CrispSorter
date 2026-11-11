//! Compile and exercise the actual application adapter, registry and CLI input.
//! Only the external secret vault and app-log sink use in-memory fixtures.
#![allow(dead_code, unused_imports)]
#[path = "../../../src-tauri/src/drives/filesystem.rs"]
mod filesystem;
#[path = "../../../src-tauri/src/drives/mounted_read.rs"]
mod mounted_read;
#[path = "../../../src-tauri/src/drives/materialize.rs"]
mod materialize;
#[path = "../../../src-tauri/src/operation_log.rs"]
mod operation_log;
#[path = "../../../src-tauri/src/drives/registry.rs"]
mod registry;
#[path = "../../../src-tauri/src/drives/smb.rs"]
mod smb;
#[path = "../../../src-tauri/src/cli/smb_options.rs"]
mod smb_options;
use crisp_smb_source::{Backend, Settings, CHUNK};
use filesystem::*;
use registry::*;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
static LOGS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
pub fn app_log(level: &str, message: String) {
    LOGS.lock().unwrap().push(format!("{level} {message}"));
}
pub mod drives {
    pub use super::{filesystem::*, registry::*};
    pub mod secret {
        use anyhow::Result;
        use serde::{Deserialize, Serialize};
        use std::{
            collections::HashMap,
            sync::{LazyLock, Mutex},
        };
        #[derive(Clone, Default, Serialize, Deserialize)]
        pub struct DriveCredentials {
            pub username: Option<String>,
            pub password: Option<String>,
            pub access_token: Option<String>,
            pub refresh_token: Option<String>,
            pub client_id: Option<String>,
        }
        static VAULT: LazyLock<Mutex<HashMap<String, DriveCredentials>>> =
            LazyLock::new(|| Mutex::new(HashMap::new()));
        pub fn get_credentials(id: &str) -> Result<Option<DriveCredentials>> {
            Ok(VAULT.lock().unwrap().get(id).cloned())
        }
        pub fn set_credentials(id: &str, value: &DriveCredentials) -> Result<()> {
            VAULT.lock().unwrap().insert(id.into(), value.clone());
            Ok(())
        }
        pub fn delete_credentials(id: &str) -> Result<()> {
            VAULT.lock().unwrap().remove(id);
            Ok(())
        }
    }
}
fn drive(root: &Path) -> smb::SmbDrive {
    smb::SmbDrive::new(
        "private-label".into(),
        "smb://private-server/share",
        Settings {
            mount_path: Some(root.to_str().unwrap().into()),
            ..Default::default()
        },
        "private-user",
        "private-password",
    )
    .unwrap()
}
fn contract(drive: &dyn CloudDrive, backend: Backend) {
    assert_eq!(drive.drive_type(), DriveType::Smb);
    assert!(drive.capabilities().streaming);
    assert!(!drive.capabilities().resumable_upload);
    assert!(drive.stat(Path::new("missing-contract-file")).is_err());
    drive.create_dir(Path::new("/contract/sub")).unwrap();
    let bytes: Vec<u8> = (0..CHUNK * 3 + 17).map(|n| (n % 251) as u8).collect();
    drive
        .write_file_from_reader(
            Path::new("/contract/sub/large.bin"),
            &mut Cursor::new(&bytes),
            bytes.len() as u64,
        )
        .unwrap();
    assert_eq!(
        drive
            .stat(Path::new("contract/sub/large.bin"))
            .unwrap()
            .size,
        bytes.len() as u64
    );
    assert_eq!(
        drive
            .read_range(Path::new("/contract/sub/large.bin"), 7, 38)
            .unwrap(),
        bytes[7..39]
    );
    let mut received = Vec::new();
    assert_eq!(
        drive
            .read_file_to_writer(Path::new("contract/sub/large.bin"), &mut received)
            .unwrap(),
        bytes.len() as u64
    );
    assert_eq!(received, bytes);
    assert!(drive
        .read_range(Path::new("contract/sub/large.bin"), 0, bytes.len() as u64)
        .is_err());
    assert!(drive
        .read_range(Path::new("contract/sub/large.bin"), u64::MAX, u64::MAX)
        .is_err());
    drive
        .move_path(
            Path::new("contract/sub/large.bin"),
            Path::new("contract/moved.bin"),
        )
        .unwrap();
    assert_eq!(
        drive.read_file(Path::new("contract/moved.bin")).unwrap(),
        bytes
    );
    assert!(drive
        .list_dir(Path::new("/"))
        .unwrap()
        .iter()
        .any(|e| e.name == "contract"));
    drive
        .write_file(Path::new("contract/empty.bin"), &[])
        .unwrap();
    assert!(drive
        .read_file(Path::new("contract/empty.bin"))
        .unwrap()
        .is_empty());
    // Reader size failures must close write handles and permit later operations.
    assert!(drive
        .write_file_from_reader(Path::new("contract/short.bin"), &mut Cursor::new([1, 2]), 3)
        .is_err());
    assert!(drive
        .write_file_from_reader(Path::new("contract/long.bin"), &mut Cursor::new([1, 2]), 1)
        .is_err());
    drive
        .write_file(Path::new("contract/after-error.bin"), &[7])
        .unwrap();
    assert_eq!(
        drive
            .read_file(Path::new("contract/after-error.bin"))
            .unwrap(),
        [7]
    );
    let remaining = drive.list_dir(Path::new("contract")).unwrap();
    for name in ["short.bin", "long.bin"] {
        let published = remaining.iter().any(|entry| entry.name == name);
        if backend == Backend::Mounted {
            assert!(!published, "failed mounted upload published {name}");
        } else if published {
            // Direct writes are not atomic; clean any partial file only after
            // proving the same session can still list, write and read.
            drive
                .delete(Path::new(&format!("contract/{name}")))
                .unwrap();
        }
    }
    drive.delete(Path::new("contract/after-error.bin")).unwrap();
    for bad in ["/../escape", "../escape", "//host/share", "C:/escape"] {
        assert!(drive.stat(Path::new(bad)).is_err());
    }
    assert!(drive.delete(Path::new("/")).is_err());
    for path in [
        "contract/empty.bin",
        "contract/moved.bin",
        "contract/sub",
        "contract",
    ] {
        drive.delete(Path::new(path)).unwrap();
    }
}
#[test]
fn mounted_adapter_contract_and_logs() {
    let root = tempfile::tempdir().unwrap();
    let (_, events) = operation_log::capture(|| contract(&drive(root.path()), Backend::Mounted));
    let logs = serde_json::to_string(&events).unwrap();
    for token in [
        "private-label",
        "private-server",
        "private-user",
        "private-password",
        "large.bin",
    ] {
        assert!(!logs.contains(token));
    }
    assert!(events.iter().any(|event| event.context.get("mode")
        == Some(&serde_json::json!("drives.smb.mounted"))));
    assert!(events.iter().any(|event| event.operation == "storage.smb.download" && event.phase == "completed"));
    assert!(events.iter().any(|event| event.operation == "storage.smb.stat" && event.phase == "failed"));
    assert!(events.iter().any(|event| event.operation == "storage.smb.upload"
        && event.stage.as_deref() == Some("transferred") && event.context.get("bytes").is_some()));
    for started in events.iter().filter(|event| event.phase == "started") {
        assert_eq!(events.iter().filter(|event| event.operation_id == started.operation_id
            && matches!(event.phase.as_str(), "completed" | "failed" | "cancelled")).count(), 1);
    }
    // The capture observes the real emitter, which also reaches the app sink.
    assert!(LOGS.lock().unwrap().iter().any(|line| line.contains(events[0].operation_id.as_str())));
}

#[test]
fn mounted_upload_failures_preserve_existing_file_and_remove_staging() {
    let root = tempfile::tempdir().unwrap();
    let target = Path::new("target.bin");
    std::fs::write(root.path().join(target), b"previous").unwrap();
    let d = drive(root.path());
    for (bytes, declared) in [(b"short".as_slice(), 20), (b"overlong".as_slice(), 3)] {
        assert!(d.write_file_from_reader(target, &mut Cursor::new(bytes), declared).is_err());
        assert_eq!(std::fs::read(root.path().join(target)).unwrap(), b"previous");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    struct FailedReader(bool);
    impl std::io::Read for FailedReader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.0 { return Err(std::io::Error::other("private-reader-error")); }
            self.0 = true;
            let count = bytes.len().min(3);
            bytes[..count].copy_from_slice(&b"new"[..count]);
            Ok(count)
        }
    }
    assert!(d.write_file_from_reader(target, &mut FailedReader(false), 10).is_err());
    assert_eq!(std::fs::read(root.path().join(target)).unwrap(), b"previous");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    d.write_file_from_reader(target, &mut Cursor::new(b"complete"), 8).unwrap();
    assert_eq!(std::fs::read(root.path().join(target)).unwrap(), b"complete");
}

#[test]
fn direct_write_failure_retains_cause_and_disables_automatic_replay() {
    let d = smb::SmbDrive::new("private-label".into(), "smb://unused.invalid/share",
        Settings { backend: Backend::Rust, ..Default::default() }, "reader", "private-password").unwrap();
    // Path validation fails before any connection, while exercising the real
    // direct-write error boundary rather than a fabricated provider response.
    let error = d.write_file(Path::new("../invalid"), b"bytes").unwrap_err();
    assert!(error.downcast_ref::<NonReplayableWrite>().is_some());
    assert!(error.to_string().contains("InvalidPath"));
    assert!(error.to_string().contains("automatic replay is disabled"));
}
#[test]
fn mounted_stream_stops_on_writer_failure_and_rejects_large_buffers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::File::create(root.path().join("large"))
        .unwrap()
        .set_len(65 * 1024 * 1024)
        .unwrap();
    let drive = drive(root.path());
    assert!(drive.read_file(Path::new("large")).is_err());
    struct Fail;
    impl Write for Fail {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("secret-local-path"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (result, events) = operation_log::capture(|| {
        drive.read_file_to_writer(Path::new("large"), &mut Fail)
    });
    assert!(result.unwrap_err().to_string().contains("secret-local-path"));
    assert!(!serde_json::to_string(&events).unwrap().contains("secret-local-path"));
}
#[cfg(unix)]
#[test]
fn mounted_paths_reject_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private"), b"private").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    let d = drive(root.path());
    assert!(d.read_file(Path::new("escape/private")).is_err());
    assert!(d.write_file(Path::new("escape/new"), b"bad").is_err());
    assert!(!outside.path().join("new").exists());
}
#[derive(clap::Parser)]
struct Parser {
    #[command(flatten)]
    options: smb_options::SmbOptions,
}
fn options(args: &[&str]) -> smb_options::SmbOptions {
    use clap::Parser as _;
    Parser::try_parse_from(std::iter::once("smb").chain(args.iter().copied()))
        .unwrap()
        .options
}
#[test]
fn cli_and_gui_registry_roundtrip_preserves_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("fresh").join("settings");
    assert!(!data_dir.exists());
    let mut r = DriveRegistry::open(&data_dir).unwrap();
    let added = options(&[
        "--label",
        "NAS",
        "--url",
        "smb://nas/books",
        "--backend",
        "mounted",
        "--mount-path",
        dir.path().to_str().unwrap(),
    ])
    .apply(&mut r, &mut Cursor::new([]))
    .unwrap();
    let rust = options(&[
        "--id",
        &added.id,
        "--backend",
        "rust",
        "--username",
        "reader",
        "--password-stdin",
    ])
    .apply(&mut r, &mut Cursor::new(b"private-password\n"))
    .unwrap();
    assert_eq!(rust.smb.as_ref().unwrap().backend, Backend::Rust);
    let json = std::fs::read_to_string(data_dir.join("drives.json")).unwrap();
    assert!(!json.contains("private-password"));
    assert!(!json.contains("reader"));
    // GUI calls this exact registry method and preserves credentials when blank.
    let mut gui = rust.clone();
    gui.smb.as_mut().unwrap().backend = Backend::Mounted;
    r.save_smb(gui, None, None).unwrap();
    let mut restored = DriveRegistry::open(&data_dir).unwrap();
    let viewed = options(&["--id", &added.id])
        .apply(&mut restored, &mut Cursor::new([]))
        .unwrap();
    assert_eq!(viewed.smb.unwrap().backend, Backend::Mounted);
    options(&["--id", &added.id, "--backend", "rust"])
        .apply(&mut restored, &mut Cursor::new([]))
        .unwrap();
    assert_eq!(
        drives::secret::get_credentials(&added.id)
            .unwrap()
            .unwrap()
            .password
            .as_deref(),
        Some("private-password")
    );
    let before = std::fs::read(data_dir.join("drives.json")).unwrap();
    assert!(options(&[
        "--id",
        &added.id,
        "--url",
        "smb://secret:password@nas/share"
    ])
    .apply(&mut restored, &mut Cursor::new([]))
    .is_err());
    assert!(options(&["--id", &added.id, "--password-stdin"])
        .apply(&mut restored, &mut Cursor::new(b"first\nsecond"))
        .is_err());
    assert_eq!(
        std::fs::read(data_dir.join("drives.json")).unwrap(),
        before
    );
}
#[test]
fn cli_parser_rejects_unknown_backend_and_misspelled_security() {
    use clap::Parser as _;
    assert!(Parser::try_parse_from(["smb", "--backend", "auto"]).is_err());
    assert!(Parser::try_parse_from(["smb", "--require-encryption", "maybe"]).is_err());
}
#[test]
fn settings_save_failure_restores_vault_and_registry() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = DriveRegistry::open(dir.path()).unwrap();
    let added = options(&[
        "--label",
        "NAS",
        "--url",
        "smb://nas/books",
        "--backend",
        "rust",
        "--username",
        "reader",
        "--password-stdin",
    ])
    .apply(&mut r, &mut Cursor::new(b"old\n"))
    .unwrap();
    std::fs::remove_file(dir.path().join("drives.json")).unwrap();
    std::fs::create_dir(dir.path().join("drives.json")).unwrap();
    assert!(options(&["--id", &added.id, "--password-stdin"])
        .apply(&mut r, &mut Cursor::new(b"new\n"))
        .is_err());
    assert_eq!(
        drives::secret::get_credentials(&added.id)
            .unwrap()
            .unwrap()
            .password
            .as_deref(),
        Some("old")
    );
    assert_eq!(r.drives.len(), 1);
}
#[tokio::test]
#[ignore = "Run scripts/test_smb_source.py with pinned shared fixture"]
async fn actual_rust_app_adapter_works_from_tauri_style_runtime() {
    let url = format!(
        "smb://{}/books",
        std::env::var("CRISP_SMB_TEST_ADDRESS").unwrap()
    );
    let settings = Settings {
        backend: Backend::Rust,
        mount_path: None,
        require_encryption: false,
    };
    let d = smb::SmbDrive::new(
        "private-label".into(),
        &url,
        settings.clone(),
        "reader",
        "fixture-password",
    )
    .unwrap();
    contract(&d, Backend::Rust);
    let strict = smb::SmbDrive::new(
        "private-label".into(),
        &url,
        Settings {
            require_encryption: true,
            ..settings
        },
        "reader",
        "fixture-password",
    )
    .unwrap();
    assert!(strict.list_dir(Path::new("/")).is_err()); // No fallback to an OS mount.
                                                       // Both runtime-backed objects are dropped inside Tokio, without a nested-runtime panic.
}
