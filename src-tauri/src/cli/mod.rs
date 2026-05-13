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
    "version", "doctor", "catalog", "index", "batch", "chat", "images",
    "manpage", "completion", "help", "--help", "-h",
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

/// Per-subcommand format selector for `chat transcribe`.  Independent
/// from the global `-f json|text` because subtitle formats only make
/// sense for transcribe; cluttering the global enum (96 match arms
/// across the CLI surface) with `Srt`/`Vtt` variants would force
/// fallback arms in unrelated commands.
///
/// `None` (default at the arg level) means "fall back to the global
/// -f mapping": `OutFormat::Json` → `Json`, `OutFormat::Text` → `Txt`.
/// SRT/VTT require this flag explicitly.
#[derive(clap::ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
enum TranscriptFormat {
    /// Plain text — joined segments without timestamps.  Equivalent
    /// to `-f text`.
    Txt,
    /// JSON envelope with the existing `-f json` shape plus a
    /// `segments` array (each segment carries `text` + `start` +
    /// `end` in seconds).
    Json,
    /// SubRip subtitle format (`HH:MM:SS,mmm --> HH:MM:SS,mmm`).
    /// One block per ASR segment, numbered from 1.
    Srt,
    /// WebVTT format (`HH:MM:SS.mmm --> HH:MM:SS.mmm` with a `WEBVTT`
    /// header).  Same one-block-per-segment shape as SRT.
    Vtt,
}

/// LID-driven routing policy choice for `chat transcribe --policy`.
/// Mirrors `crate::asr::lang::BackendFallback` variants minus the
/// runtime data (fallback / target are supplied via separate flags
/// so each variant stays a single clap word).
#[derive(clap::ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
enum LidPolicy {
    /// No LID, no routing — use `--backend` exactly as given.
    AsConfigured,
    /// Fail if `--backend` doesn't speak the detected language.
    Strict,
    /// Switch to `--fallback` when `--backend` doesn't speak the
    /// detected language; otherwise stay on `--backend`.
    Auto,
}

/// LID model method choice for `chat transcribe --lid-method`.
/// Mirrors `crate::asr::lang::LidMethod`'s Whisper + Silero variants.
/// `ecapa` / `firered` are reserved for a follow-up: they only work
/// through the session-level C-ABI today and would need session
/// pre-load wiring here.
#[derive(clap::ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
enum LidMethodChoice {
    Whisper,
    Silero,
}

