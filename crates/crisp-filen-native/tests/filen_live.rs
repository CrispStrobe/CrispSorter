//! Opt-in cross-client checks.  These never contain credentials and skip when
//! the caller has not deliberately supplied both Rust login credentials and
//! the local Python CLI checkout.

use crisp_filen::{FilenNativeClient, DEFAULT_GATEWAY_URL};
use std::{env, fs, path::PathBuf, process::Command};

fn setup() -> Option<(
    FilenNativeClient,
    crisp_filen::FilenSession,
    PathBuf,
    PathBuf,
)> {
    let email = env::var("FILEN_EMAIL")
        .ok()
        .filter(|value| !value.is_empty())?;
    let password = env::var("FILEN_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty())?;
    let cli = env::var("FILEN_PYTHON_CLI")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../filen-python/cli.py")
        });
    if !cli.exists() {
        eprintln!("skipping Filen live test: FILEN_PYTHON_CLI not found");
        return None;
    }
    let python = env::var("FILEN_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../filen-python/.venv/bin/python")
        });
    if !python.exists()
        || !Command::new(&python)
            .args(["-c", "import requests"])
            .status()
            .ok()?
            .success()
    {
        eprintln!("skipping Filen live test: Python CLI dependencies are unavailable");
        return None;
    }
    let session = match FilenNativeClient::login(
        DEFAULT_GATEWAY_URL,
        &email,
        &password,
        env::var("FILEN_TFA").ok().as_deref(),
    ) {
        Ok(value) => value,
        Err(error) => panic!("Filen live login failed: {error:#}"),
    };
    let client = FilenNativeClient::from_session(&session).unwrap();
    Some((client, session, cli, python))
}

#[test]
#[ignore = "requires FILEN_EMAIL/FILEN_PASSWORD and a configured Python CLI session"]
fn filen_live_rust_to_python() {
    let Some((client, session, cli, python)) = setup() else {
        eprintln!("skipping Filen live test: set FILEN_EMAIL and FILEN_PASSWORD");
        return;
    };
    let folder = format!("_crispsorter_live_{}_rust_to_python", std::process::id());
    let folder_uuid = client
        .create_folder(&session.root_folder_uuid, &folder)
        .unwrap();
    eprintln!("native mutation: created folder");
    let remote = format!("/{folder}/rust-to-python.txt");
    client
        .upload_file(
            &folder_uuid,
            "rust-to-python.txt",
            "text/plain",
            b"written by Rust",
        )
        .unwrap();
    let item = client
        .resolve_path(&session, std::path::Path::new(&remote))
        .unwrap();
    let output = tempfile::tempdir().unwrap();
    let result = Command::new(python)
        .arg(&cli)
        .arg("download")
        .arg(&item.uuid)
        .arg("-o")
        .arg(output.path().join("rust-to-python.txt"))
        .status()
        .unwrap();
    assert!(
        result.success(),
        "Python CLI could not download Rust upload"
    );
    assert_eq!(
        fs::read(output.path().join("rust-to-python.txt")).unwrap(),
        b"written by Rust"
    );
    client.trash(&folder_uuid, "folder").unwrap();
}

#[test]
#[ignore = "requires FILEN_EMAIL/FILEN_PASSWORD and a configured Python CLI session"]
fn filen_live_python_to_rust() {
    let Some((client, session, cli, python)) = setup() else {
        eprintln!("skipping Filen live test: set FILEN_EMAIL and FILEN_PASSWORD");
        return;
    };
    let folder = format!("_crispsorter_live_{}_python_to_rust", std::process::id());
    let folder_uuid = client
        .create_folder(&session.root_folder_uuid, &folder)
        .unwrap();
    let local = tempfile::NamedTempFile::new().unwrap();
    fs::write(local.path(), b"written by Python").unwrap();
    let result = Command::new(python)
        .arg(&cli)
        .arg("upload")
        .arg(local.path())
        .arg("-t")
        .arg(format!("/{folder}"))
        .status()
        .unwrap();
    assert!(
        result.success(),
        "Python CLI could not upload reverse-roundtrip fixture"
    );
    let item = client
        .list_folder(&folder_uuid)
        .unwrap()
        .into_iter()
        .find(|item| !item.is_dir)
        .expect("Python upload should create a file");
    assert_eq!(client.download_file(&item).unwrap(), b"written by Python");
    client.trash(&folder_uuid, "folder").unwrap();
}

