//! Explicit live tests against a real Internxt account.
//!
//! They are ignored by default because they mutate remote state and move real
//! bytes. Credentials are read from `INTERNXT_LOGIN`/`INTERNXT_PW`, optionally
//! loading the developer's external `../.env`; values are never printed.

use anyhow::{Context, Result};
use crisp_internxt_native::{InternxtNativeClient, InternxtSession, DEFAULT_DRIVE_API_URL};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const LARGE_FILE_SIZE: usize = 100 * 1024 * 1024 + 1;

#[test]
#[ignore = "mutates a real Internxt account; run explicitly with --ignored"]
fn live_login_list_refresh_and_file_mutations() {
    let Some((email, password, tfa)) = credentials() else {
        eprintln!("live test skipped: INTERNXT_LOGIN/INTERNXT_PW not available");
        return;
    };
    run_small_round_trip(&email, &password, tfa.as_deref()).unwrap();
}

#[test]
#[ignore = "uploads and downloads 100 MiB; run explicitly with --ignored"]
fn live_multipart_upload_download_round_trip() {
    let Some((email, password, tfa)) = credentials() else {
        eprintln!("live test skipped: INTERNXT_LOGIN/INTERNXT_PW not available");
        return;
    };
    let session = InternxtNativeClient::login_without_keys(
        DEFAULT_DRIVE_API_URL,
        &email,
        &password,
        tfa.as_deref(),
    )
    .unwrap();
    let client = InternxtNativeClient::new(&session.drive_api_url, &session.new_token).unwrap();
    let folder_name = unique_name("CrispSorter Rust multipart");
    let folder_uuid = client
        .create_folder(&session.root_folder_id, &folder_name)
        .unwrap();
    let result = run_multipart(&client, &session, &folder_uuid, &folder_name);
    let _ = client.trash(&folder_uuid, "folder");
    result.unwrap();
}

fn run_small_round_trip(email: &str, password: &str, tfa: Option<&str>) -> Result<()> {
    let session =
        InternxtNativeClient::login_without_keys(DEFAULT_DRIVE_API_URL, email, password, tfa)?;
    let client = InternxtNativeClient::new(&session.drive_api_url, &session.new_token)?;
    let root = client
        .list_folder(&session.root_folder_id)
        .context("listing root")?;
    assert!(root.iter().all(|item| !item.uuid.is_empty()));

    let folder_name = unique_name("CrispSorter Rust live");
    let moved_name = format!("{folder_name} moved");
    let folder_uuid = client
        .create_folder(&session.root_folder_id, &folder_name)
        .context("creating first live folder")?;
    let moved_uuid = client
        .create_folder(&session.root_folder_id, &moved_name)
        .context("creating second live folder")?;
    let result = (|| -> Result<()> {
        let payload = b"CrispSorter native Internxt live round-trip\n\xE2\x9C\x93";
        client
            .upload_file(&session, &folder_uuid, "round-trip", "txt", payload)
            .context("uploading small live file")?;
        let path = Path::new(&folder_name).join("round-trip.txt");
        let item = client
            .resolve_path(&session, &path)
            .context("resolving upload")?;
        assert_eq!(
            client
                .download_file(&session, &item.uuid)
                .context("downloading small live file")?,
            payload
        );

        client
            .rename_file(&item.uuid, "renamed", "txt")
            .context("renaming live file")?;
        let renamed = client
            .resolve_path(&session, &Path::new(&folder_name).join("renamed.txt"))
            .context("resolving renamed live file")?;
        client
            .move_file(&renamed.uuid, &moved_uuid)
            .context("moving live file")?;
        let moved = client
            .resolve_path(&session, &Path::new(&moved_name).join("renamed.txt"))
            .context("resolving moved live file")?;
        assert_eq!(
            client
                .download_file(&session, &moved.uuid)
                .context("downloading moved live file")?,
            payload
        );

        let refreshed = client
            .refresh_session(&session)
            .context("refreshing live session")?;
        assert!(!refreshed.token.is_empty());
        assert!(!refreshed.new_token.is_empty());
        Ok(())
    })();
    let _ = client.trash(&folder_uuid, "folder");
    let _ = client.trash(&moved_uuid, "folder");
    result
}

fn run_multipart(
    client: &InternxtNativeClient,
    session: &InternxtSession,
    folder_uuid: &str,
    folder_name: &str,
) -> Result<()> {
    let mut payload = vec![0u8; LARGE_FILE_SIZE];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u64).wrapping_mul(31) as u8;
    }
    client.upload_file(
        session,
        folder_uuid,
        "multipart-round-trip",
        "bin",
        &payload,
    )?;
    let path = Path::new(folder_name).join("multipart-round-trip.bin");
    let item = client.resolve_path(session, &path)?;
    let downloaded = client.download_file(session, &item.uuid)?;
    assert_eq!(downloaded.len(), payload.len());
    assert_eq!(downloaded, payload);
    Ok(())
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    format!("{prefix} {nanos}")
}

fn credentials() -> Option<(String, String, Option<String>)> {
    let mut values = std::collections::HashMap::new();
    for path in [
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.env"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env"),
    ] {
        load_env_file(&path, &mut values);
    }
    let get = |name: &str| {
        std::env::var(name)
            .ok()
            .or_else(|| values.get(name).cloned())
    };
    let email = get("INTERNXT_LOGIN")?;
    let password = get("INTERNXT_PW")?;
    let tfa = get("INTERNXT_TFA").or_else(|| get("IXT_2FA"));
    Some((email, password, tfa))
}

fn load_env_file(path: &Path, values: &mut std::collections::HashMap<String, String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().is_empty() || key.trim_start().starts_with('#') {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']);
        values.insert(key.trim().to_owned(), value.to_owned());
    }
}