impl LidMethodChoice {
    fn into_lib(self) -> crate::asr::LidMethod {
        match self {
            LidMethodChoice::Whisper => crate::asr::LidMethod::Whisper,
            LidMethodChoice::Silero => crate::asr::LidMethod::Silero,
        }
    }
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
    /// Generate man page(s) and write them to a directory (or stdout).
    Manpage {
        /// Output directory. Defaults to the current directory.
        /// One `crispsorter.1` file is written per top-level command.
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Durable sort-job queue operations.
    Batch {
        /// Override the app data directory.
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        cmd: BatchCmd,
    },
    /// Query an LLM or transcribe audio / synthesise speech (headless).
    Chat {
        #[command(subcommand)]
        cmd: ChatCmd,
    },
    /// P13 images vertical — image-row views over the local index.
    /// Tier 1 (local-only) for slice A1; Tier 2 (CrispLens) lands in B1+.
    Images {
        /// Override the app data directory. Default: OS-standard location.
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
        #[command(subcommand)]
        cmd: ImagesCmd,
    },
}

#[derive(Subcommand, Debug)]
enum ImagesCmd {
    /// Print the canonical image-extension list (jpg, jpeg, png, webp,
    /// heic, heif, tiff, bmp).  Same list `images list` filters on by
    /// default; mirrors the `images_default_extensions` Tauri command.
    Extensions,
    /// Print the count of image rows in the local index, plus a
    /// per-extension breakdown.  Cheap (server-side `count_rows`).
    Count {
        /// Override the canonical IMAGE_EXTS list (comma-separated,
        /// lower-case, no leading dot).  Empty = use the default.
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// Optional parent-folder prefix to scope the count.
        /// Matches `LocalImages::list`'s `parent_dir_prefix` filter.
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// List image rows from the local index.  Output respects the
    /// global `--format` flag (`json` default, `text` for humans).
    List {
        /// Maximum rows to print.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Override the canonical IMAGE_EXTS list (comma-separated,
        /// lower-case, no leading dot).  Empty = use the default.
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// Optional parent-folder prefix to scope the listing.
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Generate a PNG thumbnail of `path` and write the bytes to
    /// stdout (or `--out`).  No data dir / no index lookup — operates
    /// directly on a file, so it doubles as a smoke test of the
    /// thumbnail pipeline.
    Thumbnail {
        /// Input image path.
        path: PathBuf,
        /// Longest-edge size in pixels.  Default 256, max 4096.
        #[arg(long, default_value_t = 256)]
        size: u32,
        /// Write to this path instead of stdout (recommended in
        /// terminal sessions — raw PNG bytes corrupt scroll-back).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Read EXIF metadata from `path` and print the curated subset.
    /// Same caveat as `thumbnail`: takes a path, not a doc-id, so it
    /// works on any image file regardless of index state.
    Exif {
        /// Input image path.
        path: PathBuf,
    },
    /// Group image rows by SHA-256 `source_hash` and print clusters
    /// of byte-identical files (size >= 2).  Largest groups first.
    /// Output respects the global `--format` flag.
    Duplicates {
        /// Override the canonical IMAGE_EXTS list (comma-separated,
        /// lower-case, no leading dot).
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// Optional parent-folder prefix to scope the dup-detection.
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Group image rows by perceptual-hash similarity (Hamming
    /// distance ≤ threshold).  Catches visually-identical files that
    /// the SHA-256 view misses (resizes, recompressions).  Slow on
    /// big indexes — every row decoded + hashed on demand.
    NearDuplicates {
        /// Hamming-distance threshold (0..=64).  Default 8 — proven
        /// safe for JPEG resizes per the spec.
        #[arg(long, default_value_t = 8)]
        threshold: u32,
        /// Override the canonical IMAGE_EXTS list (comma-separated,
        /// lower-case, no leading dot).
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// Optional parent-folder prefix to scope the near-dup pass.
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// P13/B1 — CrispLens (Tier 2) settings + auth.  Nested
    /// subcommands so future B2-B5 routes stack here cleanly.
    Crisplens {
        #[command(subcommand)]
        cmd: CrispLensCmd,
    },
}

#[derive(Subcommand, Debug)]
enum CrispLensCmd {
    /// Print current non-secret CrispLens settings (backend + URL +
    /// UI tunables).  Does NOT touch the keychain.
    Settings,
    /// Update non-secret CrispLens settings.  Persists to the
    /// `crisplens.settings.json` file under the app data dir.
    SetUrl {
        /// CrispLens base URL (no trailing slash needed).
        url: String,
        /// Also switch the active backend to `crisplens`.
        /// When omitted the backend stays on whatever it was.
        #[arg(long)]
        enable: bool,
    },
    /// Disable Tier 2; keeps the URL on file but switches the
    /// active backend back to `local`.
    Disable,
    /// Report whether a session cookie is stored for the configured
    /// URL.  Boolean only — never leaks the cookie value.
    SessionStatus,
    /// POST `/api/auth/login` to the configured URL and store the
    /// returned session cookie in the OS keychain.  Reads the
    /// password from `--password` or, if absent, from the
    /// `CRISPLENS_PASSWORD` env var.  Never read from positional
    /// args (would leak in shell history).
    Login {
        /// Username to authenticate as.
        #[arg(long)]
        user: String,
        /// Password.  If omitted, falls back to `CRISPLENS_PASSWORD`.
        #[arg(long)]
        password: Option<String>,
    },
    /// POST `/api/auth/logout` (best-effort) and wipe the keychain
    /// entry.  Idempotent.
    Logout,
    /// Live status probe — hits `/api/health` (unauthenticated) and
    /// `/api/auth/me` (with the stored cookie when present).  One
    /// payload covers all four states the UI banner cares about:
    /// not-configured / online-authenticated / online-unauth /
    /// offline.  Useful for headless health checks (cron + jq).
    Status,
    /// List CrispLens's watchfolders.  Used in the GUI for the
    /// "this folder is also watched by CrispLens" hint in the
    /// Bilder preview pane (slice B5).
    Watchfolders,
    /// List person clusters from `GET /api/people` — the Faces
    /// subtab feed (slice B3).
    People,
    /// List face crops detected in a single image
    /// (`GET /api/images/{image_id}/faces`).
    ImageFaces {
        image_id: i64,
    },
    /// Run a semantic search on CrispLens via
    /// `/api/search/semantic` (embedding-based over the
    /// `ai_description` text column).  Falls back to a 404 against
    /// older CrispLens builds that pre-date the endpoint —
    /// upgrade the server or query `/api/search` directly with
    /// curl as a workaround.
    Search {
        /// Query string.
        q: String,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Resolve a SHA-256 file hash to a CrispLens `Image` row via
    /// `/api/images/by-hash/{sha256}`.  Used by the GUI to bridge
    /// a CrispSorter-local image to its server-side image_id so
    /// the preview pane can overlay face bounding boxes from
    /// CrispLens's face-recognition data.
    ImageByHash {
        /// 64-char lowercase hex SHA-256.  Pass `--from-file PATH`
        /// to hash a local file instead.
        #[arg(required_unless_present = "from_file")]
        sha256: Option<String>,
        /// Path to a local file — its SHA-256 is computed and used
        /// as the lookup key.
        #[arg(long, conflicts_with = "sha256")]
        from_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum ChatCmd {
    /// Send a chat prompt to an LLM and print the response.
    Query {
        /// The user prompt. Wrap in quotes.
        prompt: String,
        /// OpenAI-compatible base URL. Default: http://localhost:11434/v1
        #[arg(long, default_value = "http://localhost:11434/v1")]
        llm_url: String,
        /// Model name.
        #[arg(long, default_value = "llama3")]
        llm_model: String,
        /// API key for the endpoint.
        #[arg(long, default_value = "")]
        api_key: String,
        /// Optional system prompt.
        #[arg(long)]
        system: Option<String>,
        /// Files whose text content is appended as context.
        #[arg(long)]
        context_files: Vec<PathBuf>,
    },
    /// Transcribe an audio / video file to text via CrispASR (P13.5 slice A).
    ///
    /// Decodes the input through the shared `audio` module (symphonia
    /// tier 1, ffmpeg fallback tier 2) to 16 kHz mono Float32 PCM, then
    /// runs the configured CrispASR backend.  Output goes to stdout
    /// (or `--output PATH`) as plain text (`-f text`, default) or as a
    /// JSON envelope with decode metadata (`-f json`).
    Transcribe {
        /// Input audio / video path (.wav / .mp3 / .m4a / .flac /
        /// .ogg / .opus / .aac / .mp4 / .mov / .mkv / .webm / .m4v
        /// natively; .avi / .wmv / .flv / .ts / .amr via ffmpeg).
        path: PathBuf,
        /// CrispASR backend name. Default `whisper` (99 languages,
        /// auto-download). Run `crispasr --list-backends` for the
        /// full set — also `parakeet` (25 EU, fast), `distil-whisper`
        /// (English-only, ~6× faster), `omniasr` (1600+ langs), etc.
        #[arg(long, default_value = "whisper")]
        backend: String,
        /// Explicit model file path. Skips the registry auto-download
        /// (no `cache_ensure_file` round-trip).  Useful for testing
        /// custom checkpoints or running offline against a known file.
        #[arg(long)]
        model: Option<PathBuf>,
        /// ISO 639-1 source-language hint (`en`, `de`, `ja`, …) or
        /// the literal `auto` to run audio LID (P13.5 Phase 6) and
        /// route per `--policy`.  Optional — backends with native LID
        /// auto-detect; others fall back to their internal default.
        #[arg(long)]
        language: Option<String>,
        /// Output path; `-` (default) writes to stdout.
        #[arg(long, short = 'o', default_value = "-")]
        output: String,
        /// Override the app data dir (default: OS app-data dir; the
        /// model cache lives under `<data-dir>/models/`).
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Refuse the ffmpeg shell-out fallback for unsupported
        /// containers.  Errors on `.avi` / `.wmv` / … instead of
        /// shelling out — useful in sandboxes that disallow
        /// subprocess spawning.
        #[arg(long)]
        pure_rust: bool,
        /// Stream partial transcripts to stderr as the model
        /// commits them, instead of buffering the whole transcript
        /// and printing at the end (P13.5 follow-up).  Final result
        /// still goes to `--output` (or stdout when output is `-`).
        /// Whisper-only at the C-ABI level today; other backends
        /// will return a clear error.  Stream parameters use the
        /// Whisper-reference defaults (step=3000 ms / length=10000 ms
        /// / keep=200 ms); tune in code if you need tighter latency.
        #[arg(long)]
        stream: bool,
        /// Output format for the transcript: `txt` (plain text,
        /// joined segments), `json` (envelope with metadata + a
        /// `segments` array), `srt` (SubRip subtitle file with
        /// `HH:MM:SS,mmm` timestamps), `vtt` (WebVTT with `WEBVTT`
        /// header + `HH:MM:SS.mmm` timestamps).  Default: falls
        /// back to the global `-f` mapping (`json` / `text` →
        /// `Json` / `Txt`).  SRT and VTT require this flag
        /// explicitly + an `--output PATH.srt` / `.vtt` is the
        /// usual idiom (stdout works too).  Mutually exclusive
        /// with `--stream` for SRT/VTT (segments only arrive
        /// at the end of the buffered transcribe path).
        #[arg(long = "transcript-format", value_enum)]
        transcript_format: Option<TranscriptFormat>,
        /// LID-driven routing policy (P13.5 Phase 6).
        ///   * `as-configured` (default) — no LID, use --backend as-is.
        ///   * `strict` — fail if --backend doesn't speak the detected
        ///     language; never silently produce gibberish.
        ///   * `auto` — switch to --fallback when --backend doesn't
        ///     speak the detected language; transcribe with the
        ///     fallback otherwise.
        #[arg(long = "policy", value_enum, default_value_t = LidPolicy::AsConfigured)]
        policy: LidPolicy,
        /// Fallback backend for `--policy auto`.  Default `whisper`
        /// (99 languages, the broadest-coverage option).  Ignored
        /// when policy is `as-configured` or `strict`.
        #[arg(long = "fallback", default_value = "whisper")]
        fallback_backend: String,
        /// LID model path (required when `--language auto` and the
        /// policy is non-as-configured).  Whisper-method LID reuses
        /// the regular `ggml-*.bin` you already downloaded for
        /// transcription; Silero (16 MB) needs its own GGUF.
        #[arg(long)]
        lid_model: Option<PathBuf>,
        /// LID method.  `whisper` (default, 99 langs, reuses ASR
        /// model file) or `silero` (95 langs, smaller dedicated
        /// model).  `ecapa` / `firered` are reserved for Phase 6.5
        /// — they only work through the session-level surface and
        /// would need session pre-load here.
        #[arg(long = "lid-method", value_enum, default_value_t = LidMethodChoice::Whisper)]
        lid_method: LidMethodChoice,
        /// Target ISO 639-1 language for transcript translation
        /// (P13.5 Phase 5).  When set, the transcribed text gets a
        /// follow-up translation pass via `--translate-backend`
        /// (default m2m100, any-to-any 100 langs) and the output
        /// carries the translated text in place of the original.
        /// Source language must be known — either explicit via
        /// `--language ISO`, or detected via `--language auto`
        /// (which requires --policy != as-configured and --lid-model).
        #[arg(long = "translate-to")]
        translate_to: Option<String>,
        /// MT backend used for the `--translate-to` post-processing
        /// pass.  Default `m2m100` (100 langs, any-to-any).  Other
        /// options: `m2m100-wmt21` (higher quality on EN↔{zh,de,
        /// fr,ja,ru,is,ha}), `madlad` (419-lang long tail),
        /// `gemma4-e2b` (dual ASR+MT).
        #[arg(long = "translate-backend", default_value = "m2m100")]
        translate_backend: String,
        /// Explicit MT model path; skips the registry auto-download.
        #[arg(long = "translate-model")]
        translate_model: Option<PathBuf>,
        /// Max decoder tokens for the translate pass.  0 = upstream
        /// default (200 for m2m100).  Larger values cost more wall-
        /// clock but matter for long transcripts.
        #[arg(long = "translate-max-tokens", default_value_t = 0)]
        translate_max_tokens: i32,
    },
    /// Synthesise text to a WAV via a CrispASR TTS backend (P13.5 slice A).
    ///
    /// Backends today: kokoro (default, English + multi-lingual presets),
    /// qwen3-tts (zero-shot voice cloning from .wav references),
    /// vibevoice-tts (50+ languages), orpheus (preset-speaker), chatterbox.
    /// Output is always 24 kHz mono Float32 WAV (the native rate of
    /// every supported backend).
    Tts {
        /// Text to speak. Wrap in quotes.
        text: String,
        /// TTS backend name. Default `kokoro`.
        #[arg(long, default_value = "kokoro")]
        backend: String,
        /// Explicit model file path. Skips the registry auto-download.
        #[arg(long)]
        model: Option<PathBuf>,
        /// Voice prompt path — a baked GGUF voice pack for most
        /// backends, or a .wav reference clip for qwen3-tts (the
        /// latter also needs `--voice-ref-text`).  Cannot be combined
        /// with `--speaker`.
        #[arg(long, conflicts_with = "speaker")]
        voice: Option<PathBuf>,
        /// Reference text for `.wav` voice prompts (qwen3-tts only —
        /// CrispASR needs to know what the reference says to
        /// disentangle voice from content).
        #[arg(long, requires = "voice")]
        voice_ref_text: Option<String>,
        /// Preset speaker name for backends that bake names into the
        /// GGUF (orpheus: `tara` / `leo` / `Anton` / `Sophie` / …).
        /// Mutually exclusive with `--voice`.
        #[arg(long, conflicts_with = "voice")]
        speaker: Option<String>,
        /// Output WAV path. Required.
        #[arg(long, short = 'o')]
        output: PathBuf,
        /// Override the app data dir (default: OS app-data dir).
        #[arg(long)]
        data_dir: Option<PathBuf>,
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
        Command::Batch { data_dir, cmd } => cmd_batch(cli.format, data_dir, cmd),
        Command::Chat { cmd } => cmd_chat(cli.format, cmd),
        Command::Images { data_dir, cmd } => cmd_images(cli.format, data_dir, cmd),
        Command::Completion { shell } => {
            use clap::CommandFactory;
            use clap_complete::generate;
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_owned();
            generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        Command::Manpage { out } => cmd_manpage(out),
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
    let paddle_ocr = crate::extractors::ocr_paddle::is_paddle_ocr_available();
    let pdf_extract_ok = true;
    // Check if the default embedder model (BGE-M3) is already cached.
    let model_cache = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/Application Support/com.<user>.crispsorter/models"))
        .unwrap_or_default();
    let embedder_cached = model_cache.exists() &&
        std::fs::read_dir(&model_cache).map(|d| d.count() > 0).unwrap_or(false);
    let lance_dir = std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h)
            .join("Library/Application Support/com.<user>.crispsorter/lance")
    });
    match out {
        OutFormat::Json => {
            let payload = serde_json::json!({
                "tesseract_installed": tesseract,
                "ocrs_models_available": ocrs_models,
                "paddle_ocr_available": paddle_ocr,
                "pdf_extract_compiled_in": pdf_extract_ok,
                "embedder_model_cached": embedder_cached,
                "lance_dir_exists": lance_dir
                    .as_ref()
                    .map(|p| p.exists())
                    .unwrap_or(false),
                "lance_dir": lance_dir.as_ref().map(|p| p.display().to_string()),
            });
            println!("{}", payload);
        }
        OutFormat::Text => {
            println!("OCR Tesseract installed:          {}", yn(tesseract));
            println!("OCR ocrs models present:          {}", yn(ocrs_models));
            println!("OCR PaddleOCR compiled:           {}", yn(paddle_ocr));
            println!("PDF extractor (pdf-extract):      {}", yn(pdf_extract_ok));
            println!("Embedder model cached:            {}", yn(embedder_cached));
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
    /// Full-text search with optional filters — BM25 over Tantivy
    /// without loading the embedder, then post-filtered against
    /// the LanceDB scalar columns.  Mirrors the cloud-backup
    /// `search.py` filter set (size range, date range, hash
    /// prefix, ext, …) plus CrispSorter-specific knobs (audio
    /// duration range, image camera make/model, source language,
    /// preferred-translation language).
    Search {
        /// Query string.  Empty/`*` lists rows matching only the
        /// filters (the BM25 stage is short-circuited).
        query: String,
        /// Maximum results.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Restrict to one or more file extensions.  Comma-separated;
        /// leading dots stripped, case-insensitive.  Multi-select
        /// via repeated flag or single-flag comma list.
        /// Example: `--ext pdf,docx,mp3`
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// SHA-256 prefix match against `source_hash`.  Cloud-backup
        /// parity: `--hash a1b2c3` finds rows whose hash starts
        /// with that hex.  Case-sensitive (SHA-256 hex is lowercase).
        #[arg(long)]
        hash: Option<String>,
        /// Folder-prefix match against `parent_dir` — scalar-indexed
        /// in LanceDB so it stays fast on large catalogs.
        #[arg(long, value_name = "PATH")]
        folder_prefix: Option<String>,
        /// Owner UUID filter.  Default `None` matches every owner.
        #[arg(long, value_name = "UUID")]
        owner: Option<String>,
        /// Source-language filter (ISO 639-1, e.g. `en` / `de`).
        #[arg(long)]
        lang: Option<String>,
        /// Show rows whose pre-translated `text_translated` column
        /// matches this language.  Independent of `--lang` (which
        /// targets the source language).  ISO 639-1.
        #[arg(long, value_name = "LANG")]
        translated_to: Option<String>,
        /// Year range filters (`year` column).
        #[arg(long)]
        year_min: Option<i32>,
        #[arg(long)]
        year_max: Option<i32>,
        /// File-size range — human-readable strings ("100MB", "1.5GB").
        /// Applied post-hoc against the `fs_size` field in each row's
        /// `metadata_json` (not a scalar column today; see PLAN.md
        /// for the promote-to-column follow-up).
        #[arg(long, value_name = "SIZE")]
        min_size: Option<String>,
        #[arg(long, value_name = "SIZE")]
        max_size: Option<String>,
        /// ISO-date range against the row's `fs_mtime` metadata blob.
        /// `--after 2024-01-01`, `--before 2025-06-01` — inclusive.
        /// Like size, post-hoc filter — fast enough at CLI scale
        /// (10⁴ rows) but a future scalar column lift would help.
        #[arg(long, value_name = "YYYY-MM-DD")]
        after: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD")]
        before: Option<String>,
        /// Audio duration range (seconds).  Scalar-indexed via the
        /// `audio_duration_seconds` column added by migration v101.
        #[arg(long)]
        audio_duration_min: Option<f64>,
        #[arg(long)]
        audio_duration_max: Option<f64>,
        /// Image camera filters — substring match against the EXIF
        /// columns added by migration v102.
        #[arg(long, value_name = "MAKE")]
        image_camera_make: Option<String>,
        #[arg(long, value_name = "MODEL")]
        image_camera_model: Option<String>,
    },
    /// Download the embedder model weights to the local cache.
    /// Run this once on a fresh install before the first `index ingest`.
    Init {
        /// Model to download. Default: bge-m3.
        #[arg(long, default_value = "bge-m3")]
        model: String,
        /// Device hint: cpu (default), cuda, mps, coreml.
        #[arg(long, default_value = "cpu")]
        device: String,
    },
    /// Ingest files/folders into the local index — full extraction + embedding pipeline.
    /// Run `index init` first to download the embedder model.
    Ingest {
        /// Files or directories to ingest (directories are walked recursively).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Owner UUID. Default: nil (single-user install).
        #[arg(long)]
        owner_id: Option<String>,
        /// Embedder model. Default: bge-m3.
        #[arg(long, default_value = "bge-m3")]
        model: String,
        /// Device for inference. Default: cpu.
        #[arg(long, default_value = "cpu")]
        device: String,
    },
    /// Delete a document by doc_id.
    Delete {
        /// Document ID (UUID).
        doc_id: String,
    },
    /// List documents whose extraction failed (have an extraction_failure blob).
    ListFailed {
        /// Show only retryable failures (timeout / other).
        #[arg(long)]
        retryable_only: bool,
    },
    /// Clear extraction_failure for all retryable rows (timeout / other) so the
    /// background ingest worker re-attempts them on the next run.
    RetryFailed {
        /// Print what would be retried without making changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Permanently mark all timeout/other failure rows as "other-permanent" so
    /// the background worker skips them in future runs without retrying.
    /// Use when you want to suppress noisy retries (e.g. very large files
    /// that always time out) without deleting the L2 metadata row.
    SkipFailed {
        /// Print which rows would be marked without making changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Export a volume (or full index snapshot) to a portable .cidx archive.
    ExportCidx {
        /// Output directory (created if absent). Conventionally ends in `.cidx`.
        dest: PathBuf,
        /// Export only rows for this volume_id. Omit for a full snapshot.
        #[arg(long)]
        volume_id: Option<String>,
        /// Include embedding vectors (large — disabled by default).
        #[arg(long)]
        include_embeddings: bool,
        /// Build a Tantivy FTS index at dest/fts/ for offline full-text search.
        /// Implies exporting full_text + headings_text columns.
        #[arg(long)]
        include_fts: bool,
    },
    /// Show stats for a .cidx archive (docs, chunks).
    InspectCidx {
        /// Path to the .cidx directory.
        path: PathBuf,
    },
    /// Import file metadata from a cloud-backup manifest SQLite as L1 rows.
    /// Reads source_files table: original paths, sizes, mtimes, hashes.
    IngestCbManifest {
        /// Path to the cloud-backup manifest SQLite database.
        manifest_db: PathBuf,
        /// Owner UUID (defaults to nil UUID for single-user installs).
        #[arg(long)]
        owner_id: Option<String>,
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

        IndexCmd::Search {
            query,
            limit,
            ext,
            hash,
            folder_prefix,
            owner,
            lang,
            translated_to,
            year_min,
            year_max,
            min_size,
            max_size,
            after,
            before,
            audio_duration_min,
            audio_duration_max,
            image_camera_make,
            image_camera_model,
        } => {
            let fts_dir = data_dir.join("fts");
            if !fts_dir.exists() {
                return Err("FTS index not found — run the app and ingest some files first".into());
            }
            let fts = crate::index::FtsIndex::open_or_create(&fts_dir)
                .map_err(|e| e.to_string())?;

            // Parse the post-hoc filters (size + date) BEFORE running
            // the search so a bad value bails fast.  Both human-byte
            // and ISO-date parsing live as local helpers below.
            let min_size_bytes = match min_size.as_deref() {
                Some(s) => Some(parse_human_size(s)
                    .map_err(|e| format!("--min-size {s}: {e}"))?),
                None => None,
            };
            let max_size_bytes = match max_size.as_deref() {
                Some(s) => Some(parse_human_size(s)
                    .map_err(|e| format!("--max-size {s}: {e}"))?),
                None => None,
            };
            let after_unix = match after.as_deref() {
                Some(s) => Some(parse_iso_date_to_unix(s)
                    .map_err(|e| format!("--after {s}: {e}"))?),
                None => None,
            };
            let before_unix = match before.as_deref() {
                Some(s) => Some(parse_iso_date_to_unix(s)
                    .map_err(|e| format!("--before {s}: {e}"))?),
                None => None,
            };

            // Build the SearchFilters from the SQL-pushable flags.
            let normalised_ext: Vec<String> = ext
                .into_iter()
                .map(|e| e.trim().trim_start_matches('.').to_lowercase())
                .filter(|e| !e.is_empty())
                .collect();
            let filters = crate::index::SearchFilters {
                owner_id: owner,
                language: lang,
                year_min,
                year_max,
                tags: vec![],
                prefer_translated_lang: translated_to,
                ext: normalised_ext,
                source_hash_prefix: hash,
                parent_dir_prefix: folder_prefix,
                audio_duration_min_seconds: audio_duration_min,
                audio_duration_max_seconds: audio_duration_max,
                image_camera_make,
                image_camera_model,
            };

            // FTS pass.  An empty query is rejected to keep the
            // search-CLI shape predictable — wildcard syntax (`*foo`)
            // already lets the user widen the match; a totally
            // empty input would skip BM25 entirely and that's a
            // separate command (`index list`).
            let q_trimmed = query.trim();
            if q_trimmed.is_empty() {
                return Err(
                    "search query is empty — use `index list` to enumerate \
                     without BM25, or pass a wildcard like `*` to match all"
                        .into(),
                );
            }
            let hits = fts
                .search(q_trimmed, &filters, limit.saturating_mul(4))
                .map_err(|e| e.to_string())?;
            // Resolve doc metadata from LanceDB.  The Lance side
            // applies the SearchFilters scalar-SQL clauses; the
            // post-hoc filters (size + date) run in Rust against
            // each row's `metadata_json` blob.
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;

            // Fetch metadata for each hit + apply the SearchFilters
            // scalar SQL (ext, hash prefix, folder prefix, audio
            // duration range, image camera, source language, year
            // range, prefer-translated-lang) as a LanceDB-side
            // predicate.  Rows that don't pass the SQL are dropped
            // here, preserving the BM25 ranking on what remains.
            let doc_ids: Vec<String> = hits.iter().map(|h| h.doc_id.clone()).collect();
            let extra_sql = filters.to_lance_sql();
            let meta_map: std::collections::HashMap<String, crate::index::SearchResult> = local
                .fetch_search_results_by_ids_filtered(&doc_ids, extra_sql.as_deref())
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|r| (r.doc_id.clone(), r))
                .collect();
            let mut rows: Vec<crate::index::SearchResult> = hits
                .iter()
                .filter_map(|h| meta_map.get(&h.doc_id).cloned())
                .collect();

            // Post-hoc filter: size + date.  metadata_json shape:
            // `{"fs_size": int, "fs_mtime": unix_seconds, ...}`.
            // Parse cheaply (per-row JSON) — at CLI scale (10⁴ rows)
            // this is microseconds.
            if min_size_bytes.is_some() || max_size_bytes.is_some()
                || after_unix.is_some() || before_unix.is_some()
            {
                rows.retain(|r| {
                    let blob: serde_json::Value = r
                        .metadata_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(serde_json::Value::Null);
                    let fs_size = blob.get("fs_size").and_then(|v| v.as_i64());
                    let fs_mtime = blob.get("fs_mtime").and_then(|v| v.as_i64());
                    if let Some(min) = min_size_bytes {
                        if fs_size.map_or(true, |s| s < min) {
                            return false;
                        }
                    }
                    if let Some(max) = max_size_bytes {
                        if fs_size.map_or(true, |s| s > max) {
                            return false;
                        }
                    }
                    if let Some(after) = after_unix {
                        if fs_mtime.map_or(true, |m| m < after) {
                            return false;
                        }
                    }
                    if let Some(before) = before_unix {
                        if fs_mtime.map_or(true, |m| m > before) {
                            return false;
                        }
                    }
                    true
                });
            }
            rows.truncate(limit);

            // Render.
            match out {
                OutFormat::Json => {
                    for r in &rows {
                        let payload = serde_json::json!({
                            "doc_id": r.doc_id,
                            "filename": r.filename,
                            "title": r.title,
                            "author": r.author,
                            "year": r.year,
                            "ext": r.ext,
                            "language": r.language,
                            "location_uri": r.location_uri,
                            "snippet": r.snippet,
                            "score": r.score,
                        });
                        println!("{payload}");
                    }
                }
                OutFormat::Text => {
                    // Compact table — title (40) / year (4) / ext (8) /
                    // size human-readable / lang / filename.
                    println!(
                        "{:<40}  {:>4}  {:<6}  {:>10}  {:<4}  {}",
                        "TITLE", "YEAR", "EXT", "SIZE", "LANG", "FILENAME"
                    );
                    for r in &rows {
                        let blob: serde_json::Value = r
                            .metadata_json
                            .as_deref()
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or(serde_json::Value::Null);
                        let fs_size = blob.get("fs_size").and_then(|v| v.as_i64());
                        let title = r
                            .title
                            .as_deref()
                            .or(r.filename.as_deref())
                            .unwrap_or(&r.doc_id);
                        let title_short: String = title.chars().take(40).collect();
                        let year = r.year.map(|y| y.to_string()).unwrap_or_default();
                        let ext = r.ext.as_deref().unwrap_or("");
                        let size_h = fs_size.map(format_size_human).unwrap_or_default();
                        let lang = r.language.as_deref().unwrap_or("");
                        let filename = r.filename.as_deref().unwrap_or("");
                        println!(
                            "{:<40}  {:>4}  {:<6}  {:>10}  {:<4}  {}",
                            title_short, year, ext, size_h, lang, filename
                        );
                    }
                }
            }
            eprintln!("{} result(s)", rows.len());
        }

        IndexCmd::Ingest { paths, owner_id, model, device } => {
            use crate::index::embedder::{EmbedderConfig, EmbedderDevice, EmbedderModel};
            use crate::index::ingest::{IngestConfig, IngestPipeline, RawDocument};
            use sha2::{Digest, Sha256};

            let owner = owner_id.clone().unwrap_or_else(|| uuid::Uuid::nil().to_string());

            // Parse model + device (same logic as Init).
            let m = match model.to_ascii_lowercase().replace('-', "_").as_str() {
                "bge_m3" | "bgem3"          => EmbedderModel::BgeM3,
                "multilingual_e5_small"     => EmbedderModel::MultilingualE5Small,
                "multilingual_e5_base"      => EmbedderModel::MultilingualE5Base,
                "multilingual_e5_large"     => EmbedderModel::MultilingualE5Large,
                "bge_small_en_v1.5" | "bge_small_en" => EmbedderModel::BgeSmallEnV15,
                "bge_base_en_v1.5"  | "bge_base_en"  => EmbedderModel::BgeBaseEnV15,
                "bge_large_en_v1.5" | "bge_large_en" => EmbedderModel::BgeLargeEnV15,
                "nomic_embed_text_v1.5" | "nomic"     => EmbedderModel::NomicEmbedTextV15,
                "all_minilm_l6_v2"  | "minilm"        => EmbedderModel::AllMiniLmL6V2,
                _ => EmbedderModel::BgeM3,
            };
            let d = match device.to_ascii_lowercase().as_str() {
                "cuda"          => EmbedderDevice::Cuda,
                "metal" | "mps" => EmbedderDevice::Metal,
                "auto"          => EmbedderDevice::Auto,
                _               => EmbedderDevice::Cpu,
            };

            let cache_dir = data_dir.join("models");
            std::fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
            let embedder_config = EmbedderConfig::new(m, d, cache_dir);
            eprintln!("loading embedder model ({})…", model);
            let embedder = crate::index::embedder::Embedder::new(embedder_config)
                .await.map_err(|e| e.to_string())?;
            let embedder_arc = std::sync::Arc::new(tokio::sync::Mutex::new(embedder));

            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await.map_err(|e| e.to_string())?;
            let fts = crate::index::FtsIndex::open_or_create(&data_dir.join("fts"))
                .map_err(|e| e.to_string())?;
            let pipeline = IngestPipeline::new(
                std::sync::Arc::new(fts),
                std::sync::Arc::new(local),
                Some(embedder_arc),
                IngestConfig::default(),
            );

            // Collect all files.
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            for path in &paths {
                if path.is_dir() {
                    for entry in jwalk::WalkDir::new(path)
                        .into_iter().filter_map(|e| e.ok())
                        .filter(|e| e.file_type().is_file())
                    {
                        files.push(entry.path().to_path_buf());
                    }
                } else if path.exists() {
                    files.push(path.clone());
                } else {
                    eprintln!("skip (not found): {}", path.display());
                }
            }
            eprintln!("ingesting {} file(s)…", files.len());

            let mut ok = 0usize;
            let mut errs = 0usize;
            for p in &files {
                let bytes = match std::fs::read(p) {
                    Ok(b) => b, Err(e) => { eprintln!("skip {}: {e}", p.display()); errs += 1; continue; }
                };
                let mut h = Sha256::new(); h.update(&bytes);
                let source_hash = hex::encode(h.finalize());
                let extracted = match tokio::task::spawn_blocking({
                    let pp = p.clone();
                    move || crate::extractors::extract_text_from_path(&pp)
                }).await {
                    Ok(Ok(e)) => e,
                    Ok(Err(e)) => { eprintln!("skip {}: {e}", p.display()); errs += 1; continue; }
                    Err(e)    => { eprintln!("skip {}: {e}", p.display()); errs += 1; continue; }
                };
                let loc = crate::index::location::FileLocation::Local {
                    user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()),
                    machine_id: uuid::Uuid::nil(),
                    path: p.clone(),
                };
                let meta = p.metadata().ok();
                let raw = RawDocument {
                    full_text: extracted.full_text, full_text_md: String::new(),
                    headings: extracted.headings, title: None, author: None, year: None,
                    filename: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    ext: extracted.ext, language: String::new(),
                    source_hash, location_uri: loc.to_uri(), owner_id: owner.clone(), tags: vec![],
                    mtime_unix: meta.as_ref().and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64),
                    file_size: meta.map(|m| m.len() as i64),
                    volume_id: crate::volume::volume_id_for_path(p),
                    parent_dir: p.parent().and_then(|d| d.to_str()).map(|s| s.to_owned()),
                    translated_text: extracted.translated_text,
                    translated_to_lang: extracted.translated_to_lang,
                    // P13.6 Step 7 — audio L2 from the symphonia probe.
                    audio_duration_seconds: extracted.audio.as_ref().and_then(|a| a.duration_seconds),
                    audio_codec: extracted.audio.as_ref().and_then(|a| a.codec.clone()),
                    audio_sample_rate_hz: extracted.audio.as_ref().and_then(|a| a.sample_rate_hz.map(|s| s as i32)),
                    audio_channels: extracted.audio.as_ref().and_then(|a| a.channels.map(|c| c as i32)),
                    audio_bitrate_kbps: extracted.audio.as_ref().and_then(|a| a.bitrate_kbps.map(|b| b as i32)),
                    // P13.6 Step 9 — image L2 (EXIF) curated subset.
                    image_camera_make:  extracted.image_exif.as_ref().and_then(|e| e.camera_make.clone()),
                    image_camera_model: extracted.image_exif.as_ref().and_then(|e| e.camera_model.clone()),
                    image_lens_model:   extracted.image_exif.as_ref().and_then(|e| e.lens_model.clone()),
                    image_taken_at_unix: extracted.image_exif.as_ref().and_then(|e| e.taken_at_unix),
                    image_iso:          extracted.image_exif.as_ref().and_then(|e| e.iso.map(|i| i as i32)),
                };
                match pipeline.ingest_document(raw).await {
                    Ok(stats) => {
                        match out {
                            OutFormat::Json => println!("{}", serde_json::json!({
                                "path": p.display().to_string(), "chunks": stats.chunk_count
                            })),
                            OutFormat::Text => println!("✓ {} ({} chunks)", p.display(), stats.chunk_count),
                        }
                        ok += 1;
                    }
                    Err(e) => { eprintln!("error {}: {e}", p.display()); errs += 1; }
                }
            }
            eprintln!("{ok} ingested, {errs} errors");
        }

        IndexCmd::Init { model, device } => {
            use crate::index::embedder::{EmbedderConfig, EmbedderDevice, EmbedderModel};

            // Parse model name (common aliases).
            let m = match model.to_ascii_lowercase().replace('-', "_").as_str() {
                "bge_m3" | "bgem3"          => EmbedderModel::BgeM3,
                "multilingual_e5_small"     => EmbedderModel::MultilingualE5Small,
                "multilingual_e5_base"      => EmbedderModel::MultilingualE5Base,
                "multilingual_e5_large"     => EmbedderModel::MultilingualE5Large,
                "bge_small_en_v1.5" | "bge_small_en" => EmbedderModel::BgeSmallEnV15,
                "bge_base_en_v1.5"  | "bge_base_en"  => EmbedderModel::BgeBaseEnV15,
                "bge_large_en_v1.5" | "bge_large_en" => EmbedderModel::BgeLargeEnV15,
                "nomic_embed_text_v1.5" | "nomic"     => EmbedderModel::NomicEmbedTextV15,
                "all_minilm_l6_v2" | "minilm"         => EmbedderModel::AllMiniLmL6V2,
                _ => return Err(format!(
                    "unknown model '{model}'. Try: bge-m3, multilingual-e5-base, bge-small-en-v1.5, …"
                )),
            };

            // Parse device.
            let d = match device.to_ascii_lowercase().as_str() {
                "cuda"           => EmbedderDevice::Cuda,
                "metal" | "mps"  => EmbedderDevice::Metal,
                "auto"           => EmbedderDevice::Auto,
                _                => EmbedderDevice::Cpu,
            };

            let cache = data_dir.join("models");
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
            eprintln!("downloading {} (device={:?}) → {}", model, d, cache.display());

            let config = EmbedderConfig::new(m, d, cache.clone());
            // Embedder::new is async and downloads the model on first call.
            let _embedder = crate::index::embedder::Embedder::new(config)
                .await
                .map_err(|e| e.to_string())?;

            match out {
                OutFormat::Json => println!("{}", serde_json::json!({
                    "model": model, "cache": cache.display().to_string(), "status": "ready"
                })),
                OutFormat::Text => println!("model '{}' ready in {}", model, cache.display()),
            }
        }

        IndexCmd::ListFailed { retryable_only } => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            let rows = local
                .list_failed_extractions(retryable_only)
                .await
                .map_err(|e| e.to_string())?;
            for r in &rows {
                match out {
                    OutFormat::Json => {
                        println!("{}", serde_json::json!({
                            "doc_id":       r.doc_id,
                            "reason":       r.reason,
                            "retryable":    r.retryable,
                            "filename":     r.filename,
                            "location_uri": r.location_uri,
                        }));
                    }
                    OutFormat::Text => {
                        let label = if r.retryable { "(retryable)" } else { "          " };
                        let name = r.filename.as_deref().unwrap_or(&r.location_uri);
                        println!("{:<12} {} {}", r.reason, label, name);
                    }
                }
            }
            eprintln!("{} failed extraction(s){}",
                rows.len(),
                if retryable_only { " (retryable only)" } else { "" });
        }

        IndexCmd::RetryFailed { dry_run } => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            if dry_run {
                let rows = local.list_failed_extractions(true).await.map_err(|e| e.to_string())?;
                for r in &rows {
                    println!("would retry: {} ({})", r.filename.as_deref().unwrap_or(&r.doc_id), r.reason);
                }
                eprintln!("{} row(s) would be retried (dry-run)", rows.len());
            } else {
                let n = local.retry_all_failed_extractions().await.map_err(|e| e.to_string())?;
                match out {
                    OutFormat::Json => println!("{}", serde_json::json!({ "retried": n })),
                    OutFormat::Text => println!("cleared {n} failed extraction(s) — bg_ingest will re-attempt on next run"),
                }
            }
        }

        IndexCmd::SkipFailed { dry_run } => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            // List retryable failures (timeout / other).
            let rows = local.list_failed_extractions(true).await.map_err(|e| e.to_string())?;
            if dry_run {
                for r in &rows {
                    println!("would mark permanent: {} ({})", r.filename.as_deref().unwrap_or(&r.doc_id), r.reason);
                }
                eprintln!("{} row(s) would be permanently skipped (dry-run)", rows.len());
            } else {
                // Mark each row's extraction_failure.reason as "unsupported"
                // so the worker treats it as non-retryable on future runs.
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                let new_meta = format!(
                    r#"{{"level":2,"extraction_failure":{{"reason":"unsupported","msg":"manually skipped via CLI","at":{}}}}}"#,
                    now_ms
                );
                let mut n = 0usize;
                for row in &rows {
                    let _ = local.update_l2_fields(
                        &row.doc_id,
                        None, None, None, None, None,
                        Some(&new_meta),
                    ).await;
                    n += 1;
                }
                match out {
                    OutFormat::Json => println!("{}", serde_json::json!({ "marked_permanent": n })),
                    OutFormat::Text => println!("marked {n} row(s) as permanently skipped"),
                }
            }
        }

        IndexCmd::ExportCidx { dest, volume_id, include_embeddings, include_fts } => {
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await
                .map_err(|e| e.to_string())?;
            let rows = local
                .export_cidx(&dest, volume_id.as_deref(), include_embeddings, include_fts)
                .await
                .map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::json!({
                        "dest": dest.display().to_string(),
                        "rows_exported": rows,
                        "volume_id": volume_id,
                        "embeddings": include_embeddings,
                        "fts": include_fts,
                    }));
                }
                OutFormat::Text => {
                    println!("exported {rows} row(s) → {}{}",
                        dest.display(),
                        if include_fts { " (+ FTS index)" } else { "" });
                }
            }
        }

        IndexCmd::IngestCbManifest { manifest_db, owner_id } => {
            use rusqlite::{Connection, OpenFlags};
            if !manifest_db.exists() {
                return Err(format!("not found: {}", manifest_db.display()));
            }
            let owner = owner_id.clone().unwrap_or_else(|| uuid::Uuid::nil().to_string());
            eprintln!("opening manifest: {}", manifest_db.display());
            let conn = Connection::open_with_flags(&manifest_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| format!("open sqlite: {e}"))?;
            let mut stmt = conn.prepare(
                "SELECT file_path, file_size_bytes, modified_time, file_hash, archived_in
                 FROM source_files WHERE status NOT IN ('deleted','error') ORDER BY file_path"
            ).map_err(|e| e.to_string())?;
            let rows: Vec<(String,i64,f64,Option<String>,Option<i64>)> = stmt
                .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))
                .map_err(|e| e.to_string())?
                .filter_map(|r| r.ok())
                .collect();
            eprintln!("found {} rows", rows.len());
            let local = crate::index::LocalIndex::open_or_create(&data_dir, 1024)
                .await.map_err(|e| e.to_string())?;
            let fts = crate::index::FtsIndex::open_or_create(&data_dir.join("fts"))
                .map_err(|e| e.to_string())?;
            let pipe = crate::index::IngestPipeline::new(
                std::sync::Arc::new(fts),
                std::sync::Arc::new(local),
                None,
                crate::index::ingest::IngestConfig::default(),
            );
            let mut ingested = 0usize;
            const BATCH: usize = 64;
            for chunk in rows.chunks(BATCH) {
                let entries: Vec<crate::index::ingest::L1FileEntry> = chunk.iter().map(|(path,size,mtime,hash,archived_in)| {
                    let hash_str = hash.clone().unwrap_or_default();
                    let doc_id = if hash_str.is_empty() { uuid::Uuid::new_v4().to_string() } else { hash_str.clone() };
                    let location_uri = if let Some(aid) = archived_in {
                        crate::index::location::FileLocation::CbArchive { archive_id: *aid, file_hash: hash_str.clone(), original_path: path.clone() }.to_uri()
                    } else {
                        crate::index::location::FileLocation::Local { user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()), machine_id: uuid::Uuid::nil(), path: std::path::PathBuf::from(path) }.to_uri()
                    };
                    let p = std::path::Path::new(path);
                    crate::index::ingest::L1FileEntry {
                        doc_id, location_uri, owner_id: owner.clone(),
                        filename: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        ext: p.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_default(),
                        source_hash: hash_str,
                        mtime_ms: (*mtime * 1000.0) as i64, ctime_ms: 0, size: *size,
                        parent_dir: p.parent().and_then(|d| d.to_str()).unwrap_or("").to_owned(),
                        volume_id: None,
                    }
                }).collect();
                if let Ok(stats) = pipe.ingest_l1(&entries).await { ingested += stats.chunk_count; }
            }
            match out {
                OutFormat::Json => println!("{}", serde_json::json!({ "ingested": ingested, "total": rows.len() })),
                OutFormat::Text => println!("ingested {ingested} / {} rows from manifest", rows.len()),
            }
        }

        IndexCmd::InspectCidx { path } => {
            let idx = crate::index::LocalIndex::open_cidx(&path)
                .await
                .map_err(|e| e.to_string())?;
            let chunks = idx.count().await.map_err(|e| e.to_string())?;
            let docs = idx.count_docs().await.map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::json!({
                        "path": path.display().to_string(),
                        "docs": docs,
                        "chunks": chunks,
                    }));
                }
                OutFormat::Text => {
                    println!("Documents : {docs}");
                    println!("Chunks    : {chunks}");
                    println!("Path      : {}", path.display());
                }
            }
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

// ── images (P13) ──────────────────────────────────────────────────────────

fn cmd_images(
    out: OutFormat,
    data_dir: Option<PathBuf>,
    cmd: ImagesCmd,
) -> Result<(), String> {
    // Subcommands that don't need the index: extensions / thumbnail / exif.
    // They get short-circuited here so a missing data dir is never an
    // obstacle for the file-mode operations.  Borrow patterns so `cmd`
    // stays available for the index-backed path below.
    if matches!(cmd, ImagesCmd::Extensions) {
        let exts = crate::images::IMAGE_EXTS;
        match out {
            OutFormat::Json => {
                println!("{}", serde_json::json!({ "extensions": exts }));
            }
            OutFormat::Text => {
                for e in exts {
                    println!("{e}");
                }
            }
        }
        return Ok(());
    }
    if let ImagesCmd::Thumbnail { path, size, out: out_path } = &cmd {
        return cmd_images_thumbnail_file(path, *size, out_path.as_deref());
    }
    if let ImagesCmd::Exif { path } = &cmd {
        return cmd_images_exif_file(out, path);
    }

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
    rt.block_on(cmd_images_async(out, data_dir, cmd))
}

async fn cmd_images_async(
    out: OutFormat,
    data_dir: PathBuf,
    cmd: ImagesCmd,
) -> Result<(), String> {
    use std::sync::Arc;
    let local = Arc::new(
        crate::index::LocalIndex::open_or_create(&data_dir, 1024)
            .await
            .map_err(|e| e.to_string())?,
    );
    let backend = crate::images::local::LocalImages::new(local);
    use crate::images::{types::ListFilters, ImagesBackend};

    let folder_to_prefix = |p: Option<PathBuf>| p.map(|f| f.to_string_lossy().into_owned());

    match cmd {
        ImagesCmd::Extensions
        | ImagesCmd::Thumbnail { .. }
        | ImagesCmd::Exif { .. } => {
            unreachable!("file-mode subcommand handled in cmd_images before runtime")
        }

        ImagesCmd::Crisplens { cmd } => {
            return cmd_images_crisplens(out, &data_dir, cmd).await;
        }

        ImagesCmd::NearDuplicates { threshold, ext, folder } => {
            let filters = ListFilters {
                parent_dir_prefix: folder_to_prefix(folder),
                ext: if ext.is_empty() { None } else { Some(ext) },
                ..Default::default()
            };
            let groups = backend
                .near_duplicates(threshold, filters)
                .await
                .map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => {
                    let payload = serde_json::json!({
                        "group_count": groups.len(),
                        "threshold":   threshold,
                        "groups":      groups,
                    });
                    println!("{payload}");
                }
                OutFormat::Text => {
                    if groups.is_empty() {
                        println!("no near-duplicate clusters at threshold {threshold}");
                    } else {
                        println!("{} near-dup clusters (threshold {threshold}):", groups.len());
                        for g in &groups {
                            println!();
                            println!("  rep phash: 0x{}  ({} members)",
                                g.representative_phash_hex, g.items.len());
                            for it in &g.items {
                                let name = it.image.filename.as_deref().unwrap_or(&it.image.doc_id);
                                let ext = it.image.ext.as_deref().unwrap_or("?");
                                println!("    .{ext:<6} d={:<3} {name}\t{}",
                                    it.distance_from_rep, it.image.location_uri);
                            }
                        }
                    }
                }
            }
        }

        ImagesCmd::Duplicates { ext, folder } => {
            let filters = ListFilters {
                parent_dir_prefix: folder_to_prefix(folder),
                ext: if ext.is_empty() { None } else { Some(ext) },
                ..Default::default()
            };
            let groups = backend
                .duplicates(filters)
                .await
                .map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => {
                    let payload = serde_json::json!({
                        "group_count": groups.len(),
                        "groups":       groups,
                    });
                    println!("{payload}");
                }
                OutFormat::Text => {
                    if groups.is_empty() {
                        println!("no duplicate image rows found");
                    } else {
                        println!("{} duplicate groups:", groups.len());
                        for g in &groups {
                            println!();
                            println!("  hash: {}  ({} copies)", g.source_hash, g.items.len());
                            for img in &g.items {
                                let name = img.filename.as_deref().unwrap_or(&img.doc_id);
                                let ext = img.ext.as_deref().unwrap_or("?");
                                println!("    .{ext:<6} {name}\t{}", img.location_uri);
                            }
                        }
                    }
                }
            }
        }

