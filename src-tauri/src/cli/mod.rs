//! CrispSorter CLI mode (PLAN P8.2).
//!
//! Single-binary, mode-detected on launch — `main.rs` peeks at
//! `argv[1]` and, if it matches a known subcommand, routes through
//! [`run`] instead of bootstrapping the Tauri GUI. Otherwise the
//! GUI launches as today.
//!
//! The CLI exposes the same backend commands the Tauri frontend
//! invokes — anything that can run without the live Tauri runtime
//! (most of the catalog module + the OCR / extractor dispatchers).
//! Indexed-data subcommands (`index search`, `index ingest`, the
//! `batch` family) need an initialised SearchEngine + IngestPipeline;
//! they're stubbed for now and land in a follow-up that wires the
//! same lazy-init the GUI does at startup.
//!
//! ## Output format
//!
//! Defaults to JSON Lines so `crispsorter catalog browse foo.caf | jq`
//! works without flags. `--text` switches to a human-readable column
//! view. Errors go to stderr with exit code 1.
//!
//! ## Subcommand surface (first cut)
//!
//! ```text
//! crispsorter version              — print version JSON
//! crispsorter doctor               — environment check (tesseract, ocrs models, …)
//! crispsorter catalog scan <DIR>   — walk a folder, write a .caf
//!     [--out PATH]
//!     [--hash md5|sha1|sha256]
//! crispsorter catalog info <CAF>   — header-only metadata
//! crispsorter catalog browse <CAF> — list entries
//!     [--filter SUBSTR] [--limit N]
//! crispsorter catalog find-dupes <SRC> <DST>...
//!     [--strategy name-and-size|hash:md5|hash:sha1|hash:sha256]
//!     [--out json|text]
//! ```

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Subcommand names our CLI knows about. main.rs uses this for the
/// argv[1] sniff so we can fall through to the GUI for anything
/// unrecognised (including no args at all, the typical GUI launch).
pub const SUBCOMMANDS: &[&str] = &[
    "version", "doctor", "catalog", "index", "completion", "help", "--help", "-h",
];

#[derive(Parser, Debug)]
#[command(
    name = "crispsorter",
    about = "CrispSorter — desktop document organiser + search (CLI mode)",
    version
)]
struct Cli {
    /// Output format. JSON is the default for scripting; `text` switches
    /// to a human-readable view. Field renamed to `format` (was `out`)
    /// because clap's downcast machinery hits the global flag's *field
    /// name* when resolving subcommand args, and the original `out`
    /// collided with `catalog scan --out PATH`.
    #[arg(long = "format", short = 'f', value_enum, default_value_t = OutFormat::Json, global = true)]
    format: OutFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
enum OutFormat {
    Json,
    Text,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print version + build info.
    Version,
    /// Check the environment — model paths, optional system tools, etc.
    Doctor,
    /// Catalog (Cathy/Catfish .caf) operations.
    Catalog {
        #[command(subcommand)]
        cmd: CatalogCmd,
    },
    /// Search index operations (LanceDB + Tantivy).
    Index {
        /// Override the app data directory. Default: OS-standard location.
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        cmd: IndexCmd,
    },
    /// Emit shell-completion scripts to stdout.
    Completion {
        /// Target shell.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
enum CatalogCmd {
    /// Walk a folder and write a .caf catalog.
    Scan {
        /// Folder to scan.
        folder: PathBuf,
        /// Output .caf path. Default: <folder-name>.caf in the current dir.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Hash algorithm to compute per file. Default: none (filenames + sizes only).
        #[arg(long)]
        hash: Option<String>,
        /// Skip files larger than this many bytes.
        #[arg(long)]
        max_size: Option<u64>,
    },
    /// Read a .caf file's header-only metadata.
    Info {
        /// .caf file to inspect.
        path: PathBuf,
    },
    /// List entries inside a .caf file.
    Browse {
        /// .caf file to browse.
        path: PathBuf,
        /// Substring filter on entry path.
        #[arg(long)]
        filter: Option<String>,
        /// Cap the number of entries printed.
        #[arg(long, default_value_t = 1000)]
        limit: usize,
    },
    /// Find duplicates between a source and one or more destinations.
    /// Each path can be a folder OR a .caf file (auto-detected).
    FindDupes {
        /// Source folder or .caf file.
        source: String,
        /// Destination folder or .caf file. Repeatable.
        #[arg(required = true)]
        destinations: Vec<String>,
        /// Match strategy. `name-and-size` (default) or `hash:md5|sha1|sha256`.
        #[arg(long, default_value = "name-and-size")]
        strategy: String,
    },
}

/// Run the CLI to completion. Caller (main.rs) returns this exit code
/// directly. Stdout is reserved for the structured payload; progress
/// and diagnostics go to stderr.
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap prints help / errors to stderr already; exit code
            // matches clap's convention.
            e.print().ok();
            return if e.use_stderr() { ExitCode::from(2) } else { ExitCode::SUCCESS };
        }
    };

