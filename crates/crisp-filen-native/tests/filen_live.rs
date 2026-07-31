//! Opt-in cross-client checks.  These never contain credentials and skip when
//! the caller has not deliberately supplied both Rust login credentials and
//! the local Python CLI checkout.

use crisp_filen_native::{FilenNativeClient, DEFAULT_GATEWAY_URL};
use std::{env, fs, path::PathBuf, process::Command};

fn setup() -> Option<(FilenNativeClient, crisp_filen_native::FilenSession, PathBuf)> {
    let email = env::var("FILEN_EMAIL").ok()?;
    let password = env::var("FILEN_PASSWORD").ok()?;
    let cli = env::var("FILEN_PYTHON_CLI").unwrap_or_else(|_| "../filen-python/cli.py".into());
    if !PathBuf::from(&cli).exists() {
        eprintln!("skipping Filen live test: FILEN_PYTHON_CLI not found");
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
    Some((client, session, PathBuf::from(cli)))
}

#[test]
#[ignore = "requires FILEN_EMAIL/FILEN_PASSWORD and a configured Python CLI session"]
fn filen_live_rust_to_python() {
    let Some((client, session, cli)) = setup() else {
        eprintln!("skipping Filen live test: set FILEN_EMAIL and FILEN_PASSWORD");
        return;
    };
    let folder = format!("_crispsorter_live_{}", std::process::id());
    let folder_uuid = client
        .create_folder(&session.root_folder_uuid, &folder)
        .unwrap();
    let remote = format!("/{folder}/rust-to-python.txt");
    client
        .upload_file(
            &folder_uuid,
            "rust-to-python.txt",
            "text/plain",
            b"written by Rust",
        )
        .unwrap();
    let output = tempfile::tempdir().unwrap();
    let result = Command::new("python3")
        .arg(&cli)
        .arg("download-path")
        .arg(&remote)
        .arg("-t")
        .arg(output.path())
        .status()
        .unwrap();
    assert!(
        result.success(),
        "Python CLI could not download Rust upload"
    );
    let downloaded = output.path().join("rust-to-python.txt");
    assert_eq!(fs::read(downloaded).unwrap(), b"written by Rust");
    client.trash(&folder_uuid, "folder").unwrap();
}

#[test]
#[ignore = "requires FILEN_EMAIL/FILEN_PASSWORD and a configured Python CLI session"]
fn filen_live_python_to_rust() {
    let Some((client, session, cli)) = setup() else {
        eprintln!("skipping Filen live test: set FILEN_EMAIL and FILEN_PASSWORD");
        return;
    };
    let folder = format!("_crispsorter_live_{}", std::process::id());
    let folder_uuid = client
        .create_folder(&session.root_folder_uuid, &folder)
        .unwrap();
    let local = tempfile::NamedTempFile::new().unwrap();
    fs::write(local.path(), b"written by Python").unwrap();
    let result = Command::new("python3")
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
        .resolve_path(
            &session,
            std::path::Path::new(&format!(
                "/{folder}/{}",
                local.path().file_name().unwrap().to_string_lossy()
            )),
        )
        .unwrap();
    assert_eq!(client.download_file(&item).unwrap(), b"written by Python");
    client.trash(&folder_uuid, "folder").unwrap();
}