        ImagesCmd::Count { ext, folder } => {
            // Pull a tiny page just to read `total`; LanceDB's
            // count_rows backs `total` so the per-row cost is zero
            // regardless of `page_size`.  We still set page_size = 1
            // to avoid materialising rows we throw away.
            let filters = ListFilters {
                parent_dir_prefix: folder_to_prefix(folder),
                ext: if ext.is_empty() { None } else { Some(ext) },
                ..Default::default()
            };
            let page = backend
                .list(1, None, filters)
                .await
                .map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "total": page.total,
                            "extensions": crate::images::IMAGE_EXTS,
                        })
                    );
                }
                OutFormat::Text => {
                    println!("{} image rows", page.total);
                }
            }
        }

        ImagesCmd::List { limit, ext, folder } => {
            // Walk pages until we've printed `limit` rows or hit the
            // end of the result set.  Each page is bounded at 200 rows
            // server-side; this lets the user ask for `--limit 5` or
            // `--limit 50000` with the same code path.
            let page_size = limit.min(200).max(1) as i32;
            let filters = ListFilters {
                parent_dir_prefix: folder_to_prefix(folder),
                ext: if ext.is_empty() { None } else { Some(ext) },
                ..Default::default()
            };

            let mut printed = 0usize;
            let mut cursor: Option<String> = None;
            loop {
                let page = backend
                    .list(page_size, cursor.clone(), filters.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                if page.items.is_empty() {
                    break;
                }
                for img in &page.items {
                    if printed >= limit {
                        break;
                    }
                    match out {
                        OutFormat::Json => {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "doc_id":      img.doc_id,
                                    "filename":    img.filename,
                                    "ext":         img.ext,
                                    "size":        img.size,
                                    "indexed_at":  img.indexed_at,
                                    "location_uri": img.location_uri,
                                })
                            );
                        }
                        OutFormat::Text => {
                            let name = img.filename.as_deref().unwrap_or(&img.doc_id);
                            let ext = img.ext.as_deref().unwrap_or("?");
                            let size = img
                                .size
                                .map(|s| format!("{} B", s))
                                .unwrap_or_else(|| "?".to_owned());
                            println!("{name}\t.{ext}\t{size}\t{}", img.location_uri);
                        }
                    }
                    printed += 1;
                }
                if printed >= limit || page.next_cursor.is_none() {
                    break;
                }
                cursor = page.next_cursor;
            }
        }
    }
    Ok(())
}

