use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crisp_internxt::{
    inspect_local_directory, ConflictPolicy, InternxtNativeClient, InternxtSession, NativeItem,
    TransferFilter, TransferOptions, DEFAULT_DRIVE_API_URL,
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
        #[arg(long)]
        resume_state: Option<PathBuf>,
        /// Print transfer stages and multipart diagnostics.
        #[arg(short, long)]
        verbose: bool,
        /// Concurrent multipart PUT workers; 1 is the gateway-safe default.
        #[arg(long, default_value_t = 1)]
        multipart_workers: usize,
    },
    /// Recursively upload a local directory into a remote folder.
    WriteTree {
        session: PathBuf,
        local: PathBuf,
        remote: PathBuf,
        #[arg(long, default_value = "fail")]
        on_conflict: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        preserve_timestamps: bool,
        #[arg(long)]
        skip_unchanged: bool,
        #[arg(long = "include")]
        includes: Vec<String>,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
    },
    /// Recursively download a remote folder into a local directory.
    ReadTree {
        session: PathBuf,
        remote: PathBuf,
        out: PathBuf,
        #[arg(long, default_value = "fail")]
        on_conflict: String,
        #[arg(long = "include")]
        includes: Vec<String>,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        #[arg(long)]
        preserve_timestamps: bool,
    },
    /// Search recursively using a shell-style filename pattern.
    Search {
        session: PathBuf,
        pattern: String,
        #[arg(long, default_value = ".")]
        folder: PathBuf,
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long, default_value_t = -1)]
        max_depth: isize,
    },
    /// Copy a remote file or folder into another remote folder.
    Copy {
        session: PathBuf,
        remote: PathBuf,
        destination: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "fail")]
        on_conflict: String,
    },
    /// Replace a remote file's content from a local path.
    Update {
        session: PathBuf,
        remote: PathBuf,
        local: PathBuf,
    },
    /// Set a remote file or folder modification timestamp.
    Touch {
        session: PathBuf,
        remote: PathBuf,
        timestamp: String,
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
    /// List items currently in trash.
    TrashList {
        session: PathBuf,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Restore an item from trash into a destination folder.
    Restore {
        session: PathBuf,
        uuid: String,
        kind: String,
        destination: PathBuf,
    },
    /// Permanently delete one item from trash.
    Purge {
        session: PathBuf,
        uuid: String,
        kind: String,
        #[arg(long)]
        force: bool,
    },
    /// Permanently empty the entire trash.
    EmptyTrash {
        session: PathBuf,
        #[arg(long)]
        force: bool,
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
            for entry in client.list_folder_cached(&item.uuid)? {
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
            resume_state,
            verbose,
            multipart_workers,
        } => {
            let (mut client, value) = open(&session)?;
            client.set_verbose(verbose);
            if verbose {
                eprintln!(
                    "[verbose] session loaded; local={} remote={}",
                    local.display(),
                    remote.display()
                );
                eprintln!("[verbose] multipart=automatic (>=100 MiB), workers={multipart_workers}");
            }
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
            if verbose {
                let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
                eprintln!(
                    "[verbose] resolved parent={} file={} bytes={size}",
                    parent.display(),
                    name
                );
                eprintln!(
                    "[verbose] starting encrypted upload; a pause can mean S3 is finishing a part"
                );
            }
            if let Some(state_path) = resume_state {
                client.upload_path_with_resume_state_with_workers(
                    &value,
                    &folder.uuid,
                    stem,
                    ext,
                    &local,
                    &state_path,
                    multipart_workers,
                )?;
            } else {
                if multipart_workers == 1 {
                    client.upload_path(&value, &folder.uuid, stem, ext, &local)?;
                } else {
                    let state_path = client.default_upload_resume_state_path(&value, &local);
                    client.upload_path_with_resume_state_with_workers(
                        &value,
                        &folder.uuid,
                        stem,
                        ext,
                        &local,
                        &state_path,
                        multipart_workers,
                    )?;
                }
            }
            if verbose {
                eprintln!("[verbose] upload and Drive metadata creation completed");
            }
            println!("uploaded {}", remote.display());
        }
        Command::WriteTree {
            session,
            local,
            remote,
            on_conflict,
            dry_run,
            preserve_timestamps,
            skip_unchanged,
            includes,
            excludes,
        } => {
            if dry_run {
                let stats = inspect_local_directory(&local)?;
                println!(
                    "would upload {} file(s), {} folder(s), {} bytes",
                    stats.files, stats.folders, stats.bytes
                );
            } else {
                let (client, value) = open(&session)?;
                let folder = client.resolve_path(&value, &remote)?;
                anyhow::ensure!(folder.is_dir, "remote path is not a folder");
                let options = TransferOptions {
                    filter: TransferFilter { includes, excludes },
                    preserve_timestamps,
                    skip_unchanged,
                    ..TransferOptions::default()
                };
                let stats = client.upload_directory_with_options(
                    &value,
                    &local,
                    &folder.uuid,
                    parse_conflict_policy(&on_conflict)?,
                    options,
                )?;
                println!(
                    "uploaded {} file(s), {} folder(s), {} bytes ({} skipped)",
                    stats.files, stats.folders, stats.bytes, stats.skipped
                );
            }
        }
        Command::ReadTree {
            session,
            remote,
            out,
            on_conflict,
            includes,
            excludes,
            preserve_timestamps,
        } => {
            let (client, value) = open(&session)?;
            let folder = client.resolve_path(&value, &remote)?;
            anyhow::ensure!(folder.is_dir, "remote path is not a folder");
            let options = TransferOptions {
                filter: TransferFilter { includes, excludes },
                preserve_timestamps,
                ..TransferOptions::default()
            };
            let stats = client.download_directory_with_options(
                &value,
                &folder.uuid,
                &out,
                parse_conflict_policy(&on_conflict)?,
                options,
            )?;
            println!(
                "downloaded {} file(s), {} folder(s), {} bytes ({} skipped)",
                stats.files, stats.folders, stats.bytes, stats.skipped
            );
        }
        Command::Search {
            session,
            pattern,
            folder,
            case_sensitive,
            max_depth,
        } => {
            let (client, value) = open(&session)?;
            for result in
                client.search_files_from(&value, &folder, &pattern, case_sensitive, max_depth)?
            {
                println!(
                    "{}\t{}\t{}",
                    result.item.uuid,
                    result.item.size,
                    result.path.display()
                );
            }
        }
        Command::Copy {
            session,
            remote,
            destination,
            name,
            on_conflict,
        } => {
            let (client, value) = open(&session)?;
            let source = client.resolve_path(&value, &remote)?;
            let target = client.resolve_path(&value, &destination)?;
            anyhow::ensure!(target.is_dir, "copy destination is not a folder");
            let policy = parse_conflict_policy(&on_conflict)?;
            if source.is_dir {
                let (_, stats) = client.copy_folder_with_policy(
                    &value,
                    &source.uuid,
                    &target.uuid,
                    name.as_deref(),
                    policy,
                )?;
                println!(
                    "copied {} file(s), {} folder(s), {} bytes",
                    stats.files, stats.folders, stats.bytes
                );
            } else {
                let copied = client.copy_file_with_policy(
                    &value,
                    &source.uuid,
                    &target.uuid,
                    name.as_deref(),
                    policy,
                )?;
                println!("copied {}", copied.name);
            }
        }
        Command::Update {
            session,
            remote,
            local,
        } => {
            let (client, value) = open(&session)?;
            let source = client.resolve_path(&value, &remote)?;
            anyhow::ensure!(!source.is_dir, "cannot update a folder");
            client.update_file(&value, &source.uuid, &local)?;
            println!("updated {}", remote.display());
        }
        Command::Touch {
            session,
            remote,
            timestamp,
        } => {
            let (client, value) = open(&session)?;
            let item = client.resolve_path(&value, &remote)?;
            if item.is_dir {
                client.set_folder_timestamp(&item.uuid, &timestamp)?;
            } else {
                client.set_file_timestamp(&item.uuid, &timestamp)?;
            }
            println!("updated timestamp for {}", remote.display());
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
        Command::TrashList {
            session,
            kind,
            limit,
        } => {
            let (client, _) = open(&session)?;
            for item in client.list_trash(kind.as_deref(), limit)? {
                print_item(&item);
            }
        }
        Command::Restore {
            session,
            uuid,
            kind,
            destination,
        } => {
            let (client, value) = open(&session)?;
            let folder = client.resolve_path(&value, &destination)?;
            anyhow::ensure!(folder.is_dir, "restore destination is not a folder");
            client.restore_from_trash(&uuid, &kind, &folder.uuid)?;
            println!("restored {uuid} into {}", destination.display());
        }
        Command::Purge {
            session,
            uuid,
            kind,
            force,
        } => {
            anyhow::ensure!(force, "permanent deletion requires --force");
            let (client, _) = open(&session)?;
            client.permanently_delete(&uuid, &kind)?;
            println!("permanently deleted {uuid}");
        }
        Command::EmptyTrash { session, force } => {
            anyhow::ensure!(force, "emptying trash requires --force");
            let (client, _) = open(&session)?;
            client.clear_trash()?;
            println!("trash emptied");
        }
        Command::CryptoVector => {
            let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
            let bucket = [0u8; 12];
            let index = [0x11u8; 32];
            println!(
                "{}",
                hex::encode(crisp_internxt::file_key(mnemonic, &bucket, &index))
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

fn parse_conflict_policy(value: &str) -> Result<ConflictPolicy> {
    match value.to_ascii_lowercase().as_str() {
        "fail" => Ok(ConflictPolicy::Fail),
        "skip" => Ok(ConflictPolicy::Skip),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        other => Err(anyhow::anyhow!(
            "unknown conflict policy '{other}' (expected fail, skip, or overwrite)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_conflict_policies_case_insensitively() {
        assert_eq!(parse_conflict_policy("FAIL").unwrap(), ConflictPolicy::Fail);
        assert_eq!(parse_conflict_policy("skip").unwrap(), ConflictPolicy::Skip);
        assert_eq!(
            parse_conflict_policy("Overwrite").unwrap(),
            ConflictPolicy::Overwrite
        );
        assert!(parse_conflict_policy("merge").is_err());
    }

    #[test]
    fn splits_extensionless_and_dotted_names_safely() {
        assert_eq!(split_name("photo.jpg"), ("photo", "jpg"));
        assert_eq!(split_name("README"), ("README", "file"));
        assert_eq!(split_name(".profile"), (".profile", "file"));
    }
}
