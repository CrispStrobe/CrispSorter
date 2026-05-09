//! crispcat — standalone CLI for the catalog library.
//!
//! Mirrors the `crispsorter catalog` subcommand surface but ships as a tiny
//! independent binary (no Tauri, no LanceDB, no embedder).
//!
//! Install:  `cargo install crispcat-cli --path crates/crispcat-cli`

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "crispcat", version, about = "Cathy/Catfish .caf catalog CLI")]
struct Cli {
    #[arg(long, short = 'f', value_enum, default_value_t = OutFormat::Json, global = true)]
    format: OutFormat,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
enum OutFormat { Json, Text }

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Walk a folder and write a .caf catalog.
    Scan {
        folder: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Hash algorithm: md5 / sha1 / sha256 (omit for filename+size only).
        #[arg(long)]
        hash: Option<String>,
        #[arg(long)]
        max_size: Option<u64>,
    },
    /// Read a .caf file's header-only metadata.
    Info { path: PathBuf },
    /// List entries inside a .caf file.
    Browse {
        path: PathBuf,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long, default_value_t = 1000)]
        limit: usize,
    },
    /// Find duplicates between a source and one or more destinations.
    FindDupes {
        source: String,
        #[arg(required = true)]
        destinations: Vec<String>,
        #[arg(long, default_value = "name-and-size")]
        strategy: String,
    },
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => { e.print().ok(); return if e.use_stderr() { ExitCode::from(2) } else { ExitCode::SUCCESS } }
    };
    let r: anyhow::Result<()> = match cli.command {
        Cmd::Scan { folder, out, hash, max_size }       => run_scan(cli.format, folder, out, hash, max_size),
        Cmd::Info { path }                              => run_info(cli.format, path),
        Cmd::Browse { path, filter, limit }             => run_browse(cli.format, path, filter, limit),
        Cmd::FindDupes { source, destinations, strategy }=> run_find_dupes(cli.format, source, destinations, strategy),
    };
    match r { Ok(()) => ExitCode::SUCCESS, Err(e) => { eprintln!("error: {e}"); ExitCode::FAILURE } }
}

fn run_scan(out: OutFormat, folder: PathBuf, out_path: Option<PathBuf>, hash: Option<String>, max_size: Option<u64>) -> anyhow::Result<()> {
    let opts = crispcat::scan::ScanOptions {
        hash: hash.as_deref().and_then(|s| match s.to_ascii_lowercase().as_str() {
            "md5" => Some(crispcat::scan::HashAlgo::Md5),
            "sha1" => Some(crispcat::scan::HashAlgo::Sha1),
            "sha256" => Some(crispcat::scan::HashAlgo::Sha256),
            _ => None,
        }),
        max_size_bytes: max_size,
        follow_symlinks: false,
    };
    eprintln!("scanning {}…", folder.display());
    let idx = crispcat::scan::scan_dir(&folder, opts)?;
    let out_caf = out_path.unwrap_or_else(|| {
        let leaf = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "catalog".into());
        PathBuf::from(format!("{leaf}.caf"))
    });
    crispcat::caf::write_file(&out_caf, &idx, crispcat::caf::unix_now())?;
    match out {
        OutFormat::Json => println!("{}", serde_json::json!({
            "scanned_folder": folder.display().to_string(),
            "out": out_caf.display().to_string(),
            "files": idx.len(),
            "total_size_bytes": idx.total_size(),
        })),
        OutFormat::Text => println!("scanned {} files ({} bytes total) → {}", idx.len(), idx.total_size(), out_caf.display()),
    }
    Ok(())
}

fn run_info(out: OutFormat, path: PathBuf) -> anyhow::Result<()> {
    let meta = crispcat::caf::read_metadata(&path)?;
    match out {
        OutFormat::Json => println!("{}", serde_json::json!({
            "path": path.display().to_string(),
            "version": meta.version, "device": meta.device, "volume": meta.volume,
            "alias": meta.alias, "serial": meta.serial, "comment": meta.comment,
            "date_unix": meta.date, "file_count": meta.file_count, "total_size_bytes": meta.total_size,
        })),
        OutFormat::Text => {
            println!("path:       {}", path.display());
            println!("version:    v{}", meta.version);
            println!("volume:     {}", meta.volume);
            println!("file_count: {}", meta.file_count);
            println!("total_size: {} bytes", meta.total_size);
        }
    }
    Ok(())
}

fn run_browse(out: OutFormat, path: PathBuf, filter: Option<String>, limit: usize) -> anyhow::Result<()> {
    let idx = crispcat::caf::read_file(&path)?;
    let q = filter.as_deref().map(|s| s.to_lowercase());
    let mut shown = 0usize;
    for entry in &idx.all_files {
        if shown >= limit { break; }
        if let Some(q) = &q {
            if !entry.path.to_string_lossy().to_lowercase().contains(q) { continue; }
        }
        match out {
            OutFormat::Json => println!("{}", serde_json::json!({
                "path": entry.path.display().to_string(),
                "size": entry.size, "mtime_unix": entry.mtime, "hash": entry.hash,
            })),
            OutFormat::Text => println!("{:>10}  {}", entry.size, entry.path.display()),
        }
        shown += 1;
    }
    eprintln!("{} entries total, {} shown", idx.len(), shown);
    Ok(())
}

fn run_find_dupes(out: OutFormat, source: String, destinations: Vec<String>, strategy: String) -> anyhow::Result<()> {
    use crispcat::dedup::{find_duplicates, DedupOptions, MatchStrategy};
    use crispcat::scan::HashAlgo;
    let strat = match strategy.to_ascii_lowercase().as_str() {
        "" | "name-and-size"    => MatchStrategy::NameAndSize,
        "hash:md5" | "md5"      => MatchStrategy::Hash(HashAlgo::Md5),
        "hash:sha1" | "sha1"    => MatchStrategy::Hash(HashAlgo::Sha1),
        "hash:sha256"|"sha256"  => MatchStrategy::Hash(HashAlgo::Sha256),
        other => anyhow::bail!("unknown strategy `{other}`"),
    };
    let src_idx = load_or_scan(&source)?;
    let mut total = 0usize;
    for dest in destinations {
        let dst_idx = load_or_scan(&dest)?;
        let opts = DedupOptions { strategy: strat };
        let matches = find_duplicates(&src_idx, &dst_idx, &opts);
        for m in &matches {
            match out {
                OutFormat::Json => println!("{}", serde_json::json!({
                    "source": m.source.path.display().to_string(),
                    "destinations": m.destinations.iter().map(|d| d.path.display().to_string()).collect::<Vec<_>>(),
                    "size": m.source.size,
                })),
                OutFormat::Text => {
                    println!("{} ({} bytes)", m.source.path.display(), m.source.size);
                    for d in &m.destinations { println!("    ↳ {}", d.path.display()); }
                }
            }
        }
        total += matches.len();
    }
    eprintln!("found {total} match(es)");
    Ok(())
}

fn load_or_scan(path: &str) -> anyhow::Result<crispcat::index::FileIndex> {
    let p = PathBuf::from(path);
    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("caf") {
        crispcat::caf::read_file(&p).map_err(Into::into)
    } else if p.is_dir() {
        crispcat::scan::scan_dir(&p, crispcat::scan::ScanOptions::default()).map_err(Into::into)
    } else {
        anyhow::bail!("{} is neither a .caf file nor a directory", p.display())
    }
}