/// Synchronously run the thumbnail pipeline on a single file.  Writes
/// raw PNG bytes to `out_path` if supplied, else to stdout (the user's
/// terminal will redraw badly — see the doc-comment on the subcommand).
fn cmd_images_thumbnail_file(
    path: &std::path::Path,
    size: u32,
    out_path: Option<&std::path::Path>,
) -> Result<(), String> {
    use std::io::Write;
    let bytes = crate::images::thumbnail::generate_thumbnail(path, size)
        .map_err(|e| e.to_string())?;
    match out_path {
        Some(p) => std::fs::write(p, &bytes).map_err(|e| format!("write {}: {e}", p.display())),
        None => std::io::stdout()
            .write_all(&bytes)
            .map_err(|e| format!("stdout write: {e}")),
    }
}

/// Synchronously read EXIF from a file and print the curated subset.
fn cmd_images_exif_file(out: OutFormat, path: &std::path::Path) -> Result<(), String> {
    let summary = crate::images::exif::read_exif(path).map_err(|e| e.to_string())?;
    match out {
        OutFormat::Json => {
            let json = serde_json::to_string(&summary)
                .map_err(|e| format!("serialize exif: {e}"))?;
            println!("{json}");
        }
        OutFormat::Text => {
            if summary.is_empty() {
                println!("(no EXIF block)");
                return Ok(());
            }
            macro_rules! row {
                ($label:expr, $val:expr) => {
                    if let Some(v) = $val {
                        println!("{:<18} {}", $label, v);
                    }
                };
            }
            row!("Camera make:",  summary.camera_make.as_ref());
            row!("Camera model:", summary.camera_model.as_ref());
            row!("Lens:",         summary.lens_model.as_ref());
            row!("Taken at:",     summary.taken_at.as_ref());
            row!("Taken (unix):", summary.taken_at_unix.map(|v| v.to_string()).as_ref());
            row!("Dimensions:",   summary.width.zip(summary.height).map(|(w, h)| format!("{w} × {h}")).as_ref());
            row!("Aperture:",     summary.f_number.map(|v| format!("f/{v:.1}")).as_ref());
            row!("Exposure:",     summary.exposure_time.as_ref());
            row!("ISO:",          summary.iso.map(|v| v.to_string()).as_ref());
            row!("Focal length:", summary.focal_length_mm.map(|v| format!("{v:.1} mm")).as_ref());
            row!("GPS lat:",      summary.gps_lat.map(|v| format!("{v:.6}")).as_ref());
            row!("GPS lon:",      summary.gps_lon.map(|v| format!("{v:.6}")).as_ref());
            row!("Orientation:",  summary.orientation.map(|v| v.to_string()).as_ref());
        }
    }
    Ok(())
}

// ── images crisplens (P13/B1) ─────────────────────────────────────────────

async fn cmd_images_crisplens(
    out: OutFormat,
    data_dir: &std::path::Path,
    cmd: CrispLensCmd,
) -> Result<(), String> {
    use crate::images::crisplens::{
        secret,
        settings::{self, ImagesBackend, ImagesSettings},
        tauri_commands::{login_blocking, logout_blocking},
    };

    match cmd {
        CrispLensCmd::Settings => {
            let s = settings::load(data_dir);
            // Augment the displayed payload with the session-status
            // boolean so the user gets the whole picture in one
            // command.  Cookie value itself never leaks.
            let url = s.normalised_url().to_owned();
            let authenticated = if url.is_empty() {
                false
            } else {
                matches!(secret::get_session_for_url(&url), Ok(Some(_)))
            };
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::json!({
                        "settings":      s,
                        "authenticated": authenticated,
                    }));
                }
                OutFormat::Text => {
                    println!("backend            : {:?}", s.backend);
                    println!("url                : {}", s.url);
                    println!("thumbnail_size_px  : {}", s.thumbnail_size_px);
                    println!("phash_threshold    : {}", s.phash_threshold);
                    println!("authenticated      : {}", authenticated);
                }
            }
        }

        CrispLensCmd::SetUrl { url, enable } => {
            let mut s = settings::load(data_dir);
            s.url = url;
            if enable {
                s.backend = ImagesBackend::CrispLens;
            }
            settings::save(data_dir, &s).map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&s).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    println!("ok — URL set; backend = {:?}", s.backend);
                }
            }
        }

        CrispLensCmd::Disable => {
            let mut s = settings::load(data_dir);
            s.backend = ImagesBackend::Local;
            settings::save(data_dir, &s).map_err(|e| e.to_string())?;
            match out {
                OutFormat::Json => println!("{}", serde_json::json!({"backend": "local"})),
                OutFormat::Text => println!("ok — backend switched to local"),
            }
        }

        CrispLensCmd::SessionStatus => {
            let s = settings::load(data_dir);
            let url = s.normalised_url().to_owned();
            let authenticated = !url.is_empty()
                && matches!(secret::get_session_for_url(&url), Ok(Some(_)));
            match out {
                OutFormat::Json => println!("{}", serde_json::json!({
                    "url":           url,
                    "authenticated": authenticated,
                })),
                OutFormat::Text => {
                    if url.is_empty() {
                        println!("(no CrispLens URL configured)");
                    } else {
                        println!("{url}: {}", if authenticated { "authenticated" } else { "no stored session" });
                    }
                }
            }
        }

        CrispLensCmd::Login { user, password } => {
            let s = settings::load(data_dir);
            let url = s.normalised_url().to_owned();
            if url.is_empty() {
                return Err(
                    "no CrispLens URL configured — run \
                     `crispsorter images crisplens set-url <URL> --enable` first".to_string(),
                );
            }
            // Password resolution order: --password flag, then
            // CRISPLENS_PASSWORD env var.  Never read from a
            // positional arg (would leak in `ps`/shell history).
            let pw = password
                .or_else(|| std::env::var("CRISPLENS_PASSWORD").ok())
                .ok_or_else(|| {
                    "no password provided (use --password or set $CRISPLENS_PASSWORD)".to_string()
                })?;
            let url_for_blocking = url.clone();
            let user_for_blocking = user.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                login_blocking(&url_for_blocking, &user_for_blocking, &pw)
            })
            .await
            .map_err(|e| format!("login join: {e}"))??;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&outcome).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    println!("logged in as {} ({})", outcome.username, outcome.role);
                }
            }
        }

        CrispLensCmd::Status => {
            use crate::images::crisplens::tauri_commands::status_blocking;
            let dd = data_dir.to_path_buf();
            let status = tokio::task::spawn_blocking(move || status_blocking(&dd))
                .await
                .map_err(|e| format!("status join: {e}"))?;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&status).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    if !status.tier2_configured {
                        println!("tier 2 not configured (run `crisplens set-url <URL> --enable`)");
                        return Ok(());
                    }
                    let health = match status.health_ok {
                        Some(true) => "ok",
                        Some(false) => "FAILED",
                        None => "(unknown)",
                    };
                    println!("health         : {health}");
                    if let Some(v) = &status.health_version {
                        let backend = status.health_backend.as_deref().unwrap_or("(unknown)");
                        println!("server         : {v} ({backend})");
                    }
                    if let Some(ready) = status.health_model_ready {
                        println!("model_ready    : {ready}");
                    }
                    println!("authenticated  : {}", status.authenticated);
                    if let Some(u) = &status.username {
                        println!("user           : {u} ({})", status.role.as_deref().unwrap_or(""));
                    }
                    if !status.error.is_empty() {
                        println!("note           : {}", status.error);
                    }
                }
            }
        }

        CrispLensCmd::Watchfolders => {
            use crate::images::crisplens::tauri_commands::watchfolders_blocking;
            let dd = data_dir.to_path_buf();
            let folders = tokio::task::spawn_blocking(move || watchfolders_blocking(&dd))
                .await
                .map_err(|e| format!("watchfolders join: {e}"))??;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&folders).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    if folders.is_empty() {
                        println!("(no watchfolders configured on the CrispLens server)");
                    } else {
                        println!("{} watchfolder(s):", folders.len());
                        for f in &folders {
                            let rec  = f.recursive_bool().map(|b| if b { "rec" } else { "flat" }).unwrap_or("?");
                            let auto = f.auto_scan_bool().map(|b| if b { "auto" } else { "manual" }).unwrap_or("?");
                            let en   = f.enabled_bool().map(|b| if b { "on" } else { "off" }).unwrap_or("?");
                            println!("  [{:>3}] {:4} {:6} {:3}  {}",
                                f.id.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                                rec, auto, en, f.path);
                        }
                    }
                }
            }
        }

        CrispLensCmd::People => {
            use crate::images::crisplens::tauri_commands::get_json;
            use crisplens_protocol::Person;
            let dd = data_dir.to_path_buf();
            let people: Vec<Person> = tokio::task::spawn_blocking(move || {
                get_json::<Vec<Person>>(&dd, "/api/people")
            })
            .await
            .map_err(|e| format!("people join: {e}"))??;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&people).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    if people.is_empty() {
                        println!("(no people on the CrispLens server, or not authenticated)");
                    } else {
                        println!("{} person cluster(s):", people.len());
                        for p in &people {
                            let app = p.appearances.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
                            println!("  [{:>3}] {:>4}×  {}", p.id, app, p.name);
                        }
                    }
                }
            }
        }

        CrispLensCmd::ImageFaces { image_id } => {
            use crate::images::crisplens::tauri_commands::get_json;
            use crisplens_protocol::Face;
            let dd = data_dir.to_path_buf();
            let path = format!("/api/images/{image_id}/faces");
            let faces: Vec<Face> = tokio::task::spawn_blocking(move || {
                get_json::<Vec<Face>>(&dd, &path)
            })
            .await
            .map_err(|e| format!("faces join: {e}"))??;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&faces).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    if faces.is_empty() {
                        println!("(no faces detected in image {image_id})");
                    } else {
                        println!("{} face(s) in image {image_id}:", faces.len());
                        for f in &faces {
                            let person = f.person_name.as_deref().unwrap_or("(unknown)");
                            let conf   = f.detection_confidence.map(|c| format!("{c:.2}")).unwrap_or_else(|| "?".into());
                            let verif  = f.verified_bool().map(|b| if b { "✓" } else { "·" }).unwrap_or("·");
                            println!("  [{:>3}] {verif} det={conf}  bbox=t{:.2},r{:.2},b{:.2},l{:.2}  {}",
                                f.face_id, f.bbox.top, f.bbox.right, f.bbox.bottom, f.bbox.left, person);
                        }
                    }
                }
            }
        }

        CrispLensCmd::ImageByHash { sha256, from_file } => {
            use crate::images::crisplens::tauri_commands::{
                get_json_inner, is_lowercase_sha256_hex, sha256_file,
            };
            use crisplens_protocol::Image as CrispLensImage;

            // Resolve which hash we're actually looking up:
            // explicit arg wins; otherwise hash --from-file.
            let dd = data_dir.to_path_buf();
            let sha = match (sha256, from_file) {
                (Some(s), _) => {
                    if !is_lowercase_sha256_hex(&s) {
                        return Err("not a 64-char lowercase hex SHA-256".into());
                    }
                    s
                }
                (None, Some(path)) => {
                    let p = path.to_string_lossy().into_owned();
                    tokio::task::spawn_blocking(move || sha256_file(&p))
                        .await
                        .map_err(|e| format!("sha join: {e}"))??
                }
                (None, None) => unreachable!("clap rejects when both are absent"),
            };
            let url_path = format!("/api/images/by-hash/{sha}");

            let resolved = tokio::task::spawn_blocking(move || {
                match get_json_inner::<CrispLensImage>(&dd, &url_path) {
                    Ok(opt) => Ok::<_, String>(opt),
                    Err(e) if e.contains("HTTP 404") || e.to_lowercase().contains("not found")
                        => Ok::<_, String>(None),
                    Err(e) => Err(e),
                }
            })
            .await
            .map_err(|e| format!("by-hash join: {e}"))??;

            match (out, &resolved) {
                (OutFormat::Json, _) => {
                    println!("{}", serde_json::to_string(&resolved).map_err(|e| e.to_string())?);
                }
                (OutFormat::Text, None) => {
                    println!("(no CrispLens image with that hash)");
                }
                (OutFormat::Text, Some(img)) => {
                    println!("CrispLens image #{} — {}", img.id, img.filename);
                    println!("  path     : {}", img.filepath);
                    if let Some(s) = img.file_size { println!("  size     : {s} B"); }
                    if let (Some(w), Some(h)) = (img.width, img.height) {
                        println!("  dims     : {w} × {h}");
                    }
                    if let Some(t) = &img.taken_at { println!("  taken_at : {t}"); }
                    if let Some(fc) = img.face_count { println!("  faces    : {fc}"); }
                }
            }
        }

        CrispLensCmd::Search { q, limit } => {
            use crate::images::crisplens::tauri_commands::get_json;
            use crisplens_protocol::SearchHit;
            let dd = data_dir.to_path_buf();
            let encoded_q: String = q
                .chars()
                .flat_map(|c| {
                    if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '~') {
                        vec![c]
                    } else {
                        format!("%{:02X}", c as u32).chars().collect()
                    }
                })
                .collect();
            let path = format!("/api/search?q={encoded_q}&limit={limit}");
            let hits: Vec<SearchHit> = tokio::task::spawn_blocking(move || {
                get_json::<Vec<SearchHit>>(&dd, &path)
            })
            .await
            .map_err(|e| format!("search join: {e}"))??;
            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::to_string(&hits).map_err(|e| e.to_string())?);
                }
                OutFormat::Text => {
                    if hits.is_empty() {
                        println!("(no matches for {q:?})");
                    } else {
                        println!("{} semantic match(es) for {q:?}:", hits.len());
                        for h in &hits {
                            let faces = h.face_count.map(|n| format!("{n}f")).unwrap_or_default();
                            let score = h.score.map(|s| format!("s={s:.3}")).unwrap_or_else(|| "      ".into());
                            println!("  [{:>4}] {score} {:6}  {}", h.id, faces, h.filename);
                        }
                    }
                }
            }
        }

        CrispLensCmd::Logout => {
            let s = settings::load(data_dir);
            let url = s.normalised_url().to_owned();
            if url.is_empty() {
                // Same convention as the Tauri command — empty URL
                // is no-op success.
                match out {
                    OutFormat::Json => println!("{}", serde_json::json!({"ok": true, "noop": "no URL configured"})),
                    OutFormat::Text => println!("ok (no URL configured — nothing to log out from)"),
                }
                return Ok(());
            }
            tokio::task::spawn_blocking(move || logout_blocking(&url))
                .await
                .map_err(|e| format!("logout join: {e}"))??;
            match out {
                OutFormat::Json => println!("{}", serde_json::json!({"ok": true})),
                OutFormat::Text => println!("ok — session cleared"),
            }
        }
    }
    Ok(())
}

