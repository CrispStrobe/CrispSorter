use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crisp_filen_native::{FilenNativeClient, FilenSession, NativeItem, DEFAULT_GATEWAY_URL};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "crisp-filen", about = "Native Filen Cloud Drive client")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Login {
        email: String,
        #[arg(long, default_value = DEFAULT_GATEWAY_URL)]
        gateway_url: String,
        #[arg(long)]
        tfa: Option<String>,
        #[arg(long, short)]
        session: PathBuf,
    },
    List {
        session: PathBuf,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Read {
        session: PathBuf,
        remote: PathBuf,
        out: PathBuf,
    },
    Write {
        session: PathBuf,
        local: PathBuf,
        remote: PathBuf,
    },
    Delete {
        session: PathBuf,
        remote: PathBuf,
    },
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
            gateway_url,
            tfa,
            session,
        } => {
            let mut password = String::new();
            io::stdin().read_to_string(&mut password)?;
            let value = FilenNativeClient::login(
                &gateway_url,
                &email,
                password.trim_end_matches(['\r', '\n']),
                tfa.as_deref(),
            )?;
            write_session(&session, &value)?;
            println!(
                "logged in as {}; session written to {}",
                value.email,
                session.display()
            );
        }
        Command::List { session, path } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &path)?;
            anyhow::ensure!(item.is_dir, "remote path is not a folder");
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
            anyhow::ensure!(!item.is_dir, "remote path is a folder");
            std::fs::write(&out, client.download_file(&item)?)?;
        }
        Command::Write {
            session,
            local,
            remote,
        } => {
            let (client, value) = open(&session)?;
            let parent = remote.parent().unwrap_or_else(|| Path::new("."));
            let folder = client.resolve_path(&value, parent)?;
            let name = remote
                .file_name()
                .context("remote path has no filename")?
                .to_string_lossy();
            let data = std::fs::read(&local)?;
            client.upload_file(&folder.uuid, &name, "application/octet-stream", &data)?;
        }
        Command::Delete { session, remote } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &remote)?;
            client.trash(&item.uuid, if item.is_dir { "folder" } else { "file" })?;
        }
        Command::CryptoVector => {
            let (raw, password) = crisp_filen_native::pbkdf2_login("password", "salt");
            println!(
                "pbkdf2_raw={}\nauth_password={}",
                hex::encode(raw),
                password
            );
        }
    }
    Ok(())
}

fn open(path: &Path) -> Result<(FilenNativeClient, FilenSession)> {
    let value = FilenSession::decode(&std::fs::read_to_string(path)?)?;
    Ok((FilenNativeClient::from_session(&value)?, value))
}
fn write_session(path: &Path, value: &FilenSession) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, value.encode()?)?;
    Ok(())
}
fn print_item(item: &NativeItem) {
    println!(
        "{}\t{}\t{}",
        if item.is_dir { "dir" } else { "file" },
        item.size,
        item.name
    );
}
