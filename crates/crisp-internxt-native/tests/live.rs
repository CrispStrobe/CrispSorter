//! Explicit live tests against a real Internxt account.
//!
//! They are ignored by default because they mutate remote state and move real
//! bytes. Credentials are read from `INTERNXT_LOGIN`/`INTERNXT_PW`, optionally
//! loading the developer's external `../.env`; values are never printed.

use anyhow::{Context, Result};
use crisp_internxt::{InternxtNativeClient, InternxtSession, DEFAULT_DRIVE_API_URL};
use std::io::Write;
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

#[test]
#[ignore = "mutates a real Internxt account; run explicitly with --ignored"]
fn live_search_copy_and_update_round_trip() {
    let Some((email, password, tfa)) = credentials() else {
        eprintln!("live test skipped: INTERNXT_LOGIN/INTERNXT_PW not available");
        return;
    };
    run_search_copy_update(&email, &password, tfa.as_deref()).unwrap();
}

fn run_search_copy_update(email: &str, password: &str, tfa: Option<&str>) -> Result<()> {
    let session =
        InternxtNativeClient::login_without_keys(DEFAULT_DRIVE_API_URL, email, password, tfa)?;
    let client = InternxtNativeClient::new(&session.drive_api_url, &session.new_token)?;
    let source_name = unique_name("CrispSorter Rust lifecycle source");
    let target_name = unique_name("CrispSorter Rust lifecycle target");
    let source_uuid = client.create_folder(&session.root_folder_id, &source_name)?;
    let target_uuid = client.create_folder(&session.root_folder_id, &target_name)?;
    let source_path = std::env::temp_dir().join(unique_name("crispsorter-lifecycle-source"));
    let replacement_path =
        std::env::temp_dir().join(unique_name("crispsorter-lifecycle-replacement"));
    std::fs::write(&source_path, b"before")?;
    std::fs::write(&replacement_path, b"after")?;
    let result = (|| -> Result<()> {
        eprintln!("live lifecycle: upload");
        client.upload_path(&session, &source_uuid, "lifecycle", "txt", &source_path)?;
        eprintln!("live lifecycle: resolve");
        let item = client.resolve_path(&session, &Path::new(&source_name).join("lifecycle.txt"))?;
        eprintln!("live lifecycle: search");
        let matches =
            client.search_files_from(&session, Path::new(&source_name), "lifecycle.*", true, 1)?;
        assert!(matches.iter().any(|entry| entry.item.uuid == item.uuid));
        eprintln!("live lifecycle: copy");
        let copied = client.copy_file(&session, &item.uuid, &target_uuid, Some("copied"))?;
        assert_eq!(copied.name, "copied.txt");
        let skipped = client.copy_file_with_policy(
            &session,
            &item.uuid,
            &target_uuid,
            Some("copied"),
            crisp_internxt::ConflictPolicy::Skip,
        )?;
        assert_eq!(skipped.uuid, copied.uuid);
        let overwritten = client.copy_file_with_policy(
            &session,
            &item.uuid,
            &target_uuid,
            Some("copied"),
            crisp_internxt::ConflictPolicy::Overwrite,
        )?;
        assert_eq!(overwritten.name, "copied.txt");
        eprintln!("live lifecycle: update");
        client.update_file(&session, &item.uuid, &replacement_path)?;
        eprintln!("live lifecycle: download verification");
        let updated =
            client.resolve_path(&session, &Path::new(&source_name).join("lifecycle.txt"))?;
        let downloaded = std::env::temp_dir().join(unique_name("crispsorter-lifecycle-download"));
        client.download_file_to_path(&session, &updated.uuid, &downloaded)?;
        assert_eq!(std::fs::read(downloaded)?, b"after");
        let file_timestamp = client.set_file_timestamp(&updated.uuid, "2026-07-31T12:00:00.000Z");
        assert!(file_timestamp
            .as_ref()
            .err()
            .map(|error| error.to_string().contains("409"))
            .unwrap_or(false));
        let folder_timestamp =
            client.set_folder_timestamp(&source_uuid, "2026-07-31T12:00:00.000Z");
        assert!(
            folder_timestamp.is_ok()
                || folder_timestamp
                    .as_ref()
                    .err()
                    .map(|error| error.to_string().contains("409"))
                    .unwrap_or(false)
        );
        Ok(())
    })();
    let _ = std::fs::remove_file(source_path);
    let _ = std::fs::remove_file(replacement_path);
    let _ = client.trash(&source_uuid, "folder");
    let _ = client.trash(&target_uuid, "folder");
    result
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
        let local = std::env::temp_dir().join(unique_name("crispsorter-live-upload"));
        std::fs::write(&local, payload).context("writing small live source")?;
        client
            .upload_path(&session, &folder_uuid, "round-trip", "txt", &local)
            .context("uploading small live file")?;
        let path = Path::new(&folder_name).join("round-trip.txt");
        let item = client
            .resolve_path(&session, &path)
            .context("resolving upload")?;
        let downloaded = std::env::temp_dir().join(unique_name("crispsorter-live-download"));
        client
            .download_file_to_path(&session, &item.uuid, &downloaded)
            .context("downloading small live file")?;
        assert_eq!(
            std::fs::read(&downloaded).context("reading small live result")?,
            payload
        );
        let _ = std::fs::remove_file(local);
        let _ = std::fs::remove_file(downloaded);

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
        let moved_download = std::env::temp_dir().join(unique_name("crispsorter-live-moved"));
        client
            .download_file_to_path(&session, &moved.uuid, &moved_download)
            .context("downloading moved live file")?;
        assert_eq!(std::fs::read(&moved_download)?, payload);
        let _ = std::fs::remove_file(moved_download);

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
    let source = std::env::temp_dir().join(unique_name("crispsorter-multipart-source"));
    {
        let mut file = std::fs::File::create(&source)?;
        let mut chunk = vec![0u8; 1024 * 1024];
        for (index, byte) in chunk.iter_mut().enumerate() {
            *byte = (index as u64).wrapping_mul(31) as u8;
        }
        let mut remaining = LARGE_FILE_SIZE;
        while remaining > 0 {
            let length = remaining.min(chunk.len());
            file.write_all(&chunk[..length])?;
            remaining -= length;
        }
    }
    let state_path = std::env::temp_dir().join(unique_name("crispsorter-multipart-state"));
    client.upload_path_with_resume_state(
        session,
        folder_uuid,
        "multipart-round-trip",
        "bin",
        &source,
        &state_path,
    )?;
    assert!(client.load_upload_resume_state(&state_path)?.is_none());
    let path = Path::new(folder_name).join("multipart-round-trip.bin");
    let item = client.resolve_path(session, &path)?;
    let downloaded = std::env::temp_dir().join(unique_name("crispsorter-multipart-download"));
    client.download_file_to_path(session, &item.uuid, &downloaded)?;
    assert_eq!(std::fs::read(&downloaded)?, std::fs::read(&source)?);
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(downloaded);
    client.clear_upload_resume_state(&state_path);
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