// ── chat ──────────────────────────────────────────────────────────────────

fn cmd_chat(out: OutFormat, cmd: ChatCmd) -> Result<(), String> {
    match cmd {
        ChatCmd::Query { prompt, llm_url, llm_model, api_key, system, context_files } => {
            // Build context from optional files.
            let mut context = String::new();
            for path in &context_files {
                match crate::extractors::extract_text_from_path(path) {
                    Ok(doc) if !doc.full_text.is_empty() => {
                        context.push_str(&format!("\n--- {} ---\n{}\n", path.display(), doc.full_text));
                    }
                    _ => eprintln!("warning: could not extract text from {}", path.display()),
                }
            }

            let user_content = if context.is_empty() {
                prompt.clone()
            } else {
                format!("{prompt}\n\nContext:{context}")
            };

            let mut messages = Vec::new();
            if let Some(sys) = system {
                messages.push(serde_json::json!({"role": "system", "content": sys}));
            }
            messages.push(serde_json::json!({"role": "user", "content": user_content}));

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().map_err(|e| e.to_string())?;

            let reply = rt.block_on(async {
                let client = reqwest::Client::new();
                let body = serde_json::json!({
                    "model": llm_model,
                    "messages": messages,
                    "stream": false,
                });
                let mut req = client
                    .post(format!("{}/chat/completions", llm_url.trim_end_matches('/')))
                    .json(&body);
                if !api_key.is_empty() {
                    req = req.bearer_auth(&api_key);
                }
                let resp = req.send().await.map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("LLM returned {}", resp.status()));
                }
                let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                json["choices"][0]["message"]["content"]
                    .as_str()
                    .map(|s| s.to_owned())
                    .ok_or_else(|| "unexpected response shape".to_string())
            })?;

            match out {
                OutFormat::Json => println!("{}", serde_json::json!({
                    "prompt": prompt, "reply": reply
                })),
                OutFormat::Text => println!("{reply}"),
            }
        }
        ChatCmd::Transcribe {
            path, backend, model, language, output, data_dir, pure_rust, stream,
            transcript_format,
            policy, fallback_backend, lid_model, lid_method,
            translate_to, translate_backend, translate_model, translate_max_tokens,
        } => {
            cmd_chat_transcribe(
                out, path, backend, model, language, output, data_dir, pure_rust, stream,
                transcript_format,
                policy, fallback_backend, lid_model, lid_method,
                translate_to, translate_backend, translate_model, translate_max_tokens,
            )?;
        }
        ChatCmd::Tts {
            text, backend, model, voice, voice_ref_text, speaker, output, data_dir,
        } => {
            cmd_chat_tts(out, text, backend, model, voice, voice_ref_text, speaker, output, data_dir)?;
        }
    }
    Ok(())
}

// ── chat transcribe / tts (P13.5 slice A) ──────────────────────────────────

/// Resolve the model cache dir for the CLI's CrispASR session.  We
/// mirror the GUI's path (`<app-data>/models/`) so the same downloaded
/// GGUFs are shared between the two invocations — running the GUI
/// once seeds `whisper-base` for the CLI's first transcribe, etc.
fn asr_cache_dir(data_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    let dd = resolve_data_dir(data_dir)?;
    let cache = dd.join("models");
    std::fs::create_dir_all(&cache)
        .map_err(|e| format!("creating cache dir {}: {e}", cache.display()))?;
    Ok(cache)
}

/// True for the multilingual whisper variants whose ggml file is a
/// valid Whisper-method LID model.  distil-whisper is intentionally
/// EXCLUDED because its model is English-only (won't classify
/// anything else, defeats the point of auto-resolve).
///
/// Tracks the upstream `crispasr` registry — entries that come back
/// as multilingual ggmls live here; ones that don't get listed in
/// `Self::is_whisper_family` are filed under "use --lid-model PATH".
fn is_multilingual_whisper_backend(backend: &str) -> bool {
    matches!(
        backend,
        "whisper" | "whisper-base" | "whisper-small" | "whisper-medium" | "whisper-large-v3"
    )
}

/// Resolve a Whisper-method LID model path without the user supplying
/// `--lid-model PATH`.  Two strategies, tried in order:
///
///   1. **Reuse the loaded ASR model** when the user passed an
///      explicit `--model PATH` AND the configured ASR backend is a
///      multilingual whisper variant.  Whisper's LID surface
///      (`crispasr::detect_language_pcm` with `LidMethod::Whisper`)
///      accepts the same `ggml-*.bin` file the ASR side uses, so we
///      skip the redundant download.
///   2. **Auto-download `whisper-base`** via the CrispASR registry.
///      A small (≈150 MB) multilingual variant; cached after first
///      use so subsequent transcribes don't redownload.
///
/// Returns the on-disk path.  Errors propagate through the standard
/// anyhow chain so callers can wrap them with `format!("{e:#}")` to
/// surface the underlying registry / cache failure.
///
/// Stub for non-`crispasr` builds errors with the standard --features
/// hint — same pattern the other asr/extractor helpers use.
#[cfg(feature = "crispasr")]
async fn resolve_whisper_lid_model_path(
    asr_backend: &str,
    explicit_asr_model_path: &Option<PathBuf>,
    cache_dir: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context;

    // Path 1 — reuse the user's explicit ASR model when whisper-family.
    if let Some(p) = explicit_asr_model_path {
        if is_multilingual_whisper_backend(asr_backend) {
            // Trust-but-verify: the file should exist now, since
            // `Asr::load` would have errored already if not.
            if p.exists() {
                return Ok(p.clone());
            }
        }
    }

    // Path 2 — registry-lookup + cache_ensure_file on `whisper`.
    // The crispasr registry is sync; wrap in spawn_blocking so we
    // don't stall the async runtime.
    let cache_str = cache_dir.to_string_lossy().into_owned();
    let path = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let name = "whisper";
        let entry = crispasr::registry_lookup(name)
            .map_err(|e| anyhow::anyhow!("registry_lookup {name}: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "LID auto-resolve: `{name}` not in CrispASR registry — \
                     this is a build-time bug if the upstream registry \
                     doesn't list it"
                )
            })?;
        let path = crispasr::cache_ensure_file(
            &entry.filename,
            &entry.url,
            false,
            Some(&cache_str),
        )
        .map_err(|e| anyhow::anyhow!("cache_ensure_file for {}: {e}", entry.filename))?
        .ok_or_else(|| anyhow::anyhow!("cache returned no path for {}", entry.filename))?;
        Ok(path)
    })
    .await
    .context("spawn_blocking joined unexpectedly")??;
    Ok(PathBuf::from(path))
}

#[cfg(not(feature = "crispasr"))]
#[allow(dead_code)]
async fn resolve_whisper_lid_model_path(
    _asr_backend: &str,
    _explicit_asr_model_path: &Option<PathBuf>,
    _cache_dir: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    anyhow::bail!(
        "Whisper LID auto-resolve needs the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    )
}

#[allow(clippy::too_many_arguments)]
fn cmd_chat_transcribe(
    out: OutFormat,
    path: PathBuf,
    backend: String,
    model: Option<PathBuf>,
    language: Option<String>,
    output: String,
    data_dir: Option<PathBuf>,
    pure_rust: bool,
    stream: bool,
    transcript_format: Option<TranscriptFormat>,
    policy: LidPolicy,
    fallback_backend: String,
    lid_model: Option<PathBuf>,
    lid_method: LidMethodChoice,
    translate_to: Option<String>,
    translate_backend: String,
    translate_model: Option<PathBuf>,
    translate_max_tokens: i32,
) -> Result<(), String> {
    // Resolve which transcript format the user actually wants.  Per-
    // subcommand --transcript-format wins; falls back to the global
    // -f mapping (json → Json, text → Txt).  SRT/VTT require the
    // explicit flag — they're never the inferred default.
    let effective_fmt = transcript_format.unwrap_or_else(|| match out {
        OutFormat::Json => TranscriptFormat::Json,
        OutFormat::Text => TranscriptFormat::Txt,
    });
    // Stream mode + SRT/VTT is incoherent: segments only arrive at
    // the end of the buffered transcribe path.  Refuse early rather
    // than producing a single-segment subtitle file with the whole
    // transcript on one line.
    if stream && matches!(effective_fmt, TranscriptFormat::Srt | TranscriptFormat::Vtt) {
        return Err(
            "--stream is incompatible with --transcript-format srt|vtt — \
             SRT/VTT need per-segment timestamps that aren't available until \
             the buffered transcribe path completes.  Drop --stream OR use \
             --transcript-format txt|json."
                .to_string(),
        );
    }
    // ── Step 1: decode to 16 kHz mono Float32 ────────────────────────
    let decode_policy = if pure_rust {
        crate::audio::FallbackPolicy::PureRust
    } else {
        crate::audio::FallbackPolicy::AllowFfmpeg
    };
    eprintln!("decoding {}…", path.display());
    let decoded = crate::audio::decode_to_16khz_mono(&path, decode_policy)
        .map_err(|e| format!("audio decode: {e:#}"))?;
    eprintln!(
        "  → {} samples ({:.2} s) via {}",
        decoded.pcm.len(),
        decoded.duration_seconds,
        decoded.tier.as_str()
    );

    // ── Step 2: configure primary ASR handle ─────────────────────────
    let primary_config = match &model {
        Some(p) => crate::asr::AsrConfig::with_model_path(&backend, p.to_string_lossy()),
        None => crate::asr::AsrConfig::new(&backend),
    };
    let cache_dir = asr_cache_dir(data_dir.clone())?;
    let primary_handle = crate::asr::AsrHandle::new(primary_config.clone(), cache_dir);

    // ── Step 3: map CLI flags → orchestrator inputs ──────────────────
    //
    // `--language auto` is the magic word for "run LID"; any other
    // string is passed through as an ISO hint.  The orchestrator's
    // fast path handles `policy=AsConfigured` without LID either way,
    // so `--language auto --policy as-configured` is a no-op (LID
    // never runs); use `--policy auto|strict` to actually invoke it.
    let language_for_orchestrator = match language.as_deref() {
        Some("auto") => None,
        other => other.map(|s| s.to_string()),
    };

    let backend_policy = match policy {
        LidPolicy::AsConfigured => crate::asr::BackendFallback::AsConfigured,
        LidPolicy::Strict => crate::asr::BackendFallback::Strict,
        LidPolicy::Auto => crate::asr::BackendFallback::Auto {
            fallback: crate::asr::AsrConfig::new(&fallback_backend),
        },
    };

    // Runtime constructed early — we need it for the optional async
    // LID-model resolution below (text-LID auto-download via the
    // CrispASR registry), AND for the orchestrator's block_on.  Cheap
    // to construct, reuses across both calls so we don't pay the
    // setup twice.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    // P13.5 follow-up — audio-LID auto-resolution for the Whisper
    // method.  Three cases:
    //   1. User passed `--lid-model PATH` → use it verbatim.
    //   2. No --lid-model AND --lid-method whisper AND LID is
    //      actually going to fire (language=auto AND policy !=
    //      as-configured) → auto-resolve a Whisper ggml file.
    //      Reuses the user's explicit `--model PATH` when the ASR
    //      backend is whisper-family; otherwise downloads whisper-
    //      base via the CrispASR registry.
    //   3. No --lid-model AND --lid-method silero|ecapa|firered →
    //      `None` — orchestrator errors with a clear "needs
    //      --language or --lid-model" message because those LID
    //      models aren't in the upstream registry yet.
    let needs_lid =
        language_for_orchestrator.is_none() && !matches!(policy, LidPolicy::AsConfigured);
    let lid_options = match (lid_model, lid_method, needs_lid) {
        (Some(p), method, _) => Some(crate::asr::LidOptions {
            method: method.into_lib(),
            model_path: p,
            n_threads: 2,
        }),
        (None, LidMethodChoice::Whisper, true) => {
            let lid_cache = asr_cache_dir(data_dir.clone())?;
            let resolved = rt
                .block_on(resolve_whisper_lid_model_path(&backend, &model, &lid_cache))
                .map_err(|e| format!("Whisper LID auto-resolve: {e:#}"))?;
            eprintln!(
                "[lid] auto-resolved Whisper LID model → {}",
                resolved.display()
            );
            Some(crate::asr::LidOptions {
                method: crate::asr::LidMethod::Whisper,
                model_path: resolved,
                n_threads: 2,
            })
        }
        _ => None,
    };

    // ── Step 4: orchestrate (transcribe + optional LID + routing) ────

    // P13.5 follow-up — when --stream is set, skip the LID-routing
    // orchestrator and feed PCM directly to AsrHandle::transcribe_streaming.
    // Reasons:
    //   * Routing needs LID over the first ~10 s before deciding which
    //     backend to load, which defeats the "partials as soon as
    //     possible" UX --stream is for.
    //   * Streaming is whisper-only at the C-ABI level today; the
    //     auto-routing's Switch decision would have nothing to switch
    //     TO that also supports streaming, so the orchestrator's
    //     value-add is moot.
    //   * Translate post-processing happens AFTER the full transcript
    //     is in hand, which is incompatible with streaming-as-it-arrives.
    // If the user asks for both --stream and --policy != as-configured,
    // we emit a warning and ignore the policy (rather than refusing
    // both — streaming wins, since it's the more visible intent).
    if stream {
        if !matches!(policy, LidPolicy::AsConfigured) {
            eprintln!(
                "[chat transcribe] --stream and --policy {policy:?} both set; \
                 --stream wins (LID routing requires the full PCM before \
                 deciding).  Ignoring policy."
            );
        }
        if translate_to.is_some() {
            eprintln!(
                "[chat transcribe] --stream and --translate-to both set; \
                 translation happens AFTER the stream finishes, so partials \
                 will be in the source language."
            );
        }
        eprintln!(
            "transcribing (streaming) via {} …",
            primary_config.display_name()
        );
        let final_text = rt
            .block_on(primary_handle.transcribe_streaming(
                decoded.pcm,
                language.clone(),
                false, // translate-to-EN on whisper is opt-in via the
                       // --policy translate path; not exposed on --stream
                       // for the same reason translate_to is deferred above.
                |partial| {
                    eprint!("{partial}");
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                },
            ))
            .map_err(|e| format!("ASR streaming: {e:#}"))?;
        eprintln!(); // newline after the last partial

        // Optional translate post-processing on the final text.  Same
        // shape as the non-streaming branch below, just collapsed since
        // we don't have a TranscribeResult to thread through.
        let (final_emit, translation_meta) = if let Some(tgt) = translate_to.as_deref() {
            let src = language.clone().ok_or_else(|| {
                "--translate-to with --stream needs --language <ISO> (LID \
                 routing is bypassed when streaming)"
                    .to_string()
            })?;
            let translate_config = match &translate_model {
                Some(p) => crate::asr::AsrConfig::with_model_path(&translate_backend, p.to_string_lossy()),
                None => crate::asr::AsrConfig::new(&translate_backend),
            };
            let translate_cache_dir = asr_cache_dir(data_dir.clone())?;
            let translate_handle =
                crate::asr::AsrHandle::new(translate_config.clone(), translate_cache_dir);
            eprintln!(
                "translating {} → {} via {}…",
                src,
                tgt,
                translate_config.display_name()
            );
            let translated = rt
                .block_on(translate_handle.translate_text(
                    final_text.clone(),
                    src.clone(),
                    tgt.to_string(),
                    translate_max_tokens,
                ))
                .map_err(|e| format!("translate: {e:#}"))?;
            (
                translated,
                Some(serde_json::json!({
                    "from": src,
                    "to": tgt,
                    "backend": translate_backend,
                    "max_tokens": translate_max_tokens,
                    "original_text": final_text,
                })),
            )
        } else {
            (final_text.clone(), None)
        };

        // Streaming path: effective_fmt is guaranteed to be Txt or
        // Json (Srt/Vtt error'd above).  Match the same shape as the
        // buffered path's JSON envelope minus the segments (the
        // streaming wrapper concatenates partials without timing
        // breakdown).
        let payload = match effective_fmt {
            TranscriptFormat::Json => serde_json::json!({
                "text": final_emit,
                "backend": backend,
                "streaming": true,
                "language_hint": language,
                "translation": translation_meta,
                "source_path": path.display().to_string(),
                "source_sample_rate": decoded.source_sample_rate,
                "source_channels": decoded.source_channels,
                "duration_seconds": decoded.duration_seconds,
                "decode_tier": decoded.tier.as_str(),
            })
            .to_string(),
            TranscriptFormat::Txt => final_emit.clone(),
            // Unreachable: guarded above.
            TranscriptFormat::Srt | TranscriptFormat::Vtt => unreachable!(
                "--stream + srt/vtt was supposed to error before this point"
            ),
        };
        write_chat_output(&output, &payload)?;
        return Ok(());
    }

    eprintln!("transcribing via {} (policy={:?})…", primary_config.display_name(), policy);
    let result = rt
        .block_on(crate::asr::transcribe_with_lid_routing(
            decoded.pcm,
            &primary_handle,
            backend_policy,
            lid_options,
            language_for_orchestrator,
        ))
        .map_err(|e| format!("ASR: {e:#}"))?;

    // ── Step 5: optional translate post-processing (P13.5 Phase 5) ──
    //
    // When --translate-to is set, the transcribed text gets a follow-
    // up MT pass via --translate-backend (default m2m100, any-to-any
    // 100 langs).  Needs to know the source language: either the
    // user passed --language ISO, or LID detected it (Phase 6) and
    // stashed it in result.language.  AsConfigured + no --language
    // hint means no source lang is known → hard error here so the
    // user knows to pick one rather than getting silent wrong output.
    let (final_text, translation_meta) = if let Some(tgt) = translate_to.as_deref() {
        let src = result
            .language
            .as_ref()
            .map(|l| l.as_str().to_owned())
            .ok_or_else(|| {
                "--translate-to needs a known source language: pass either \
                 --language <ISO> or --language auto with --policy != as-configured \
                 + --lid-model so LID can detect it"
                    .to_string()
            })?;

        let translate_config = match &translate_model {
            Some(p) => crate::asr::AsrConfig::with_model_path(&translate_backend, p.to_string_lossy()),
            None => crate::asr::AsrConfig::new(&translate_backend),
        };
        let translate_cache_dir = asr_cache_dir(data_dir.clone())?;
        let translate_handle =
            crate::asr::AsrHandle::new(translate_config.clone(), translate_cache_dir);
        eprintln!(
            "translating {} → {} via {}…",
            src,
            tgt,
            translate_config.display_name()
        );
        let translated = rt
            .block_on(translate_handle.translate_text(
                result.text.clone(),
                src.clone(),
                tgt.to_string(),
                translate_max_tokens,
            ))
            .map_err(|e| format!("translate: {e:#}"))?;
        (
            translated,
            Some(serde_json::json!({
                "from": src,
                "to": tgt,
                "backend": translate_backend,
                "max_tokens": translate_max_tokens,
                "original_text": result.text,
            })),
        )
    } else {
        (result.text.clone(), None)
    };

    // ── Step 6: write out ────────────────────────────────────────────
    let decision_str = format!("{:?}", result.decision);
    let payload = match effective_fmt {
        TranscriptFormat::Json => serde_json::json!({
            "text": final_text,
            "backend": backend,
            "used_backend": result.used_config.backend,
            "language_hint": language,
            "detected_language": result.language.as_ref().map(|l| l.as_str()),
            "confidence": result.confidence,
            "decision": decision_str,
            "translation": translation_meta,
            "segments": result.segments.iter().map(|s| serde_json::json!({
                "text": s.text,
                "start": s.start_seconds,
                "end": s.end_seconds,
            })).collect::<Vec<_>>(),
            "source_path": path.display().to_string(),
            "source_sample_rate": decoded.source_sample_rate,
            "source_channels": decoded.source_channels,
            "duration_seconds": decoded.duration_seconds,
            "decode_tier": decoded.tier.as_str(),
        })
        .to_string(),
        TranscriptFormat::Txt => final_text.clone(),
        TranscriptFormat::Srt => {
            if result.segments.is_empty() {
                return Err(
                    "SRT requested but the backend returned no segments — \
                     check that the model supports timestamped output \
                     (Whisper does; some others don't)."
                        .to_string(),
                );
            }
            format_segments_srt(&result.segments)
        }
        TranscriptFormat::Vtt => {
            if result.segments.is_empty() {
                return Err(
                    "VTT requested but the backend returned no segments — \
                     check that the model supports timestamped output \
                     (Whisper does; some others don't)."
                        .to_string(),
                );
            }
            format_segments_vtt(&result.segments)
        }
    };
    write_chat_output(&output, &payload)?;
    Ok(())
}

// ── P13.7 Step 6 — search-CLI filter parsers ──────────────────────

/// Parse a human-readable byte size into raw bytes.  Cloud-backup
/// parity: accepts `"100"` (bytes), `"100KB"`, `"100MB"`, `"1.5GB"`,
/// `"2TB"`.  Case-insensitive, optional decimal, optional `B` /
/// `KB`/`MB`/`GB`/`TB` suffix.
fn parse_human_size(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size string".into());
    }
    // Find the boundary between the numeric prefix and the unit.
    let split = s
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(s.len());
    let (num_str, unit_str) = s.split_at(split);
    let num: f64 = num_str
        .trim()
        .parse()
        .map_err(|e| format!("not a number: {e}"))?;
    if num < 0.0 {
        return Err("size must be non-negative".into());
    }
    let mul: f64 = match unit_str.trim().to_uppercase().as_str() {
        "" | "B" => 1.0,
        "K" | "KB" => 1024.0,
        "M" | "MB" => 1024.0 * 1024.0,
        "G" | "GB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" => 1024.0_f64.powi(4),
        other => return Err(format!("unknown size unit `{other}`")),
    };
    let bytes = (num * mul).round();
    if !bytes.is_finite() || bytes > i64::MAX as f64 {
        return Err("size overflows i64".into());
    }
    Ok(bytes as i64)
}

