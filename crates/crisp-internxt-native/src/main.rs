use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crisp_internxt_native::{
    InternxtNativeClient, InternxtSession, NativeItem, DEFAULT_DRIVE_API_URL,
};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "crisp-internxt",
    version,
    about = "Test the native Internxt Cloud Drive client"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Log in using a password read from stdin and write an explicit session file.
    Login {
        email: String,
        #[arg(long, default_value = DEFAULT_DRIVE_API_URL)]
        drive_api_url: String,
        #[arg(long)]
        tfa: Option<String>,
        #[arg(long, short)]
        session: PathBuf,
    },
    /// Refresh bearer tokens in an existing explicit session file.
    Refresh { session: PathBuf },
    /// List the contents of a remote folder path.
    List {
        session: PathBuf,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Download a remote file to a local path.
    Read {
        session: PathBuf,
        remote: PathBuf,
        out: PathBuf,
    },
    /// Upload a local file to a remote path.
    Write {
        session: PathBuf,
        local: PathBuf,
        remote: PathBuf,
    },
    /// Move a remote file or folder to trash.
    Delete { session: PathBuf, remote: PathBuf },
    /// Move a remote file or folder into another remote folder.
    Move {
        session: PathBuf,
        remote: PathBuf,
        destination: PathBuf,
    },
    /// Rename a remote file or folder in place.
    Rename {
        session: PathBuf,
        remote: PathBuf,
        name: String,
    },
    /// Print deterministic protocol vectors without contacting Internxt.
    CryptoVector,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Login {
            email,
            drive_api_url,
            tfa,
            session,
        } => {
            let mut password = String::new();
            io::stdin()
                .read_to_string(&mut password)
                .context("reading password from stdin")?;
            let password = password.trim_end_matches(['\r', '\n']);
            let value = InternxtNativeClient::login_without_keys(
                &drive_api_url,
                &email,
                password,
                tfa.as_deref(),
            )?;
            write_session(&session, &value)?;
            println!(
                "logged in as {}; session written to {}",
                value.email,
                session.display()
            );
        }
        Command::Refresh { session } => {
            let (client, value) = open(&session)?;
            let refreshed = client.refresh_session(&value)?;
            write_session(&session, &refreshed)?;
            println!("session refreshed: {}", session.display());
        }
        Command::List { session, path } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &path)?;
            anyhow::ensure!(
                item.is_dir,
                "remote path is not a folder: {}",
                path.display()
            );
            for entry in client.list_folder(&item.uuid)? {
                print_item(&entry);
            }
        }
        Command::Read {
            session,
            remote,
            out,
        } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &remote)?;
            anyhow::ensure!(
                !item.is_dir,
                "remote path is a folder: {}",
                remote.display()
            );
            client.download_file_to_path(&value, &item.uuid, &out)?;
            println!("downloaded {}", out.display());
        }
        Command::Write {
            session,
            local,
            remote,
        } => {
            let (client, value) = open(&session)?;
            let parent = remote.parent().unwrap_or_else(|| Path::new("."));
            let folder = client.resolve_path(&value, parent)?;
            anyhow::ensure!(
                folder.is_dir,
                "remote parent is not a folder: {}",
                parent.display()
            );
            let name = remote
                .file_name()
                .context("remote path has no file name")?
                .to_string_lossy();
            let (stem, ext) = split_name(&name);
            client.upload_path(&value, &folder.uuid, stem, ext, &local)?;
            println!("uploaded {}", remote.display());
        }
        Command::Delete { session, remote } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &remote)?;
            client.trash(&item.uuid, if item.is_dir { "folder" } else { "file" })?;
            println!("trashed {}", remote.display());
        }
        Command::Move {
            session,
            remote,
            destination,
        } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &remote)?;
            let target = client.resolve_path(&value, &destination)?;
            anyhow::ensure!(target.is_dir, "move destination is not a folder");
            if item.is_dir {
                client.move_folder(&item.uuid, &target.uuid)?;
            } else {
                client.move_file(&item.uuid, &target.uuid)?;
            }
            println!("moved {} into {}", remote.display(), destination.display());
        }
        Command::Rename {
            session,
            remote,
            name,
        } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &remote)?;
            if item.is_dir {
                client.rename_folder(&item.uuid, &name)?;
            } else {
                let (stem, ext) = split_name(&name);
                client.rename_file(&item.uuid, stem, ext)?;
            }
            println!("renamed {} to {}", remote.display(), name);
        }
        Command::CryptoVector => {
            let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
            let bucket = [0u8; 12];
            let index = [0x11u8; 32];
            println!(
                "{}",
                hex::encode(crisp_internxt_native::file_key(mnemonic, &bucket, &index))
            );
        }
    }
    Ok(())
}

fn open(path: &Path) -> Result<(InternxtNativeClient, InternxtSession)> {
    let value = InternxtSession::decode(
        &std::fs::read_to_string(path)
            .with_context(|| format!("reading session {}", path.display()))?,
    )?;
    let bearer = if value.new_token.is_empty() {
        &value.token
    } else {
        &value.new_token
    };
    let client = InternxtNativeClient::new(&value.drive_api_url, bearer)?;
    Ok((client, value))
}

fn write_session(path: &Path, value: &InternxtSession) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, value.encode()?)
        .with_context(|| format!("writing session {}", path.display()))
}

fn print_item(item: &NativeItem) {
    println!(
        "{}\t{}\t{}",
        if item.is_dir { "dir" } else { "file" },
        item.size,
        item.name
    );
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, "file"),
    }
}