    let result: Result<(), String> = match cli.command {
        Command::Version => cmd_version(cli.format),
        Command::Doctor => cmd_doctor(cli.format),
        Command::Catalog { cmd } => match cmd {
            CatalogCmd::Scan { folder, out, hash, max_size } => {
                cmd_catalog_scan(cli.format, folder, out, hash, max_size)
            }
            CatalogCmd::Info { path } => cmd_catalog_info(cli.format, path),
            CatalogCmd::Browse { path, filter, limit } => {
                cmd_catalog_browse(cli.format, path, filter, limit)
            }
            CatalogCmd::FindDupes { source, destinations, strategy } => {
                cmd_catalog_find_dupes(cli.format, source, destinations, strategy)
            }
        },
        Command::Index { data_dir, cmd } => cmd_index(cli.format, data_dir, cmd),
        Command::Completion { shell } => {
            use clap::CommandFactory;
            use clap_complete::generate;
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_owned();
            generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

// ── version + doctor ────────────────────────────────────────────────────────

fn cmd_version(out: OutFormat) -> Result<(), String> {
    let v = env!("CARGO_PKG_VERSION");
    match out {
        OutFormat::Json => {
            let payload = serde_json::json!({
                "name": "crispsorter",
                "version": v,
                "target": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            });
            println!("{}", payload);
        }
        OutFormat::Text => {
            println!("crispsorter {v} ({}/{})", std::env::consts::OS, std::env::consts::ARCH);
        }
    }
    Ok(())
}

fn cmd_doctor(out: OutFormat) -> Result<(), String> {
    let tesseract = crate::extractors::ocr::is_tesseract_installed();
    let ocrs_models = crate::extractors::ocr_ocrs::is_ocrs_available();
    let pdf_extract_ok = true; // pulled in unconditionally
    let lance_dir = std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h)
            .join("Library/Application Support/com.<user>.crispsorter/lance")
    });
    match out {
        OutFormat::Json => {
            let payload = serde_json::json!({
                "tesseract_installed": tesseract,
                "ocrs_models_available": ocrs_models,
                "pdf_extract_compiled_in": pdf_extract_ok,
                "lance_dir_exists": lance_dir
                    .as_ref()
                    .map(|p| p.exists())
                    .unwrap_or(false),
                "lance_dir": lance_dir.as_ref().map(|p| p.display().to_string()),
            });
            println!("{}", payload);
        }
        OutFormat::Text => {
            println!("OCR Tier 1 (tesseract installed): {}", yn(tesseract));
            println!("OCR Tier 2 (ocrs models present): {}", yn(ocrs_models));
            println!("PDF extractor (pdf-extract):     {}", yn(pdf_extract_ok));
            if let Some(p) = lance_dir {
                println!("Lance dir: {} ({})", p.display(), if p.exists() { "exists" } else { "absent" });
            }
        }
    }
    Ok(())
}

fn yn(b: bool) -> &'static str { if b { "✓" } else { "✗" } }

// ── catalog ────────────────────────────────────────────────────────────────

fn cmd_catalog_scan(
    out: OutFormat,
    folder: PathBuf,
    out_path: Option<PathBuf>,
    hash: Option<String>,
    max_size: Option<u64>,
) -> Result<(), String> {
    let opts = crate::catalog::scan::ScanOptions {
        hash: hash.as_deref().and_then(|s| match s.to_ascii_lowercase().as_str() {
            "md5" => Some(crate::catalog::scan::HashAlgo::Md5),
            "sha1" => Some(crate::catalog::scan::HashAlgo::Sha1),
            "sha256" => Some(crate::catalog::scan::HashAlgo::Sha256),
            _ => None,
        }),
        max_size_bytes: max_size,
        follow_symlinks: false,
    };
    eprintln!("scanning {}…", folder.display());
    let idx = crate::catalog::scan::scan_dir(&folder, opts).map_err(|e| e.to_string())?;
    let out_caf = out_path.unwrap_or_else(|| {
        let leaf = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "catalog".into());
        PathBuf::from(format!("{leaf}.caf"))
    });
    crate::catalog::caf::write_file(&out_caf, &idx, crate::catalog::caf::unix_now())
        .map_err(|e| e.to_string())?;
    match out {
        OutFormat::Json => {
            let payload = serde_json::json!({
                "scanned_folder": folder.display().to_string(),
                "out": out_caf.display().to_string(),
                "files": idx.len(),
                "total_size_bytes": idx.total_size(),
            });
            println!("{}", payload);
        }
        OutFormat::Text => {
            println!("scanned {} files ({} bytes total) → {}",
                     idx.len(), idx.total_size(), out_caf.display());
        }
    }
    Ok(())
}