#[test]
#[ignore = "requires FILEN_EMAIL/FILEN_PASSWORD and a configured Python CLI session"]
fn filen_live_native_mutations() {
    let Some((client, session, _cli, _python)) = setup() else {
        eprintln!("skipping Filen live test: set FILEN_EMAIL and FILEN_PASSWORD");
        return;
    };
    let folder = format!("_crispsorter_live_{}_mutations", std::process::id());
    let folder_uuid = client
        .create_folder(&session.root_folder_uuid, &folder)
        .unwrap();
    let nested_uuid = client.create_folder(&folder_uuid, "nested").unwrap();
    let local_tree = tempfile::tempdir().unwrap();
    std::fs::create_dir(local_tree.path().join("subdir")).unwrap();
    std::fs::write(
        local_tree.path().join("subdir").join("path.txt"),
        b"path transfer fixture",
    )
    .unwrap();
    let expected_modified = 1_700_000_123_000i64;
    std::fs::File::open(local_tree.path().join("subdir").join("path.txt"))
        .unwrap()
        .set_modified(
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(expected_modified as u64),
        )
        .unwrap();
    client
        .upload_path_with_timestamps(
            &folder_uuid,
            "path-tree",
            "text/plain",
            local_tree.path(),
            true,
        )
        .unwrap();
    let path_root = client
        .resolve_path(
            &session,
            std::path::Path::new(&format!("/{folder}/path-tree")),
        )
        .unwrap();
    let local_download = tempfile::tempdir().unwrap();
    let downloaded_tree = local_download.path().join("path-tree");
    client
        .download_path_with_timestamps(&path_root, &downloaded_tree, true)
        .unwrap();
    let downloaded_file = downloaded_tree.join("subdir/path.txt");
    assert_eq!(
        fs::read(&downloaded_file).unwrap(),
        b"path transfer fixture"
    );
    let downloaded_modified = fs::metadata(downloaded_file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!((downloaded_modified - expected_modified).abs() < 5_000);
    let mut resumable = client
        .begin_upload(&folder_uuid, "before.txt", "text/plain", 16)
        .unwrap();
    client
        .resume_upload_from_reader(&mut resumable, std::io::Cursor::new(b"mutation fixture"))
        .unwrap();
    let before = client
        .resolve_path(
            &session,
            std::path::Path::new(&format!("/{folder}/before.txt")),
        )
        .unwrap();
    eprintln!("native mutation: uploaded and resolved");
    client.rename_item(&before, "after.txt").unwrap();
    let after = client
        .resolve_path(
            &session,
            std::path::Path::new(&format!("/{folder}/after.txt")),
        )
        .unwrap();
    eprintln!("native mutation: renamed");
    let matches = client
        .search(&session, &format!("{folder}/**/*.txt"))
        .unwrap();
    assert!(matches.iter().any(|item| item.uuid == after.uuid));
    eprintln!("native mutation: searched");
    client
        .update_timestamps(&after, 1_700_000_000_000, 1_700_000_001_000)
        .unwrap();
    eprintln!("native mutation: timestamps");
    client
        .replace_file(&after, "text/plain", b"replaced contents")
        .unwrap();
    eprintln!("native mutation: replaced");
    let replaced = client
        .resolve_path(
            &session,
            std::path::Path::new(&format!("/{folder}/after.txt")),
        )
        .unwrap();
    assert!(client.file_exists(&folder_uuid, "after.txt").unwrap());
    assert!(client
        .get_flat_folder_tree(&session.root_folder_uuid)
        .is_ok());
    assert_eq!(
        client.download_file(&replaced).unwrap(),
        b"replaced contents"
    );
    assert_eq!(
        client.download_file_range(&replaced, 2, 7).unwrap(),
        b"placed "
    );
    assert_eq!(
        client.download_files(vec![replaced.clone()]).unwrap()[0],
        b"replaced contents"
    );
    eprintln!("native mutation: verified replace");
    client.copy_file(&replaced, &nested_uuid).unwrap();
    eprintln!("native mutation: copied file");
    let copied = client
        .list_folder(&nested_uuid)
        .unwrap()
        .into_iter()
        .find(|item| !item.is_dir)
        .expect("copy should create a file");
    client
        .move_item(&copied.uuid, &session.root_folder_uuid, false)
        .unwrap();
    client.delete_permanent(&copied.uuid, false).unwrap();
    eprintln!("native mutation: deleted copied file");
    let copied_folder = client
        .copy_item(
            &client
                .resolve_path(&session, std::path::Path::new(&format!("/{folder}/nested")))
                .unwrap(),
            &session.root_folder_uuid,
        )
        .unwrap();
    eprintln!("native mutation: copied folder");
    client.trash(&copied_folder, "folder").unwrap();
    client.trash(&folder_uuid, "folder").unwrap();
    assert!(client
        .list_trash()
        .unwrap()
        .iter()
        .any(|item| item.uuid == folder_uuid));
    client.restore(&folder_uuid, "folder").unwrap();
    client.trash(&folder_uuid, "folder").unwrap();
}