/// Parse `YYYY-MM-DD` (or `YYYY-MM-DD HH:MM:SS` / `YYYY-MM-DDTHH:MM:SS`)
/// into Unix seconds (UTC).  Hand-written so we don't pull `chrono`
/// or `time` into src-tauri's direct deps — both are present
/// transitively but adding either as a direct dep grows the
/// surface unnecessarily for one parser.
///
/// Uses Howard Hinnant's days-from-civil algorithm for the
/// date → Unix-seconds conversion: handles years 1970..= without
/// special-casing leap years.
fn parse_iso_date_to_unix(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty date string".into());
    }

    // Three shapes accepted: `YYYY-MM-DD`, `YYYY-MM-DD HH:MM:SS`,
    // and `YYYY-MM-DDTHH:MM:SS` (with optional trailing `Z`).
    let (date_part, time_part) = if s.len() == 10 {
        (s, "00:00:00")
    } else if let Some(t_idx) = s.find(['T', ' ']) {
        let (d, t) = s.split_at(t_idx);
        let t = t.trim_start_matches(['T', ' ']).trim_end_matches('Z');
        (d, t)
    } else {
        return Err("expected YYYY-MM-DD[ THH:MM:SS]".into());
    };

    // Date components.
    let date_components: Vec<&str> = date_part.split('-').collect();
    if date_components.len() != 3 {
        return Err(format!("malformed date `{date_part}` (need YYYY-MM-DD)"));
    }
    let y: i32 = date_components[0]
        .parse()
        .map_err(|e| format!("year parse: {e}"))?;
    let m: u32 = date_components[1]
        .parse()
        .map_err(|e| format!("month parse: {e}"))?;
    let d: u32 = date_components[2]
        .parse()
        .map_err(|e| format!("day parse: {e}"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(format!("date out of range: {y}-{m}-{d}"));
    }

    // Time components.
    let (hh, mm, ss) = if time_part.is_empty() {
        (0u32, 0u32, 0u32)
    } else {
        let tc: Vec<&str> = time_part.split(':').collect();
        if tc.len() != 3 {
            return Err(format!("malformed time `{time_part}` (need HH:MM:SS)"));
        }
        let hh: u32 = tc[0].parse().map_err(|e| format!("hour parse: {e}"))?;
        let mm: u32 = tc[1].parse().map_err(|e| format!("minute parse: {e}"))?;
        let ss: u32 = tc[2].parse().map_err(|e| format!("second parse: {e}"))?;
        if hh > 23 || mm > 59 || ss > 60 {
            return Err(format!("time out of range: {time_part}"));
        }
        (hh, mm, ss)
    };

    // Howard Hinnant — days_from_civil.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = (y_adj - era * 400) as u32; // [0, 399]
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era as i64 * 146097 + doe as i64 - 719468; // since 1970-01-01

    Ok(days * 86400 + hh as i64 * 3600 + mm as i64 * 60 + ss as i64)
}

/// Compact human-readable byte size — `"1.4 MB"`, `"950 KB"`, etc.
/// Mirrors cloud-backup's `format_bytes` output.
fn format_size_human(bytes: i64) -> String {
    if bytes < 0 {
        return String::new();
    }
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut idx = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else {
        format!("{:.1} {}", size, UNITS[idx])
    }
}

// ── Subtitle formatters (P13.5 follow-up — SRT / VTT) ─────────────

/// Render seconds as `HH:MM:SS,mmm` (SRT idiom — comma as decimal
/// separator).  Caps the hour field at `99` because SRT players
/// expect a 2-digit hour and an 8-hour audio file is well past
/// anyone's tolerance for waiting on the model anyway.
fn format_srt_time(seconds: f64) -> String {
    let total_ms = (seconds * 1000.0).round().max(0.0) as i64;
    let h = (total_ms / 3_600_000).min(99);
    let m = (total_ms / 60_000) % 60;
    let s = (total_ms / 1000) % 60;
    let ms = total_ms % 1000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// Render seconds as `HH:MM:SS.mmm` (WebVTT idiom — period as
/// decimal separator).  Same time-range cap as SRT.
fn format_vtt_time(seconds: f64) -> String {
    let total_ms = (seconds * 1000.0).round().max(0.0) as i64;
    let h = (total_ms / 3_600_000).min(99);
    let m = (total_ms / 60_000) % 60;
    let s = (total_ms / 1000) % 60;
    let ms = total_ms % 1000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Render a [`crate::asr::AsrSegment`] slice as a SubRip (`.srt`)
/// subtitle file.  Numbering starts at 1 (SRT convention); empty
/// segments are skipped so the cue numbering stays contiguous.
fn format_segments_srt(segments: &[crate::asr::AsrSegment]) -> String {
    let mut out = String::new();
    let mut cue_idx = 1;
    for seg in segments {
        if seg.text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("{cue_idx}\n"));
        out.push_str(&format!(
            "{} --> {}\n",
            format_srt_time(seg.start_seconds),
            format_srt_time(seg.end_seconds)
        ));
        out.push_str(seg.text.trim());
        out.push_str("\n\n");
        cue_idx += 1;
    }
    out
}

/// Render a [`crate::asr::AsrSegment`] slice as a WebVTT (`.vtt`)
/// subtitle file.  Starts with the required `WEBVTT` header.
/// Cue identifiers are omitted (WebVTT allows it); empty segments
/// are skipped.
fn format_segments_vtt(segments: &[crate::asr::AsrSegment]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for seg in segments {
        if seg.text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{} --> {}\n",
            format_vtt_time(seg.start_seconds),
            format_vtt_time(seg.end_seconds)
        ));
        out.push_str(seg.text.trim());
        out.push_str("\n\n");
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn cmd_chat_tts(
    out: OutFormat,
    text: String,
    backend: String,
    model: Option<PathBuf>,
    voice: Option<PathBuf>,
    voice_ref_text: Option<String>,
    speaker: Option<String>,
    output: PathBuf,
    data_dir: Option<PathBuf>,
) -> Result<(), String> {
    if text.is_empty() {
        return Err("TTS input text is empty".to_string());
    }

    // ── Step 1: configure the session ────────────────────────────────
    let config = match &model {
        Some(p) => crate::asr::AsrConfig::with_model_path(&backend, p.to_string_lossy()),
        None => crate::asr::AsrConfig::new(&backend),
    };
    let cache_dir = asr_cache_dir(data_dir)?;
    let handle = crate::asr::AsrHandle::new(config.clone(), cache_dir);

    // ── Step 2: synthesise (async + atomic voice/speaker apply) ──────
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let voice_opt = voice.clone().map(|p| (p, voice_ref_text.clone()));
    eprintln!("synthesising via {}…", config.display_name());
    let pcm = rt
        .block_on(handle.synthesize_with_options(text.clone(), voice_opt, speaker.clone()))
        .map_err(|e| format!("TTS: {e:#}"))?;

    // ── Step 3: write WAV ────────────────────────────────────────────
    // CrispASR TTS emits 24 kHz mono Float32 across every supported
    // backend (per Session::synthesize docstring).  Preserve that —
    // resampling to 16 kHz here would degrade the audio for no gain.
    const TTS_SAMPLE_RATE: u32 = 24_000;
    crate::audio::writer::write_wav_mono(&output, &pcm, TTS_SAMPLE_RATE)
        .map_err(|e| format!("WAV write: {e:#}"))?;
    eprintln!(
        "wrote {} samples ({:.2} s @ {} Hz) → {}",
        pcm.len(),
        pcm.len() as f64 / TTS_SAMPLE_RATE as f64,
        TTS_SAMPLE_RATE,
        output.display()
    );

    // ── Step 4: report metadata on stdout ────────────────────────────
    match out {
        OutFormat::Json => println!(
            "{}",
            serde_json::json!({
                "backend": backend,
                "text": text,
                "output": output.display().to_string(),
                "samples": pcm.len(),
                "sample_rate": TTS_SAMPLE_RATE,
                "duration_seconds": pcm.len() as f64 / TTS_SAMPLE_RATE as f64,
            })
        ),
        OutFormat::Text => println!(
            "synthesised {} samples ({:.2}s) → {}",
            pcm.len(),
            pcm.len() as f64 / TTS_SAMPLE_RATE as f64,
            output.display()
        ),
    }
    Ok(())
}

/// Write `payload` to `output`, with `-` routing to stdout.  Used by
/// `chat transcribe` only (TTS writes binary WAV via the audio writer,
/// not text via this helper).  Adds a single trailing newline to
/// match the convention of every other CLI subcommand (so output is
/// safe to pipe into `read`, `xargs -I`, etc.).
fn write_chat_output(output: &str, payload: &str) -> Result<(), String> {
    if output == "-" {
        println!("{payload}");
        Ok(())
    } else {
        let path = PathBuf::from(output);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("creating parent {}: {e}", parent.display()))?;
            }
        }
        let mut content = payload.to_string();
        if !content.ends_with('\n') {
            content.push('\n');
        }
        std::fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        eprintln!("wrote → {}", path.display());
        Ok(())
    }
}

// ── manpage ────────────────────────────────────────────────────────────────

fn cmd_manpage(out: PathBuf) -> Result<(), String> {
    use clap::CommandFactory;
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buf = Vec::new();
    man.render(&mut buf).map_err(|e| e.to_string())?;
    let dest = out.join("crispsorter.1");
    std::fs::write(&dest, &buf).map_err(|e| e.to_string())?;
    eprintln!("wrote {}", dest.display());
    Ok(())
}