fn cmd_catalog_info(out: OutFormat, path: PathBuf) -> Result<(), String> {
    let meta = crate::catalog::caf::read_metadata(&path).map_err(|e| e.to_string())?;
    match out {
        OutFormat::Json => {
            let payload = serde_json::json!({
                "path": path.display().to_string(),
                "version": meta.version,
                "device": meta.device,
                "volume": meta.volume,
                "alias": meta.alias,
                "serial": meta.serial,
                "comment": meta.comment,
                "date_unix": meta.date,
                "file_count": meta.file_count,
                "total_size_bytes": meta.total_size,
                "archive_flag": meta.archive,
                "freesize": meta.freesize,
            });
            println!("{}", payload);
        }
        OutFormat::Text => {
            println!("path:        {}", path.display());
            println!("version:     v{}", meta.version);
            println!("device:      {}", meta.device);
            println!("volume:      {}", meta.volume);
            println!("alias:       {}", meta.alias);
            println!("comment:     {}", meta.comment);
            println!("file_count:  {}", meta.file_count);
            println!("total_size:  {} bytes", meta.total_size);
            let date = chrono_like(meta.date);
            println!("created:     {date}");
        }
    }
    Ok(())
}

fn cmd_catalog_browse(
    out: OutFormat,
    path: PathBuf,
    filter: Option<String>,
    limit: usize,
) -> Result<(), String> {
    let idx = crate::catalog::caf::read_file(&path).map_err(|e| e.to_string())?;
    let q = filter.as_deref().map(|s| s.to_lowercase());
    let mut shown = 0usize;
    for entry in &idx.all_files {
        if shown >= limit {
            break;
        }
        if let Some(q) = &q {
            if !entry.path.to_string_lossy().to_lowercase().contains(q) {
                continue;
            }
        }
        match out {
            OutFormat::Json => {
                let payload = serde_json::json!({
                    "path": entry.path.display().to_string(),
                    "size": entry.size,
                    "mtime_unix": entry.mtime,
                    "hash": entry.hash,
                });
                println!("{}", payload);
            }
            OutFormat::Text => {
                println!(
                    "{:>10}  {}  {}",
                    entry.size,
                    chrono_like(entry.mtime),
                    entry.path.display()
                );
            }
        }
        shown += 1;
    }
    eprintln!(
        "{} entries total, {} shown{}",
        idx.len(),
        shown,
        if filter.is_some() { " (after filter)" } else { "" }
    );
    Ok(())
}

fn cmd_catalog_find_dupes(
    out: OutFormat,
    source: String,
    destinations: Vec<String>,
    strategy: String,
) -> Result<(), String> {
    use crate::catalog::dedup::{find_duplicates, DedupOptions, MatchStrategy};
    use crate::catalog::scan::HashAlgo;
    let strat = match strategy.to_ascii_lowercase().as_str() {
        "" | "name-and-size" => MatchStrategy::NameAndSize,
        "hash:md5" | "md5" => MatchStrategy::Hash(HashAlgo::Md5),
        "hash:sha1" | "sha1" => MatchStrategy::Hash(HashAlgo::Sha1),
        "hash:sha256" | "sha256" => MatchStrategy::Hash(HashAlgo::Sha256),
        other => return Err(format!("unknown strategy `{other}`")),
    };
    let src_idx = load_or_scan(&source)?;
    let mut total_matches = 0usize;
    for dest in destinations {
        let dst_idx = load_or_scan(&dest)?;
        let opts = DedupOptions { strategy: strat };
        let matches = find_duplicates(&src_idx, &dst_idx, &opts);
        for m in &matches {
            match out {
                OutFormat::Json => {
                    let payload = serde_json::json!({
                        "source": m.source.path.display().to_string(),
                        "destinations": m.destinations.iter()
                            .map(|d| d.path.display().to_string())
                            .collect::<Vec<_>>(),
                        "size": m.source.size,
                    });
                    println!("{}", payload);
                }
                OutFormat::Text => {
                    println!("{} ({} bytes)", m.source.path.display(), m.source.size);
                    for d in &m.destinations {
                        println!("    ↳ {}", d.path.display());
                    }
                }
            }
        }
        total_matches += matches.len();
    }
    eprintln!("found {total_matches} match(es)");
    Ok(())
}