// ── batch ──────────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
enum BatchCmd {
    /// Add files to the durable ingest queue (visible in the GUI on next launch).
    Add {
        /// Files or folders to enqueue for ingest.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Target ingest level. 1=filesystem metadata only, 3=full extraction (default).
        #[arg(long, default_value_t = 3)]
        level: u8,
        /// Append to an existing job instead of creating a new one.
        #[arg(long)]
        job_id: Option<String>,
    },
    /// List ingest jobs and their file counts.
    List {
        /// Show all files for a specific job.
        #[arg(long)]
        job_id: Option<String>,
        /// Filter files by status (pending / done / error / skipped).
        #[arg(long)]
        status: Option<String>,
    },
    /// Extract text + call an LLM to infer title/author/year, then emit a sort plan.
    ///
    /// Requires an OpenAI-compatible chat endpoint (Ollama, llamacpp, OpenAI).
    /// Output: JSON sort plan `{ mode, items: [{src, dst}] }` written to --out-plan
    /// (or stdout when --out-plan is omitted). Pipe to `batch apply` to execute.
    Process {
        /// Job ID to process. Omit to process the most-recent pending job.
        #[arg(long)]
        job_id: Option<String>,
        /// Max files to process in one run. Default: all pending.
        #[arg(long)]
        limit: Option<usize>,
        /// OpenAI-compatible chat base URL. Default: http://localhost:11434/v1
        #[arg(long, default_value = "http://localhost:11434/v1")]
        llm_url: String,
        /// Model name to pass to the chat endpoint. Default: llama3
        #[arg(long, default_value = "llama3")]
        llm_model: String,
        /// API key for the endpoint. Leave empty for Ollama.
        #[arg(long, default_value = "")]
        api_key: String,
        /// Sort destination root. Default: sibling folder named "Sorted".
        #[arg(long)]
        export_path: Option<PathBuf>,
        /// Path template. Default: {Author}/{Year}/{Title}
        #[arg(long, default_value = "{Author}/{Year}/{Title}")]
        path_template: String,
        /// Write the sort plan to this file. Default: stdout.
        #[arg(long)]
        out_plan: Option<PathBuf>,
        /// Dry-run: compute plan without marking files as done.
        #[arg(long)]
        dry_run: bool,
    },
    /// Execute a JSON sort plan — move or copy files as described in the plan.
    ///
    /// Plan format:
    ///   { "mode": "move"|"copy", "items": [{ "src": "...", "dst": "..." }, ...] }
    Apply {
        /// Path to the plan JSON file. Use `-` to read from stdin.
        plan: String,
        /// Override mode from plan (move / copy).
        #[arg(long)]
        mode: Option<String>,
        /// Dry-run: print what would happen without actually moving/copying.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(serde::Deserialize)]
struct SortPlan {
    #[serde(default = "default_move")]
    mode: String,
    items: Vec<SortPlanItem>,
}
#[derive(serde::Deserialize)]
struct SortPlanItem {
    src: String,
    dst: String,
}
fn default_move() -> String { "move".to_owned() }

fn cmd_batch(out: OutFormat, data_dir: Option<PathBuf>, cmd: BatchCmd) -> Result<(), String> {
    let data_dir = resolve_data_dir(data_dir)?;
    match cmd {
        BatchCmd::Add { paths, level, job_id } => {
            let queue = crate::jobs::JobQueue::open_or_create(&data_dir)
                .map_err(|e| e.to_string())?;

            // Expand folders to individual files.
            let mut files: Vec<crate::jobs::FileEntry> = Vec::new();
            for p in &paths {
                if p.is_dir() {
                    for entry in jwalk::WalkDir::new(p)
                        .into_iter()
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().is_file())
                    {
                        files.push(crate::jobs::FileEntry {
                            file_path: entry.path().to_string_lossy().into_owned(),
                            doc_id: None,
                            target_level: level,
                        });
                    }
                } else if p.exists() {
                    files.push(crate::jobs::FileEntry {
                        file_path: p.to_string_lossy().into_owned(),
                        doc_id: None,
                        target_level: level,
                    });
                } else {
                    eprintln!("warning: path does not exist: {}", p.display());
                }
            }

            let jid = if let Some(id) = job_id {
                id
            } else {
                let source_paths: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                queue
                    .create_job("cli_add", &source_paths, level, None)
                    .map_err(|e| e.to_string())?
            };

            let added = queue
                .add_files(&jid, &files)
                .map_err(|e| e.to_string())?;

            match out {
                OutFormat::Json => {
                    println!("{}", serde_json::json!({
                        "job_id": jid,
                        "files_added": added,
                        "total_queued": files.len(),
                    }));
                }
                OutFormat::Text => {
                    println!("job {jid}: queued {added} file(s)");
                    println!("Open the GUI to process them (Hinzufügen → Resume).");
                }
            }
        }

        BatchCmd::List { job_id, status } => {
            let queue = crate::jobs::JobQueue::open_or_create(&data_dir)
                .map_err(|e| e.to_string())?;
            if let Some(jid) = job_id {
                let files = queue
                    .list_files(&jid, status.as_deref(), 10_000, 0)
                    .map_err(|e| e.to_string())?;
                for f in &files {
                    match out {
                        OutFormat::Json => println!("{}", serde_json::to_string(f).unwrap_or_default()),
                        OutFormat::Text => {
                            println!("[{}] {}", f.status, f.file_path);
                            if let Some(ref e) = f.error_text {
                                println!("    error: {e}");
                            }
                        }
                    }
                }
                eprintln!("{} file(s)", files.len());
            } else {
                let jobs = queue.list_jobs().map_err(|e| e.to_string())?;
                for j in &jobs {
                    match out {
                        OutFormat::Json => println!("{}", serde_json::to_string(j).unwrap_or_default()),
                        OutFormat::Text => {
                            println!(
                                "[{}] {} — {}/{} done  {} err  type={}",
                                j.status, j.id,
                                j.done_files, j.total_files,
                                j.error_files, j.job_type
                            );
                        }
                    }
                }
                eprintln!("{} job(s)", jobs.len());
            }
        }

        BatchCmd::Process { job_id, limit, llm_url, llm_model, api_key, export_path, path_template, out_plan, dry_run } => {
            let queue = crate::jobs::JobQueue::open_or_create(&data_dir)
                .map_err(|e| e.to_string())?;

            // Find job.
            let effective_job_id = if let Some(jid) = job_id {
                jid
            } else {
                let jobs = queue.list_jobs().map_err(|e| e.to_string())?;
                jobs.into_iter()
                    .filter(|j| j.status == "pending" || j.status == "running")
                    .max_by_key(|j| j.created_at)
                    .map(|j| j.id)
                    .ok_or_else(|| "No pending job found — run `batch add` first".to_string())?
            };

            let files = queue
                .list_files(&effective_job_id, Some("pending"), limit.unwrap_or(10_000) as i64, 0)
                .map_err(|e| e.to_string())?;

            if files.is_empty() {
                eprintln!("no pending files in job {effective_job_id}");
                return Ok(());
            }
            eprintln!("processing {} file(s) from job {effective_job_id}…", files.len());

            // Build tokio runtime for async HTTP + extraction.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().map_err(|e| e.to_string())?;

            let mut plan_items: Vec<serde_json::Value> = Vec::new();
            let sanitize = |s: &str| s.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_")
                .chars().take(100).collect::<String>();

            for file in &files {
                let p = std::path::PathBuf::from(&file.file_path);
                if !p.exists() {
                    eprintln!("skip (not found): {}", p.display());
                    continue;
                }

                // Extract text.
                let extracted = rt.block_on(tokio::task::spawn_blocking({
                    let pp = p.clone();
                    move || crate::extractors::extract_text_from_path(&pp)
                }))
                    .map_err(|e| e.to_string())?.ok();

                let text_sample = extracted.as_ref()
                    .map(|e| e.full_text.chars().take(4000).collect::<String>())
                    .unwrap_or_default();

                // Call LLM.
                let filename = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let prompt = format!(
                    "Extract bibliographic metadata from the text and filename.\n\
                     Output ONLY in this XML format:\n\
                     <METADATA>\n  <TITLE>...</TITLE>\n  <AUTHOR>Lastname, Firstname</AUTHOR>\n  <YEAR>YYYY</YEAR>\n</METADATA>\n\
                     Use \"Unknown\" when unavailable.\n\nFilename: \"{filename}\"\n\nText:\n{text_sample}"
                );

                let (title, author, year) = rt.block_on(async {
                    let client = reqwest::Client::new();
                    let body = serde_json::json!({
                        "model": llm_model,
                        "messages": [{"role": "user", "content": prompt}],
                        "stream": false
                    });
                    let mut req = client.post(format!("{}/chat/completions", llm_url.trim_end_matches('/')))
                        .json(&body);
                    if !api_key.is_empty() {
                        req = req.bearer_auth(&api_key);
                    }
                    let resp = req.send().await.ok()?.json::<serde_json::Value>().await.ok()?;
                    let text = resp["choices"][0]["message"]["content"].as_str()?.to_owned();
                    let title  = regex_capture(&text, r"<TITLE>(.*?)</TITLE>");
                    let author = regex_capture(&text, r"<AUTHOR>(.*?)</AUTHOR>");
                    let year   = regex_capture(&text, r"<YEAR>(\d{4})</YEAR>");
                    Some((title, author, year))
                }).unwrap_or_default();

                let title_s  = title.as_deref().unwrap_or("Unknown Title");
                let author_s = author.as_deref().unwrap_or("Unknown Author");
                let year_s   = year.as_deref().unwrap_or("0000");
                let ext      = p.extension().and_then(|e| e.to_str()).unwrap_or("");

                // Compute destination path.
                let relative = path_template
                    .replace("{Title}",    &sanitize(title_s))
                    .replace("{Author}",   &sanitize(author_s))
                    .replace("{Year}",     &sanitize(year_s))
                    .replace("{Filename}", &sanitize(&filename))
                    .replace("{Ext}",      ext);
                let base = export_path.clone().unwrap_or_else(|| {
                    p.parent().map(|d| d.join("Sorted")).unwrap_or_else(|| std::path::PathBuf::from("Sorted"))
                });
                let has_ext_token = path_template.contains("{Ext}");
                let dst = if has_ext_token {
                    base.join(&relative)
                } else {
                    base.join(format!("{relative}.{ext}"))
                };

                plan_items.push(serde_json::json!({
                    "src": file.file_path,
                    "dst": dst.display().to_string(),
                    "title": title_s,
                    "author": author_s,
                    "year": year_s,
                }));

                if !dry_run {
                    queue.mark_done(&effective_job_id, &[file.row_id])
                        .map_err(|e| e.to_string())?;
                }

                match out {
                    OutFormat::Text => eprintln!("  ✓ {} → {}", filename, dst.display()),
                    OutFormat::Json => {}
                }
            }

            let plan_json = serde_json::json!({ "mode": "move", "items": plan_items });
            let plan_str = serde_json::to_string_pretty(&plan_json).unwrap_or_default();

            if let Some(ref plan_path) = out_plan {
                std::fs::write(plan_path, &plan_str).map_err(|e| e.to_string())?;
                eprintln!("plan written to {}", plan_path.display());
            } else {
                println!("{plan_str}");
            }
            eprintln!("{} items in plan{}", plan_items.len(), if dry_run { " (dry-run)" } else { "" });
        }

        BatchCmd::Apply { plan, mode, dry_run } => {
            let json = if plan == "-" {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
                s
            } else {
                std::fs::read_to_string(&plan)
                    .map_err(|e| format!("reading {plan}: {e}"))?
            };
            let mut p: SortPlan = serde_json::from_str(&json)
                .map_err(|e| format!("parsing plan: {e}"))?;
            if let Some(m) = mode { p.mode = m; }

            let is_move = p.mode == "move";
            let mut done = 0usize;
            let mut errs = 0usize;
            for item in &p.items {
                let src = std::path::Path::new(&item.src);
                let dst = std::path::Path::new(&item.dst);
                if !src.exists() {
                    eprintln!("skip (not found): {}", item.src);
                    errs += 1;
                    continue;
                }
                if dry_run {
                    println!("{} {} -> {}", if is_move { "mv" } else { "cp" }, item.src, item.dst);
                    done += 1;
                    continue;
                }
                if let Some(parent) = dst.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("mkdir {}: {e}", parent.display());
                        errs += 1;
                        continue;
                    }
                }
                let result = if is_move {
                    std::fs::rename(src, dst).or_else(|_| {
                        std::fs::copy(src, dst).and_then(|_| std::fs::remove_file(src))
                    })
                } else {
                    std::fs::copy(src, dst).map(|_| ())
                };
                match result {
                    Ok(()) => {
                        match out {
                            OutFormat::Json => println!("{}", serde_json::json!({"ok": true, "src": item.src, "dst": item.dst})),
                            OutFormat::Text => println!("ok  {}", item.dst),
                        }
                        done += 1;
                    }
                    Err(e) => {
                        match out {
                            OutFormat::Json => println!("{}", serde_json::json!({"ok": false, "src": item.src, "error": e.to_string()})),
                            OutFormat::Text => eprintln!("err {} -> {}: {e}", item.src, item.dst),
                        }
                        errs += 1;
                    }
                }
            }
            eprintln!("{done} ok, {errs} errors{}",
                if dry_run { " (dry-run)" } else { "" });
        }
    }
    Ok(())
}