// ── index ──────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
enum IndexCmd {
    /// Show document and chunk counts.
    Stats,
    /// List indexed documents.
    List {
        /// Maximum rows to print.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Full-text search without loading the embedder (BM25 only).
    Search {
        /// Query string.
        query: String,
        /// Maximum results.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Ingest a file or folder into the index (requires the app to have
    /// already initialised the embedder via the GUI or `index init`).
    /// Currently stubbed — use the GUI Hinzufügen tab for full ingest.
    Ingest {
        /// Paths to ingest.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Delete a document by doc_id.
    Delete {
        /// Document ID (UUID).
        doc_id: String,
    },
}

/// Return the OS-default app data dir for CrispSorter, or the override.
fn resolve_data_dir(override_: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = override_ {
        return Ok(p);
    }
    // Mirror what tauri::path::app_data_dir() returns per OS.
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "$HOME not set".to_string())?;
        return Ok(home
            .join("Library/Application Support")
            .join("com.<user>.crispsorter"));
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "%APPDATA% not set".to_string())?;
        return Ok(appdata.join("com.<user>.crispsorter"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // XDG: $XDG_DATA_HOME or ~/.local/share
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".local/share")
            });
        return Ok(base.join("com.<user>.crispsorter"));
    }
}

fn cmd_index(out: OutFormat, data_dir: Option<PathBuf>, cmd: IndexCmd) -> Result<(), String> {
    let data_dir = resolve_data_dir(data_dir)?;
    if !data_dir.exists() {
        return Err(format!(
            "data dir not found: {} — run the GUI once to initialise the index",
            data_dir.display()
        ));
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(cmd_index_async(out, data_dir, cmd))
}

async fn cmd_index_async(
    out: OutFormat,
    data_dir: PathBuf,
    cmd: IndexCmd,
) -> Result<(), String> {
    match cmd {
        IndexCmd::Stats => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            let chunks = local.count().await.map_err(|e| e.to_string())?;
            let docs = local.count_docs().await.map_err(|e| e.to_string())?;
            let fts_dir = data_dir.join("fts");
            let fts_docs: u64 = if fts_dir.exists() {
                crate::index::FtsIndex::open_or_create(&fts_dir)
                    .map(|fts| fts.doc_count())
                    .unwrap_or(0)
            } else {
                0
            };
            match out {
                OutFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "docs": docs,
                            "chunks": chunks,
                            "fts_docs": fts_docs,
                            "data_dir": data_dir.display().to_string(),
                        })
                    );
                }
                OutFormat::Text => {
                    println!("Documents : {docs}");
                    println!("Chunks    : {chunks}");
                    println!("FTS docs  : {fts_docs}");
                    println!("Data dir  : {}", data_dir.display());
                }
            }
        }

        IndexCmd::List { limit } => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            let rows = local
                .list_documents(limit)
                .await
                .map_err(|e| e.to_string())?;
            for r in &rows {
                match out {
                    OutFormat::Json => {
                        let payload = serde_json::json!({
                            "doc_id": r.doc_id,
                            "filename": r.filename,
                            "title": r.title,
                            "author": r.author,
                            "year": r.year,
                            "ext": r.ext,
                            "location_uri": r.location_uri,
                        });
                        println!("{payload}");
                    }
                    OutFormat::Text => {
                        let title = r.title.as_deref().unwrap_or(
                            r.filename.as_deref().unwrap_or("(unknown)"),
                        );
                        let author = r.author.as_deref().unwrap_or("");
                        let year = r.year.map(|y| y.to_string()).unwrap_or_default();
                        let ext = r.ext.as_deref().unwrap_or("");
                        println!("{:<50} {:>4}  {:<8}  {}", title, year, ext, author);
                    }
                }
            }
            eprintln!("{} document(s) shown (limit {})", rows.len(), limit);
        }

        IndexCmd::Search { query, limit } => {
            let fts_dir = data_dir.join("fts");
            if !fts_dir.exists() {
                return Err("FTS index not found — run the app and ingest some files first".into());
            }
            let fts = crate::index::FtsIndex::open_or_create(&fts_dir)
                .map_err(|e| e.to_string())?;
            let filters = crate::index::SearchFilters::default();
            let hits = fts
                .search(&query, &filters, limit)
                .map_err(|e| e.to_string())?;
            // Resolve doc metadata from LanceDB.
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            let doc_ids: Vec<String> = hits.iter().map(|h| h.doc_id.clone()).collect();
            let meta_map: std::collections::HashMap<String, crate::index::SearchResult> = local
                .fetch_search_results_by_ids(&doc_ids)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|r| (r.doc_id.clone(), r))
                .collect();
            for hit in &hits {
                let meta = meta_map.get(&hit.doc_id);
                match out {
                    OutFormat::Json => {
                        let payload = serde_json::json!({
                            "doc_id": hit.doc_id,
                            "score": hit.score,
                            "filename": meta.and_then(|m| m.filename.as_deref()),
                            "title": meta.and_then(|m| m.title.as_deref()),
                            "author": meta.and_then(|m| m.author.as_deref()),
                            "year": meta.and_then(|m| m.year),
                        });
                        println!("{payload}");
                    }
                    OutFormat::Text => {
                        let title = meta
                            .and_then(|m| m.title.as_deref())
                            .or_else(|| meta.and_then(|m| m.filename.as_deref()))
                            .unwrap_or(&hit.doc_id);
                        println!("[{:.3}] {}", hit.score, title);
                    }
                }
            }
            eprintln!("{} result(s)", hits.len());
        }

        IndexCmd::Ingest { .. } => {
            return Err(
                "headless ingest is not yet implemented — use the GUI Hinzufügen tab \
                 or background ingest scheduler".into(),
            );
        }

        IndexCmd::Delete { doc_id } => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            local.delete_doc(&doc_id).await.map_err(|e| e.to_string())?;
            let fts_dir = data_dir.join("fts");
            if fts_dir.exists() {
                if let Ok(fts) = crate::index::FtsIndex::open_or_create(&fts_dir) {
                    if let Ok(mut writer) = fts.writer() {
                        fts.delete_document(&mut writer, &doc_id)
                            .map_err(|e| e.to_string())?;
                        writer.commit().map_err(|e| e.to_string())?;
                    }
                }
            }
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::json!({ "deleted": doc_id }));
                }
                OutFormat::Text => {
                    println!("deleted {doc_id}");
                }
            }
        }
    }
    Ok(())
}