/// Cheap XML-tag capture without a regex crate — finds the first match of
/// `<TAG>content</TAG>` and returns `content`. Case-sensitive.
fn regex_capture(text: &str, pattern: &str) -> Option<String> {
    // Pattern is like r"<TITLE>(.*?)</TITLE>" — extract tag name and reconstruct.
    let open_end = pattern.find('>')?;
    let open_tag = &pattern[..=open_end]; // "<TITLE>"
    let close_tag_start = pattern.rfind('<')?;
    let close_tag = &pattern[close_tag_start..]; // "</TITLE>"
    let tag_name = &open_tag[1..open_tag.len() - 1]; // "TITLE"
    let _ = close_tag; let _ = tag_name;

    let start = text.find(open_tag)? + open_tag.len();
    let end_tag = format!("</{}>", &open_tag[1..open_tag.len()-1]);
    let end = text[start..].find(&end_tag)?;
    let content = text[start..start + end].trim().to_owned();
    if content.is_empty() { None } else { Some(content) }
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

    // ── P13.7 Step 6 — search CLI filter helpers ─────────────────────

    #[test]
    fn parse_human_size_handles_known_units() {
        assert_eq!(parse_human_size("100").unwrap(), 100);
        assert_eq!(parse_human_size("100B").unwrap(), 100);
        assert_eq!(parse_human_size("100KB").unwrap(), 100 * 1024);
        assert_eq!(parse_human_size("100MB").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_human_size("1.5GB").unwrap(),
                   (1.5_f64 * 1024.0 * 1024.0 * 1024.0).round() as i64);
        assert_eq!(parse_human_size("2TB").unwrap(),
                   2_i64 * 1024_i64.pow(4));
    }

    #[test]
    fn parse_human_size_case_insensitive_and_whitespace_tolerant() {
        assert_eq!(parse_human_size(" 100 mb ").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_human_size("100Mb").unwrap(), 100 * 1024 * 1024);
        // Single-letter shorthand.
        assert_eq!(parse_human_size("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_human_size_rejects_invalid() {
        assert!(parse_human_size("").is_err());
        assert!(parse_human_size("abc").is_err());
        assert!(parse_human_size("-100MB").is_err());
        assert!(parse_human_size("100XB").is_err());
    }

    #[test]
    fn parse_iso_date_to_unix_known_dates() {
        // 1970-01-01 → 0.
        assert_eq!(parse_iso_date_to_unix("1970-01-01").unwrap(), 0);
        // 2020-01-01 00:00:00 UTC → 1_577_836_800 (well-known epoch).
        assert_eq!(parse_iso_date_to_unix("2020-01-01").unwrap(), 1_577_836_800);
        // Same date with explicit time.
        assert_eq!(
            parse_iso_date_to_unix("2020-01-01T00:00:00Z").unwrap(),
            1_577_836_800
        );
        assert_eq!(
            parse_iso_date_to_unix("2020-01-01 00:00:00").unwrap(),
            1_577_836_800
        );
    }

    #[test]
    fn parse_iso_date_to_unix_leap_year() {
        // 2024-02-29 must work (leap year).  Days_from_civil handles
        // leap years implicitly — this pin catches a future
        // refactor that gets the algorithm wrong.
        let d = parse_iso_date_to_unix("2024-02-29").unwrap();
        let d_next = parse_iso_date_to_unix("2024-03-01").unwrap();
        assert_eq!(d_next - d, 86400, "Feb 29 → Mar 1 must be exactly 1 day");
    }

    #[test]
    fn parse_iso_date_to_unix_rejects_garbage() {
        assert!(parse_iso_date_to_unix("").is_err());
        assert!(parse_iso_date_to_unix("not-a-date").is_err());
        assert!(parse_iso_date_to_unix("2024-13-01").is_err());
        assert!(parse_iso_date_to_unix("2024-02-30T25:00:00").is_err());
    }

    #[test]
    fn format_size_human_known_thresholds() {
        assert_eq!(format_size_human(0), "0 B");
        assert_eq!(format_size_human(512), "512 B");
        assert_eq!(format_size_human(1024), "1.0 KB");
        assert_eq!(format_size_human(1536), "1.5 KB");
        assert_eq!(format_size_human(1024 * 1024), "1.0 MB");
        assert_eq!(format_size_human(1024_i64 * 1024 * 1024), "1.0 GB");
    }

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
        for s in ["version", "doctor", "catalog", "index", "images"] {
            assert!(SUBCOMMANDS.contains(&s), "SUBCOMMANDS missing {s}");
        }
    }

    /// P13 — `images extensions` is the cheapest reachable subcommand
    /// (no data dir touched), so it's the canary for "the Images
    /// surface parses at all".  Failure here means a clap derive macro
    /// rejected a field rename or a missing trait derive.
    #[test]
    fn images_extensions_parses() {
        let cli = Cli::try_parse_from(["crispsorter", "images", "extensions"]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Extensions, .. } => (),
            other => panic!("expected Images Extensions, got {other:?}"),
        }
    }

    #[test]
    fn images_list_accepts_limit_and_ext() {
        // value_delimiter = ',' should split jpg,png into two strings.
        let cli = Cli::try_parse_from([
            "crispsorter", "images", "list", "--limit", "5", "--ext", "jpg,png",
        ]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::List { limit, ext, folder }, .. } => {
                assert_eq!(limit, 5);
                assert_eq!(ext, vec!["jpg".to_owned(), "png".to_owned()]);
                assert!(folder.is_none());
            }
            other => panic!("expected Images List, got {other:?}"),
        }
    }

    #[test]
    fn images_count_accepts_folder_and_ext_overrides() {
        let cli = Cli::try_parse_from([
            "crispsorter", "images", "count",
            "--folder", "/tmp/photos",
            "--ext", "heic",
        ]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Count { ext, folder }, .. } => {
                assert_eq!(ext, vec!["heic".to_owned()]);
                assert_eq!(folder.as_deref(), Some(std::path::Path::new("/tmp/photos")));
            }
            other => panic!("expected Images Count, got {other:?}"),
        }
    }

    #[test]
    fn images_thumbnail_accepts_size_and_out() {
        let cli = Cli::try_parse_from([
            "crispsorter", "images", "thumbnail", "/tmp/x.jpg",
            "--size", "512", "--out", "/tmp/x.png",
        ]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Thumbnail { path, size, out }, .. } => {
                assert_eq!(path, std::path::PathBuf::from("/tmp/x.jpg"));
                assert_eq!(size, 512);
                assert_eq!(out.as_deref(), Some(std::path::Path::new("/tmp/x.png")));
            }
            other => panic!("expected Images Thumbnail, got {other:?}"),
        }
    }

    #[test]
    fn images_thumbnail_defaults_size_to_256_and_out_to_stdout() {
        let cli = Cli::try_parse_from(["crispsorter", "images", "thumbnail", "/tmp/x.jpg"]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Thumbnail { size, out, .. }, .. } => {
                assert_eq!(size, 256);
                assert!(out.is_none(), "default --out should be None (= stdout)");
            }
            other => panic!("expected Images Thumbnail, got {other:?}"),
        }
    }

    #[test]
    fn images_exif_parses_with_path_only() {
        let cli = Cli::try_parse_from(["crispsorter", "images", "exif", "/tmp/x.jpg"]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Exif { path }, .. } => {
                assert_eq!(path, std::path::PathBuf::from("/tmp/x.jpg"));
            }
            other => panic!("expected Images Exif, got {other:?}"),
        }
    }

    #[test]
    fn images_duplicates_accepts_ext_and_folder_overrides() {
        let cli = Cli::try_parse_from([
            "crispsorter", "images", "duplicates",
            "--ext", "jpg,heic",
            "--folder", "/Users/me/Photos",
        ]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Duplicates { ext, folder }, .. } => {
                assert_eq!(ext, vec!["jpg".to_owned(), "heic".to_owned()]);
                assert_eq!(folder.as_deref(), Some(std::path::Path::new("/Users/me/Photos")));
            }
            other => panic!("expected Images Duplicates, got {other:?}"),
        }
    }

    #[test]
    fn images_duplicates_parses_with_defaults() {
        let cli = Cli::try_parse_from(["crispsorter", "images", "duplicates"]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::Duplicates { ext, folder }, .. } => {
                assert!(ext.is_empty());
                assert!(folder.is_none());
            }
            other => panic!("expected Images Duplicates, got {other:?}"),
        }
    }

    #[test]
    fn images_near_duplicates_uses_default_threshold() {
        let cli = Cli::try_parse_from(["crispsorter", "images", "near-duplicates"]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::NearDuplicates { threshold, ext, folder }, .. } => {
                // Spec calls for default = 8.
                assert_eq!(threshold, 8);
                assert!(ext.is_empty());
                assert!(folder.is_none());
            }
            other => panic!("expected Images NearDuplicates, got {other:?}"),
        }
    }

    #[test]
    fn images_near_duplicates_accepts_threshold_override() {
        let cli = Cli::try_parse_from([
            "crispsorter", "images", "near-duplicates", "--threshold", "12",
        ]).unwrap();
        match cli.command {
            Command::Images { cmd: ImagesCmd::NearDuplicates { threshold, .. }, .. } => {
                assert_eq!(threshold, 12);
            }
            other => panic!("expected Images NearDuplicates, got {other:?}"),
        }
    }

    #[test]
    fn images_data_dir_is_global_under_subcommand() {
        // --data-dir is `global = true` on the Images variant, so it
        // can be supplied either before or after the subcommand name.
        for argv in [
            vec!["crispsorter", "images", "--data-dir", "/tmp/x", "extensions"],
            vec!["crispsorter", "images", "extensions", "--data-dir", "/tmp/x"],
        ] {
            let cli = Cli::try_parse_from(argv.clone())
                .unwrap_or_else(|e| panic!("failed parsing {argv:?}: {e}"));
            match cli.command {
                Command::Images { data_dir, cmd: ImagesCmd::Extensions } => {
                    assert_eq!(data_dir.as_deref(), Some(std::path::Path::new("/tmp/x")));
                }
                other => panic!("expected Images Extensions, got {other:?}"),
            }
        }
    }

    // ── P13.5 slice A — chat transcribe / tts clap parsing + helpers ──

    #[test]
    fn chat_transcribe_minimal_args_parse() {
        // The cheapest positive case: just a path. Backend defaults to
        // whisper, output to stdout, everything else None.  Phase 6
        // adds --policy/--fallback/--lid-model/--lid-method to this
        // variant — they all have defaults so the bare command still
        // parses; this test pins those defaults.
        let cli = Cli::try_parse_from([
            "crispsorter", "chat", "transcribe", "/tmp/foo.wav",
        ])
        .expect("transcribe with just a path should parse");
        match cli.command {
            Command::Chat { cmd: ChatCmd::Transcribe {
                path, backend, model, language, output, pure_rust, stream,
                policy, fallback_backend, lid_model, lid_method,
                translate_to, translate_backend, translate_model, translate_max_tokens, ..
            } } => {
                assert_eq!(path, PathBuf::from("/tmp/foo.wav"));
                assert_eq!(backend, "whisper", "default backend must be whisper");
                assert!(model.is_none());
                assert!(language.is_none());
                assert_eq!(output, "-", "default output is stdout");
                assert!(!pure_rust);
                assert!(!stream, "--stream is off by default");
                // Phase 6 defaults: AsConfigured (no LID), whisper fallback,
                // whisper LID method, no LID model.
                assert_eq!(policy, LidPolicy::AsConfigured);
                assert_eq!(fallback_backend, "whisper");
                assert!(lid_model.is_none());
                assert_eq!(lid_method, LidMethodChoice::Whisper);
                // Phase 5 defaults: no translate-to (translation skipped),
                // m2m100 fallback for when --translate-to IS set, no
                // explicit model, 0 = upstream default max_tokens.
                assert!(translate_to.is_none());
                assert_eq!(translate_backend, "m2m100");
                assert!(translate_model.is_none());
                assert_eq!(translate_max_tokens, 0);
            }
            other => panic!("expected Chat Transcribe, got {other:?}"),
        }
    }

    #[test]
    fn chat_transcribe_transcript_format_parses() {
        // --transcript-format flag accepts all four values + None
        // default.  Pinning the round-trip catches a clap rename
        // (e.g. `value_enum` → string list) silently breaking the
        // SRT/VTT path.
        for (cli_arg, expect) in [
            ("txt",  Some(TranscriptFormat::Txt)),
            ("json", Some(TranscriptFormat::Json)),
            ("srt",  Some(TranscriptFormat::Srt)),
            ("vtt",  Some(TranscriptFormat::Vtt)),
        ] {
            let cli = Cli::try_parse_from([
                "crispsorter", "chat", "transcribe", "/tmp/foo.wav",
                "--transcript-format", cli_arg,
            ])
            .unwrap_or_else(|e| panic!("--transcript-format {cli_arg} should parse: {e}"));
            match cli.command {
                Command::Chat { cmd: ChatCmd::Transcribe { transcript_format, .. } } => {
                    assert_eq!(transcript_format, expect);
                }
                other => panic!("expected Chat Transcribe, got {other:?}"),
            }
        }
    }

    // ── SRT / VTT formatter unit tests ───────────────────────────────

    fn sample_segments() -> Vec<crate::asr::AsrSegment> {
        vec![
            crate::asr::AsrSegment {
                text: "Hello world.".to_string(),
                start_seconds: 0.0,
                end_seconds: 1.5,
            },
            crate::asr::AsrSegment {
                text: "This is a test.".to_string(),
                start_seconds: 1.5,
                end_seconds: 3.25,
            },
            // Empty segment — must be skipped (cue numbering stays
            // contiguous on the SRT side).
            crate::asr::AsrSegment {
                text: "   ".to_string(),
                start_seconds: 3.25,
                end_seconds: 3.5,
            },
        ]
    }

    #[test]
    fn format_srt_time_round_trips_known_offsets() {
        // Pin a handful of well-known timestamps so a future
        // formatter regression (off-by-one ms, hour cap) shows up.
        assert_eq!(format_srt_time(0.0), "00:00:00,000");
        assert_eq!(format_srt_time(1.5), "00:00:01,500");
        assert_eq!(format_srt_time(59.999), "00:00:59,999");
        assert_eq!(format_srt_time(60.0), "00:01:00,000");
        assert_eq!(format_srt_time(3600.0), "01:00:00,000");
        // SRT uses a comma decimal separator (different from VTT's
        // period); this is the spec, not a typo.
        assert!(format_srt_time(1.5).contains(','));
        assert!(!format_srt_time(1.5).contains('.'));
    }

    #[test]
    fn format_vtt_time_uses_period_separator() {
        // WebVTT spec uses period as the millisecond separator.
        // Drift here would silently break any consumer that
        // distinguishes the two formats (e.g. HTML5 <track>
        // strict parsing).
        assert_eq!(format_vtt_time(0.0), "00:00:00.000");
        assert_eq!(format_vtt_time(1.5), "00:00:01.500");
        assert_eq!(format_vtt_time(3600.5), "01:00:00.500");
        assert!(format_vtt_time(1.5).contains('.'));
        assert!(!format_vtt_time(1.5).contains(','));
    }

    #[test]
    fn format_srt_renders_numbered_cues_skipping_empty() {
        let out = format_segments_srt(&sample_segments());
        assert!(out.contains("1\n00:00:00,000 --> 00:00:01,500\nHello world.\n"));
        assert!(out.contains("2\n00:00:01,500 --> 00:00:03,250\nThis is a test.\n"));
        // The whitespace-only segment must NOT produce a cue 3.
        assert!(!out.contains("3\n"), "empty segments must be skipped");
        // Cues separated by a blank line (SRT idiom).
        assert!(out.ends_with("\n\n"), "SRT must end with a blank line: {out:?}");
    }

    #[test]
    fn format_vtt_starts_with_required_header_and_no_cue_ids() {
        let out = format_segments_vtt(&sample_segments());
        // WebVTT requires the literal "WEBVTT" header followed by a
        // blank line.  Players that don't see this will reject the
        // file.
        assert!(out.starts_with("WEBVTT\n\n"), "got: {out:?}");
        // Period decimal separator
        assert!(out.contains("00:00:00.000 --> 00:00:01.500\nHello world.\n"));
        // No numbered cue identifiers in our output (WebVTT allows
        // them but we don't emit them — the timestamp line is the
        // first non-blank line of each cue).
        assert!(!out.contains("\n1\n"), "should NOT emit numbered cue ids");
    }

    #[test]
    #[test]
    fn is_multilingual_whisper_backend_covers_known_variants() {
        // Pin the membership list — distil-whisper deliberately EXCLUDED
        // because its model is English-only (auto-resolving it as a
        // Whisper-method LID model would always misclassify).  Drift
        // here would either silently degrade LID quality (false-positive
        // matches) OR force users back to passing --lid-model PATH
        // (false-negative matches).
        let multilingual = [
            "whisper",
            "whisper-base",
            "whisper-small",
            "whisper-medium",
            "whisper-large-v3",
        ];
        for b in multilingual {
            assert!(
                is_multilingual_whisper_backend(b),
                "{b:?} should be classified as multilingual whisper",
            );
        }
        let non_multilingual = [
            "distil-whisper",       // EN-only by training
            "parakeet",             // FastConformer-TDT, not whisper
            "qwen3",                // LLM-based, not whisper
            "",                     // empty / unknown
            "Whisper",              // case-sensitive — caller passes the
                                    // registry-canonical name
        ];
        for b in non_multilingual {
            assert!(
                !is_multilingual_whisper_backend(b),
                "{b:?} should NOT be classified as multilingual whisper",
            );
        }
    }

    #[test]
    fn format_segments_empty_input_returns_minimal_output() {
        // Edge case: a backend that returns zero segments shouldn't
        // produce a crash.  SRT gives empty string; VTT gives
        // just the WEBVTT header.
        let empty: Vec<crate::asr::AsrSegment> = vec![];
        assert_eq!(format_segments_srt(&empty), "");
        assert_eq!(format_segments_vtt(&empty), "WEBVTT\n\n");
    }

    #[test]
    fn chat_transcribe_stream_flag_parses() {
        // --stream is the P13.5 follow-up that opts into a rolling-
        // window Whisper streaming decode instead of the buffered
        // transcribe path.  Pin that the flag flips a bool field
        // through clap correctly.
        let cli = Cli::try_parse_from([
            "crispsorter", "chat", "transcribe", "/tmp/foo.wav", "--stream",
        ])
        .expect("--stream should parse");
        match cli.command {
            Command::Chat { cmd: ChatCmd::Transcribe { stream, .. } } => {
                assert!(stream);
            }
            other => panic!("expected Chat Transcribe, got {other:?}"),
        }
    }

    #[test]
    fn chat_transcribe_full_args_parse() {
        // All optional flags set — language hint, explicit model
        // path, output redirect, pure-rust policy, custom data dir,
        // Phase 6 routing knobs (policy/fallback/lid-model/lid-method).
        let cli = Cli::try_parse_from([
            "crispsorter", "chat", "transcribe",
            "/tmp/de.mp3",
            "--backend", "parakeet",
            "--model", "/models/parakeet.gguf",
            "--language", "auto",
            "--output", "/tmp/out.txt",
            "--data-dir", "/tmp/xdg",
            "--pure-rust",
            "--policy", "auto",
            "--fallback", "whisper",
            "--lid-model", "/models/ggml-tiny.bin",
            "--lid-method", "silero",
            "--translate-to", "en",
            "--translate-backend", "m2m100-wmt21",
            "--translate-model", "/models/wmt21-de-en.gguf",
            "--translate-max-tokens", "512",
        ])
        .expect("full-flag transcribe should parse");
        match cli.command {
            Command::Chat { cmd: ChatCmd::Transcribe {
                path, backend, model, language, output, data_dir, pure_rust, stream,
                transcript_format,
                policy, fallback_backend, lid_model, lid_method,
                translate_to, translate_backend, translate_model, translate_max_tokens,
            } } => {
                assert!(!stream, "--stream not set in full-args test (covered separately)");
                assert!(transcript_format.is_none(), "--transcript-format not set in full-args test (covered separately)");
                assert_eq!(path, PathBuf::from("/tmp/de.mp3"));
                assert_eq!(backend, "parakeet");
                assert_eq!(model, Some(PathBuf::from("/models/parakeet.gguf")));
                assert_eq!(language.as_deref(), Some("auto"));
                assert_eq!(output, "/tmp/out.txt");
                assert_eq!(data_dir, Some(PathBuf::from("/tmp/xdg")));
                assert!(pure_rust);
                assert_eq!(policy, LidPolicy::Auto);
                assert_eq!(fallback_backend, "whisper");
                assert_eq!(lid_model, Some(PathBuf::from("/models/ggml-tiny.bin")));
                assert_eq!(lid_method, LidMethodChoice::Silero);
                assert_eq!(translate_to.as_deref(), Some("en"));
                assert_eq!(translate_backend, "m2m100-wmt21");
                assert_eq!(translate_model, Some(PathBuf::from("/models/wmt21-de-en.gguf")));
                assert_eq!(translate_max_tokens, 512);
            }
            other => panic!("expected Chat Transcribe, got {other:?}"),
        }
    }

    #[test]
    fn chat_tts_requires_output_path() {
        // `--output` is the only mandatory flag beyond the text
        // positional — clap should reject if it's missing.
        let err = Cli::try_parse_from([
            "crispsorter", "chat", "tts", "Hello world",
        ])
        .expect_err("tts without --output must error");
        let msg = err.to_string();
        assert!(
            msg.contains("--output") || msg.contains("output"),
            "error must name the missing arg: {msg}"
        );
    }

    #[test]
    fn chat_tts_voice_and_speaker_are_mutually_exclusive() {
        // CrispASR's TTS picks voice EITHER by path (set_voice) OR by
        // name (set_speaker_name) — never both.  clap's
        // `conflicts_with` catches misuse at parse time, before any
        // session load.  Drift here would let the runtime panic on
        // surprising upstream state.
        let err = Cli::try_parse_from([
            "crispsorter", "chat", "tts", "hi",
            "--output", "/tmp/out.wav",
            "--voice", "/voices/sophia.gguf",
            "--speaker", "tara",
        ])
        .expect_err("voice + speaker should conflict");
        assert!(
            err.to_string().contains("cannot be used with"),
            "expected conflict error, got: {err}"
        );
    }

    #[test]
    fn chat_tts_voice_ref_text_requires_voice() {
        // `--voice-ref-text` is meaningless without `--voice` (it
        // describes what the voice reference clip says).  clap's
        // `requires` should error if `--voice-ref-text` is used
        // standalone.  clap's phrasing is "the following required
        // arguments were not provided: --voice <VOICE>" — anchor
        // on the missing-arg name, not the literal verb.
        let err = Cli::try_parse_from([
            "crispsorter", "chat", "tts", "hi",
            "--output", "/tmp/out.wav",
            "--voice-ref-text", "reading test",
        ])
        .expect_err("voice-ref-text without voice should error");
        let msg = err.to_string();
        assert!(
            msg.contains("--voice"),
            "expected error to name the missing --voice arg, got: {msg}"
        );
    }

    #[test]
    fn write_chat_output_writes_file_and_adds_newline() {
        // Round-trip: write a payload, read it back, confirm it ends
        // in a newline (so downstream `read $(...)` etc. behave) and
        // the parent dir was created.
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("crispsorter_chat_out_{nanos}"));
        let path = base.join("sub").join("transcript.txt");

        write_chat_output(&path.to_string_lossy(), "hello world")
            .expect("write_chat_output");
        assert!(path.exists(), "output file must exist");

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "hello world\n", "must add a trailing newline");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_chat_output_preserves_existing_newline() {
        // Don't double-append: payloads that already terminate in '\n'
        // round-trip byte-exact.
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("crispsorter_chat_out_{nanos}.txt"));

        write_chat_output(&base.to_string_lossy(), "hi\n").unwrap();
        let body = std::fs::read_to_string(&base).unwrap();
        assert_eq!(body, "hi\n");

        let _ = std::fs::remove_file(&base);
    }
}