fn load_or_scan(path: &str) -> Result<crate::catalog::index::FileIndex, String> {
    let p = std::path::PathBuf::from(path);
    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("caf") {
        crate::catalog::caf::read_file(&p).map_err(|e| format!("loading {}: {e}", p.display()))
    } else if p.is_dir() {
        crate::catalog::scan::scan_dir(&p, crate::catalog::scan::ScanOptions::default())
            .map_err(|e| format!("scanning {}: {e}", p.display()))
    } else {
        Err(format!("{} is neither a .caf file nor a directory", p.display()))
    }
}

/// Cheap epoch-seconds → ISO-8601 date formatting without pulling in
/// `chrono` (already-too-many-deps-this-session). UTC, day-precision.
fn chrono_like(epoch_secs: u32) -> String {
    if epoch_secs == 0 {
        return "(unset)".to_string();
    }
    let secs = epoch_secs as i64;
    // Days since unix epoch.
    let days = secs / 86400;
    // Approximate Y-M-D — Howard Hinnant's date algorithm. We could
    // grab `chrono` but a hand-rolled formatter keeps the CLI module
    // dependency-free.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_formatter_handles_known_epochs() {
        assert_eq!(chrono_like(0), "(unset)");
        // 2020-01-01 00:00:00 UTC
        assert_eq!(chrono_like(1577836800), "2020-01-01");
        // A recent known timestamp should parse without "(unset)".
        // Use std::time to derive what the current year-month is so
        // this doesn't need updating every month.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .min(u32::MAX as u64) as u32;
        let today = chrono_like(now_secs);
        assert_ne!(today, "(unset)");
        assert!(today.len() == 10, "expected YYYY-MM-DD, got {today:?}");
    }

    #[test]
    fn subcommands_cover_the_router() {
        // Sanity that every value clap routes is in our argv[1] sniff
        // list. If a new subcommand lands in `Command` without an
        // entry here, main.rs would silently fall through to the GUI.
        for s in ["version", "doctor", "catalog", "index"] {
            assert!(SUBCOMMANDS.contains(&s), "SUBCOMMANDS missing {s}");
        }
    }
}
