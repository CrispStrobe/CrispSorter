pub mod asr;
pub mod audio;
pub mod bg_ingest;
pub mod images;
/// Re-export the extracted crispcat workspace crate as `catalog` so existing
/// `crate::catalog::…` paths in the rest of the binary keep working unchanged.
pub use crispcat as catalog;
pub mod cli;
pub mod drives;
pub mod extractors;
pub mod sync;
pub mod index;
pub mod jobs;
pub mod migrations;
pub mod tts;
pub mod volume;
pub mod watcher;

/// Speak `text` aloud via the platform's native TTS synth.
///
/// Replaces any in-flight utterance — calling `tts_speak` twice in a row
/// kills the first synth and starts the second. The Rust handler returns
/// as soon as the synth process is spawned; speaking happens in the
/// background. Use `tts_stop` to interrupt mid-utterance (e.g. on the
/// chat Stop button).
#[tauri::command]
async fn tts_speak(
    state: tauri::State<'_, AppState>,
    text: String,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    // Stop any current utterance first — overlapping synths make the
    // output unintelligible and the user's mental model is "speak this
    // now, not after the previous reply finishes".
    {
        let mut slot = state.tts_process.lock().await;
        if let Some(mut prev) = slot.take() {
            tts::kill_quietly(&mut prev).await;
        }
    }
    let child = tts::spawn_speak(&text)
        .await
        .map_err(|e| format!("TTS spawn failed: {e:#}"))?;
    let mut slot = state.tts_process.lock().await;
    *slot = Some(child);
    Ok(())
}

/// Start watching `folder` recursively. Idempotent — adding the same
/// folder twice does not create a duplicate watcher.
#[tauri::command]
async fn watch_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    folder: String,
) -> Result<(), String> {
    let path = std::path::PathBuf::from(folder);
    let mut guard = state.watcher.lock().await;
    watcher::start(&mut guard, app, path).map_err(|e| format!("watch_start failed: {e:#}"))
}

/// Stop watching a single folder. Returns true if a watcher was
/// actually removed; false when the folder wasn't being watched
/// (idempotent — frontend can call this without checking first).
#[tauri::command]
async fn watch_stop_one(
    state: tauri::State<'_, AppState>,
    folder: String,
) -> Result<bool, String> {
    let path = std::path::PathBuf::from(folder);
    let mut guard = state.watcher.lock().await;
    Ok(watcher::stop_one(&mut guard, &path))
}

/// Stop all active watchers. No-op when none are running.
#[tauri::command]
async fn watch_stop_all(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.watcher.lock().await;
    watcher::stop_all(&mut guard);
    Ok(())
}

/// Returns all currently watched folders (sorted, canonical paths).
#[tauri::command]
async fn watch_list(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let guard = state.watcher.lock().await;
    Ok(guard.list())
}

// ── Volume awareness (PLAN P7.6) ────────────────────────────────────────

/// List currently-mounted volumes with their stable id, mount point,
/// and human label. Used by the frontend to show "Volumes" in index
/// filters once the matching search-time filter lands. Each call
/// shells out (`mount` / `findmnt` / `wmic`); the list is small and
/// the call is rare (a settings panel open) so we don't cache.
#[tauri::command]
async fn volume_list_mounted() -> Result<Vec<volume::MountedVolume>, String> {
    Ok(tokio::task::spawn_blocking(volume::list_mounted_volumes)
        .await
        .map_err(|e| format!("volume_list_mounted join error: {e}"))?)
}

// ── Catalog (Cathy/Catfish .caf) commands ────────────────────────────────
//
// Phase 1 of PLAN P6: load/save .caf files + scan a folder into a
// FileIndex. The frontend gets the FileIndex JSON-serialized; the
// `size_index` and `hash_index` buckets are #[serde(skip)] so the
// payload stays linear in `all_files` length.

/// Read a .caf file and return its full FileIndex. For huge catalogs,
/// prefer `catalog_metadata` first to decide whether to load the body.
#[tauri::command]
async fn catalog_load_caf(path: String) -> Result<catalog::index::FileIndex, String> {
    let p = std::path::PathBuf::from(path);
    tokio::task::spawn_blocking(move || catalog::caf::read_file(&p).map_err(|e| e.to_string()))
        .await
        .map_err(|e| format!("catalog_load_caf join error: {e}"))?
}

/// Write a FileIndex to disk in v8 .caf format. The `created_date`
/// (epoch seconds) is what gets stamped into the header — pass 0 to
/// use the current time.
#[tauri::command]
async fn catalog_save_caf(
    path: String,
    index: catalog::index::FileIndex,
    created_date: u32,
) -> Result<(), String> {
    let p = std::path::PathBuf::from(path);
    let date = if created_date == 0 {
        catalog::caf::unix_now()
    } else {
        created_date
    };
    tokio::task::spawn_blocking(move || {
        catalog::caf::write_file(&p, &index, date).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("catalog_save_caf join error: {e}"))?
}

/// Walk a directory tree (rayon-parallel) and produce a FileIndex.
/// Compute the SHA-256 hex digest of a single file. Used by the batch
/// pre-processor (P15a) to confirm content-identical duplicates after a
/// size-bucket match — cheaper than reading the file in JS.
/// Returns an error string if the file is unreadable.
#[tauri::command]
async fn file_sha256(path: String) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
        let mut h = Sha256::new();
        h.update(&bytes);
        Ok(hex::encode(h.finalize()))
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

/// `hash_algo` is one of `"md5"`, `"sha1"`, `"sha256"`, or absent for
/// no hashing (Cathy-classic behaviour).
#[tauri::command]
async fn catalog_scan_dir(
    root: String,
    hash_algo: Option<String>,
    max_size_bytes: Option<u64>,
) -> Result<catalog::index::FileIndex, String> {
    let p = std::path::PathBuf::from(root);
    let opts = catalog::scan::ScanOptions {
        hash: hash_algo
            .as_deref()
            .and_then(|s| match s.to_ascii_lowercase().as_str() {
                "md5" => Some(catalog::scan::HashAlgo::Md5),
                "sha1" => Some(catalog::scan::HashAlgo::Sha1),
                "sha256" => Some(catalog::scan::HashAlgo::Sha256),
                _ => None,
            }),
        max_size_bytes,
        follow_symlinks: false,
    };
    tokio::task::spawn_blocking(move || catalog::scan::scan_dir(&p, opts).map_err(|e| e.to_string()))
        .await
        .map_err(|e| format!("catalog_scan_dir join error: {e}"))?
}

/// Cheap header-only read for index-listing UIs. Avoids decoding the
/// full element block — typically a few-hundred-byte read regardless
/// of catalog size.
#[derive(serde::Serialize)]
struct CafMetadataDto {
    version: u8,
    device: String,
    volume: String,
    alias: String,
    serial: u32,
    comment: String,
    date: u32,
    file_count: i32,
    total_size: u64,
    archive: i16,
    freesize: f32,
}

/// Find duplicates of `source` files inside one or more `destinations`.
///
/// `source` and `destinations` can be either .caf paths (loaded
/// automatically) or directory paths (scanned on the fly with the
/// configured hash strategy). Mixed inputs are allowed.
///
/// `strategy` is one of `"name-and-size"` (Cathy default — same name +
/// same size) or `"hash:<algo>"` where `<algo>` is `md5` / `sha1` /
/// `sha256`. Hash strategy reads bytes for size-collision candidates
/// only, so it stays cheap on large catalogs.
#[tauri::command]
async fn catalog_find_duplicates(
    source: String,
    destinations: Vec<String>,
    strategy: Option<String>,
) -> Result<Vec<catalog::dedup::DuplicateMatch>, String> {
    let strategy = parse_match_strategy(strategy.as_deref())?;
    tokio::task::spawn_blocking(move || -> Result<_, String> {
        let src_index = load_or_scan_for_dedup(&source)?;
        let mut all_matches: Vec<catalog::dedup::DuplicateMatch> = Vec::new();
        for dest in destinations {
            let dst_index = load_or_scan_for_dedup(&dest)?;
            let opts = catalog::dedup::DedupOptions { strategy };
            let mut matches = catalog::dedup::find_duplicates(&src_index, &dst_index, &opts);
            all_matches.append(&mut matches);
        }
        Ok(all_matches)
    })
    .await
    .map_err(|e| format!("catalog_find_duplicates join error: {e}"))
    .and_then(|r| r)
}

/// Render a deletion script (bash / batch / powershell) from a
/// duplicate-match list. The script never auto-runs — the caller is
/// expected to save and review before executing.
///
/// `format` is `"bash"`, `"batch"`, or `"powershell"`; `target` is
/// `"destinations"` (default — delete duplicates, keep the source) or
/// `"source"` (delete the source, keep the destinations).
#[tauri::command]
async fn catalog_generate_deletion_script(
    matches: Vec<catalog::dedup::DuplicateMatch>,
    format: Option<String>,
    target: Option<String>,
) -> Result<String, String> {
    let format = match format.as_deref().unwrap_or("bash").to_ascii_lowercase().as_str() {
        "bash" => catalog::dedup::ScriptFormat::Bash,
        "batch" | "bat" | "cmd" => catalog::dedup::ScriptFormat::Batch,
        "powershell" | "ps" | "ps1" => catalog::dedup::ScriptFormat::Powershell,
        other => return Err(format!("unknown script format `{other}`")),
    };
    let target = match target.as_deref().unwrap_or("destinations") {
        "destinations" | "dest" => catalog::dedup::DeletionTarget::Destinations,
        "source" | "src" => catalog::dedup::DeletionTarget::Source,
        other => return Err(format!("unknown deletion target `{other}`")),
    };
    Ok(catalog::dedup::generate_deletion_script(&matches, format, target))
}

/// Parse the user-facing `strategy` string into the typed enum.
///
/// Accepts:
/// * `None`, `""`, or `"name-and-size"` → name+size match (Cathy default).
/// * `"hash:md5"` / `"hash:sha1"` / `"hash:sha256"` → byte-level match.
/// * Bare `"md5"` / `"sha1"` / `"sha256"` → same as `hash:<algo>` (so a
///   simpler frontend dropdown works without prefix gymnastics).
fn parse_match_strategy(s: Option<&str>) -> Result<catalog::dedup::MatchStrategy, String> {
    let s = s.unwrap_or("").to_ascii_lowercase();
    match s.as_str() {
        "" | "name-and-size" => Ok(catalog::dedup::MatchStrategy::NameAndSize),
        "hash:md5" | "md5" => Ok(catalog::dedup::MatchStrategy::Hash(
            catalog::scan::HashAlgo::Md5,
        )),
        "hash:sha1" | "sha1" => Ok(catalog::dedup::MatchStrategy::Hash(
            catalog::scan::HashAlgo::Sha1,
        )),
        "hash:sha256" | "sha256" => Ok(catalog::dedup::MatchStrategy::Hash(
            catalog::scan::HashAlgo::Sha256,
        )),
        other => Err(format!(
            "unknown match strategy `{other}` (try name-and-size / hash:md5 / hash:sha1 / hash:sha256)"
        )),
    }
}

/// Toggle whether a catalog is materialized into the LanceDB
/// `catalog_entries` table — i.e. whether its entries participate in
/// the unified search alongside the documents table.
///
/// `active = true`: load the .caf, insert all rows tagged with
///   `catalog_path`. Replaces any prior rows for that catalog (calls
///   the drop path first) so a re-materialize after a refresh is
///   idempotent.
/// `active = false`: delete every row where `catalog_path = X`.
#[tauri::command]
async fn catalog_set_active(
    catalog_path: String,
    active: bool,
    data_dir: String,
) -> Result<usize, String> {
    let cp = std::path::PathBuf::from(catalog_path);
    let dd = std::path::PathBuf::from(data_dir);
    if active {
        // Off-thread: .caf I/O + Arrow build + Lance write are all
        // sync-ish work that'd block the runtime if we ran them inline.
        let cp_clone = cp.clone();
        let idx = tokio::task::spawn_blocking(move || {
            catalog::caf::read_file(&cp_clone).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("read_file join error: {e}"))??;
        catalog::lance::materialize(&dd, &cp, &idx)
            .await
            .map_err(|e| e.to_string())
    } else {
        catalog::lance::drop_catalog(&dd, &cp)
            .await
            .map(|_| 0)
            .map_err(|e| e.to_string())
    }
}

// ── Background ingest commands (PLAN P7.4.2b) ────────────────────────────

/// Push paths into the background ingest queue and start the worker
/// if it isn't already running. Each path goes through the per-filetype
/// extractor (P7.4.1) → embed → write to LanceDB cycle.
///
/// Idempotent: re-enqueueing the same path is harmless — the underlying
/// ingest dedups by source_hash. Returns the post-enqueue status snapshot.
#[tauri::command]
async fn bg_ingest_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
    owner_id: Option<String>,
) -> Result<bg_ingest::BgStatusSnapshot, String> {
    let items: Vec<bg_ingest::PendingIngest> = paths
        .into_iter()
        .map(|p| bg_ingest::PendingIngest {
            path: std::path::PathBuf::from(p),
            owner_id: owner_id.clone(),
            title: None,
            author: None,
            year: None,
            language: None,
        })
        .collect();
    let bg = state.bg_ingest.clone();
    {
        let mut g = bg.lock().await;
        g.enqueue(items);
    }
    bg_ingest::ensure_worker(bg.clone(), app);
    let snap = bg.lock().await.snapshot();
    Ok(snap)
}

#[tauri::command]
async fn bg_ingest_status(
    state: tauri::State<'_, AppState>,
) -> Result<bg_ingest::BgStatusSnapshot, String> {
    Ok(state.bg_ingest.lock().await.snapshot())
}

#[tauri::command]
async fn bg_ingest_pause(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.bg_ingest.lock().await.pause();
    Ok(())
}

#[tauri::command]
async fn bg_ingest_resume(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.bg_ingest.lock().await.resume();
    Ok(())
}

#[tauri::command]
async fn bg_ingest_cancel(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.bg_ingest.lock().await.cancel();
    Ok(())
}

#[tauri::command]
async fn bg_ingest_clear(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.bg_ingest.lock().await.clear();
    Ok(())
}

/// Update the OCR options used by the background ingest worker.
/// Called from Settings.svelte when ocrEnabled or ocrTier changes.
#[tauri::command]
async fn bg_ingest_set_ocr(
    state: tauri::State<'_, AppState>,
    enabled: bool,
    tier: String,
    rec_lang: Option<String>,
) -> Result<(), String> {
    let mut bg = state.bg_ingest.lock().await;
    bg.ocr_enabled = enabled;
    bg.ocr_tier = tier;
    if let Some(lang) = rec_lang { bg.ocr_rec_lang = lang; }
    Ok(())
}

/// Export the LanceDB documents table to a .caf file (PLAN P6 4d).
///
/// Walks the documents table once (whole-doc rows only, `chunk_index =
/// 0`), extracts the local file path from each `crisp+local://...` URI,
/// stat()s the file for current size + mtime (falls back to 0 if the
/// file's gone), and writes the result as a v8 .caf at `out_path`.
///
/// Skips non-local URIs (`crisp+vps`, `crisp+internxt*`) — the .caf
/// format has no place for those provenance bits, and a Cathy reader
/// looking at one would be confused by an absolute path with no
/// matching device.
///
/// Returns the number of entries actually written. The caller knows
/// the discrepancy with the documents-table row count if any URIs got
/// skipped.
#[tauri::command]
async fn catalog_export_sorted(
    state: tauri::State<'_, AppState>,
    out_path: String,
    limit: Option<usize>,
) -> Result<usize, String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled — initialise it first".into());
    }
    let local = lock
        .local
        .as_ref()
        .ok_or("Local index not available (remote backend?)")?
        .clone();
    drop(lock);

    let docs = local
        .list_documents(limit.unwrap_or(usize::MAX))
        .await
        .map_err(|e| e.to_string())?;

    tokio::task::spawn_blocking(move || -> Result<usize, String> {
        use catalog::index::{FileEntry, FileIndex};
        let out = std::path::PathBuf::from(out_path);
        // Root = "/" is the safe default — entries carry absolute paths
        // from arbitrary drives, the writer's dir-allocation walks
        // each up to the root regardless.
        let mut idx = FileIndex::new(std::path::PathBuf::from("/"), cfg!(windows));

        for hit in docs {
            // Extract the local filesystem path from the URI; skip
            // non-local locations.
            let loc = match index::location::FileLocation::from_uri(&hit.location_uri) {
                Ok(loc) => loc,
                Err(_) => continue,
            };
            let path = match loc {
                index::location::FileLocation::Local { path, .. } => path,
                _ => continue,
            };
            // stat() for live size + mtime; fall through to 0 / 0 when
            // the file's been moved / removed since indexing.
            let (size, mtime) = match std::fs::metadata(&path) {
                Ok(m) => {
                    let s = m.len();
                    let t = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as u32)
                        .unwrap_or(0);
                    (s, t)
                }
                Err(_) => (0, 0),
            };
            idx.add(FileEntry::new(path, size, mtime));
        }

        let n = idx.len();
        catalog::caf::write_file(&out, &idx, catalog::caf::unix_now())
            .map_err(|e| e.to_string())?;
        Ok(n)
    })
    .await
    .map_err(|e| format!("catalog_export_sorted join error: {e}"))?
}

/// Substring search over filenames in the materialized catalog table.
/// Filenames only — for path-component or content matching, the existing
/// `index_search` covers documents-table rows; future Phase 4c will
/// merge both channels via RRF.
#[tauri::command]
async fn catalog_search(
    query: String,
    data_dir: String,
    limit: Option<usize>,
) -> Result<Vec<catalog::lance::CatalogHit>, String> {
    let dd = std::path::PathBuf::from(data_dir);
    catalog::lance::search(&dd, &query, limit)
        .await
        .map_err(|e| e.to_string())
}

/// Distinct catalog paths currently materialized — useful for the UI
/// to verify the search-side state matches the registry's `active`
/// flag (frontend store can drift from backend reality if a write
/// failed).
#[tauri::command]
async fn catalog_active_list(data_dir: String) -> Result<Vec<String>, String> {
    let dd = std::path::PathBuf::from(data_dir);
    catalog::lance::list_active(&dd)
        .await
        .map_err(|e| e.to_string())
}

/// `source` / `destination` arg can be either a .caf path or a folder.
/// Detect by extension + file-vs-dir; load the .caf or scan the folder
/// (no inline hashing — dedup_options decides whether to hash).
fn load_or_scan_for_dedup(path: &str) -> Result<catalog::index::FileIndex, String> {
    let p = std::path::PathBuf::from(path);
    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("caf") {
        catalog::caf::read_file(&p).map_err(|e| format!("loading {}: {e}", p.display()))
    } else if p.is_dir() {
        catalog::scan::scan_dir(&p, catalog::scan::ScanOptions::default())
            .map_err(|e| format!("scanning {}: {e}", p.display()))
    } else {
        Err(format!(
            "{} is neither a .caf file nor a directory",
            p.display()
        ))
    }
}

#[tauri::command]
async fn catalog_metadata(path: String) -> Result<CafMetadataDto, String> {
    let p = std::path::PathBuf::from(path);
    tokio::task::spawn_blocking(move || {
        catalog::caf::read_metadata(&p).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("catalog_metadata join error: {e}"))
    .and_then(|r| r)
    .map(|m| CafMetadataDto {
        version: m.version,
        device: m.device,
        volume: m.volume,
        alias: m.alias,
        serial: m.serial,
        comment: m.comment,
        date: m.date,
        file_count: m.file_count,
        total_size: m.total_size,
        archive: m.archive,
        freesize: m.freesize,
    })
}

/// Stop any in-flight TTS utterance. No-op when nothing is speaking.
#[tauri::command]
async fn tts_stop(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut slot = state.tts_process.lock().await;
    if let Some(mut child) = slot.take() {
        tts::kill_quietly(&mut child).await;
    }
    Ok(())
}

/// Audio/video extraction result returned to the JS-side
/// `extractText` dispatcher.  Mirrors a subset of the Rust-side
/// `ExtractedDocument` — only the fields the frontend actually
/// surfaces today (text + detected source language).  Adding
/// fields here is cheap (serde defaults handle frontend rollback)
/// so future steps can grow the shape without breaking older
/// JS callers.
#[derive(serde::Serialize)]
struct AudioExtractResult {
    text: String,
    /// Whisper-detected source language as an ISO 639-1 code
    /// (e.g. `"en"`, `"de"`, `"bs"`).  `None` when the ASR
    /// backend didn't run a LID pass or the audio was too short
    /// for a confident classification.
    language: Option<String>,
}

/// File-path-based audio/video extraction for the JS-side
/// `extractText` dispatcher.  Wraps [`extractors::audio::extract`]
/// (symphonia tier-1 + ffmpeg fallback + shared CrispASR handle) and
/// returns transcript + detected source language — keeps the large
/// `Vec<f32>` PCM buffer entirely inside the Rust process.  Use this
/// from the frontend whenever a dropped/picked file's extension is
/// in `MULTIMODAL_EXTENSIONS` (the audio/video subset) — the same
/// path the bg_ingest classifier walks for index-time audio.
///
/// Spawns into a blocking thread because the audio extractor builds
/// a nested current-thread tokio runtime (the standard pattern for
/// bridging the sync extractor boundary into async ASR).
///
/// Errors out with a clear --features hint when the binary was built
/// without `crispasr-*`; the JS side surfaces the message verbatim
/// on the failing entry so the user knows to rebuild via
/// `enable-crispasr.sh` / `.ps1`.
#[tauri::command]
async fn audio_extract_text(path: String) -> Result<AudioExtractResult, String> {
    let p = std::path::PathBuf::from(&path);
    tokio::task::spawn_blocking(move || {
        extractors::audio::extract(&p)
            .map(|doc| AudioExtractResult {
                text: doc.full_text,
                language: doc.language,
            })
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("audio_extract_text join error: {e}"))?
}

/// Lightweight L2 audio metadata probe — symphonia format reader
/// only, no decode pass.  Returns duration / codec / sample rate /
/// channels / bitrate so the UI can pre-fill row tooltips and the
/// (default-hidden) duration column.  Same data goes into the
/// LanceDB `audio_*` columns at index-time via the bg_ingest path
/// (added by schema-migration v101 in P13.6 Step 3c).
///
/// Cost: O(1) container header scan; sub-millisecond on a typical
/// 200 MB mp3 vs the 30-60 s full ASR transcribe.  spawn_blocking
/// because the long-tail of containers occasionally needs a deeper
/// header read (truncated mp4 / streaming m4a).
#[tauri::command]
async fn audio_metadata(path: String) -> Result<audio::probe::AudioMetadata, String> {
    let p = std::path::PathBuf::from(&path);
    tokio::task::spawn_blocking(move || {
        audio::probe::probe_metadata(&p).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("audio_metadata join error: {e}"))?
}

/// Transcribe Float32 PCM 16 kHz mono audio to text via CrispASR.
///
/// Lazy-initializes the ASR handle on first call (the handle's first
/// `transcribe()` then triggers the model download + load). The model
/// cache is shared with the embedder via the same `model_cache_dir`
/// resolution flow.
#[tauri::command]
async fn asr_transcribe(
    state: tauri::State<'_, AppState>,
    pcm: Vec<f32>,
) -> Result<String, String> {
    if pcm.is_empty() {
        return Ok(String::new());
    }
    // Resolve model cache from the active IndexConfig (so voice and
    // embedder share the same external-volume override) with a sane
    // app-data fallback when the index is disabled.
    let cache_dir = {
        let idx = state.index.lock().await;
        let data_dir_for_default = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|h| h.join(".cache").join("crispsorter"))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        index::resolve_model_cache_dir(&idx.config, &data_dir_for_default)
    };

    let handle = {
        let mut slot = state.asr.lock().await;
        if slot.is_none() {
            *slot = Some(asr::AsrHandle::new(asr::AsrConfig::default(), cache_dir));
        }
        slot.as_ref().unwrap().clone()
    };

    handle
        .transcribe(pcm)
        .await
        .map_err(|e| format!("ASR transcribe failed: {e:#}"))
}

use futures_util::StreamExt;
use mistralrs::{
    best_device, initialize_logging, GgufModelBuilder, IsqType, Model, PagedAttentionMetaBuilder,
    RequestBuilder, TextMessageRole, TextMessages,
};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tokio::sync::Mutex;

// ── App-wide log relay ────────────────────────────────────────────────────
// Ring buffer of recent log messages + Tauri event emission so the frontend
// can display a live log panel.  Works even when there is no console
// (Windows release builds with `windows_subsystem = "windows"`).

use std::sync::LazyLock;

/// In-memory ring buffer of the most recent log messages.
static LOG_BUFFER: LazyLock<std::sync::Mutex<LogRing>> =
    LazyLock::new(|| std::sync::Mutex::new(LogRing::new(500)));

struct LogRing {
    entries: Vec<LogEntry>,
    max: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct LogEntry {
    pub ts: f64,       // seconds since UNIX epoch
    pub level: String, // "info", "warn", "error"
    pub msg: String,
}

impl LogRing {
    fn new(max: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max),
            max,
        }
    }
    fn push(&mut self, entry: LogEntry) {
        if self.entries.len() >= self.max {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }
    fn snapshot(&self) -> Vec<LogEntry> {
        self.entries.clone()
    }
}

/// Global app handle set once in `run()`, used by `app_log!` from anywhere.
static APP_HANDLE: LazyLock<std::sync::Mutex<Option<tauri::AppHandle>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

/// Log a message to stderr, the ring buffer, and the frontend (via Tauri event).
pub fn app_log(level: &str, msg: String) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    eprintln!("[{level}] {msg}");
    let entry = LogEntry {
        ts,
        level: level.to_string(),
        msg,
    };
    if let Ok(mut buf) = LOG_BUFFER.lock() {
        buf.push(entry.clone());
    }
    if let Ok(guard) = APP_HANDLE.lock() {
        if let Some(ref handle) = *guard {
            let _ = handle.emit("app-log", &entry);
        }
    }
}

/// Convenience macro: `app_log!("info", "loaded model {}", name);`
#[macro_export]
macro_rules! app_log {
    ($level:expr, $($arg:tt)*) => {
        $crate::app_log($level, format!($($arg)*))
    };
}

/// Fire-and-forget Tauri event using the global `APP_HANDLE` set up in
/// `run()`. Lets module-level code (where there's no `tauri::AppHandle`
/// in scope — e.g. the embedder prefetch progress callback) emit events
/// without threading the handle through every async call.
pub fn emit_app_event<T: serde::Serialize + Clone>(event: &str, payload: &T) {
    if let Ok(guard) = APP_HANDLE.lock() {
        if let Some(ref handle) = *guard {
            let _ = handle.emit(event, payload);
        }
    }
}

#[tauri::command]
fn get_logs() -> Vec<LogEntry> {
    LOG_BUFFER.lock().map(|b| b.snapshot()).unwrap_or_default()
}

/// Pipe a frontend log entry into the same ring buffer + Tauri event
/// channel the Rust side uses, so per-file extraction errors and
/// tauri-plugin-fs permission rejections show up in the in-app Logs
/// panel alongside Rust-side messages.
#[tauri::command]
fn frontend_log(level: String, msg: String) {
    // Clamp the level to the small set the LogPanel knows how to colour.
    let level = match level.as_str() {
        "error" | "warn" | "info" => level,
        _ => "info".to_string(),
    };
    app_log(&level, msg);
}

#[derive(Serialize)]
pub struct FileEntry {
    path: String,
    size: u64,
}

#[derive(Serialize, Clone)]
struct DownloadProgress {
    id: String,
    received: u64,
    total: u64,
}

// Global state to hold the high-level Model instance and current model path
// Using tokio::sync::Mutex because guards need to be Send across await points in Tauri commands
use tokio::process::Child as TokioChild;

pub struct AppState {
    model: Mutex<Option<Arc<Model>>>,
    current_model_path: Mutex<Option<String>>,
    sidecar_process: Mutex<Option<TokioChild>>,
    mlx_process: Mutex<Option<TokioChild>>,
    ollama_process: Mutex<Option<TokioChild>>,
    pub index: Mutex<index::IndexState>,
    /// Speech-to-text handle. Lazy-loaded on first `asr_transcribe` call.
    /// `None` until the user invokes voice input; `Some` thereafter.
    pub asr: Mutex<Option<asr::AsrHandle>>,
    /// Currently-speaking TTS child process, if any. Held so `tts_stop`
    /// can kill it mid-utterance.
    pub tts_process: Mutex<Option<TokioChild>>,
    /// Folder-watcher state. Single watched directory for v1; the
    /// `notify::RecommendedWatcher` lives inside the state and gets
    /// dropped when the user changes folders or stops watching.
    pub watcher: Mutex<watcher::WatcherState>,
    /// Background full-content ingest queue + worker (PLAN P7.4.2b).
    /// Lives in its own Arc so the worker task can hold a reference
    /// without borrowing the AppState lifetime.
    pub bg_ingest: Arc<Mutex<bg_ingest::BackgroundIngest>>,
    /// Counter of in-flight foreground searches (PLAN P7.4.4 — QoS).
    /// `index_search` increments on entry, decrements on exit via a
    /// RAII guard. The bg_ingest worker observes the count at the top
    /// of each iteration and yields back if non-zero, so foreground
    /// queries don't get stuck behind a background embed batch.
    /// AtomicUsize so reads + writes are lock-free — neither side
    /// pays for a Mutex hop.
    pub foreground_active: Arc<std::sync::atomic::AtomicUsize>,
    /// Durable per-file ingest job queue (PLAN P10 job persistence).
    /// Initialized in the Tauri setup hook once `app_data_dir` is known.
    /// `None` only during the very brief window before setup completes;
    /// all `jobs_*` commands return an error if accessed before init.
    pub job_queue: Arc<std::sync::Mutex<Option<jobs::JobQueue>>>,
    /// App data directory, set once in the Tauri setup hook.
    /// Used by drive_* commands and any future component that needs the
    /// data-dir without going through the index or job subsystems.
    pub data_dir: tokio::sync::Mutex<Option<std::path::PathBuf>>,
}

#[tauri::command]
async fn start_llamacpp_sidecar(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    model_path: String,
    port: u16,
) -> Result<String, String> {
    let mut sidecar_lock = state.sidecar_process.lock().await;

    // Kill existing process if running
    if let Some(mut child) = sidecar_lock.take() {
        let _ = child.kill().await;
    }

    app_log!("info", "Starting llama-server sidecar, port={}", port);
    app_log!("info", "Model path: {}", model_path);

    // Resolve the bin directory: in dev it's src-tauri/bin, in release it's next to the exe
    let bin_dir = if cfg!(debug_assertions) {
        let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
        // target/debug/tauri-app.exe  →  src-tauri/bin
        exe_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("bin")
    } else {
        let resource_dir = app_handle
            .path()
            .resource_dir()
            .map_err(|e: tauri::Error| e.to_string())?;
        resource_dir.join("bin")
    };

    let bin_dir_str = bin_dir.to_string_lossy().to_string();
    println!("[Sidecar] Library/Bin path: {}", bin_dir_str);

    // Locate the llama-server executable (Tauri appends the target triple)
    let exe_name = if cfg!(windows) {
        "llama-server-x86_64-pc-windows-msvc.exe"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "llama-server-aarch64-apple-darwin"
        } else {
            "llama-server-x86_64-apple-darwin"
        }
    } else {
        "llama-server-x86_64-unknown-linux-gnu"
    };

    let exe_path = bin_dir.join(exe_name);
    if !exe_path.exists() {
        return Err(format!(
            "llama-server binary not found at: {}",
            exe_path.display()
        ));
    }

    // Build PATH so Windows can find the DLLs next to the exe
    let current_path = std::env::var("PATH").unwrap_or_default();
    let augmented_path = if cfg!(windows) {
        format!("{};{}", bin_dir_str, current_path)
    } else {
        current_path
    };

    // Always request maximum GPU offload; llama.cpp silently falls back to CPU if no GPU backend is loaded
    let ngl_value = "99";

    let mut child = tokio::process::Command::new(&exe_path)
        .args([
            "-m",
            &model_path,
            "--port",
            &port.to_string(),
            "--host",
            "0.0.0.0",
            "-ngl",
            ngl_value,
            "--parallel",
            "1",
            "-c",
            "4096",
        ])
        // Set CWD to bin dir so Windows finds the DLLs relative to the exe
        .current_dir(&bin_dir)
        .env("PATH", &augmented_path)
        // Tells ggml_backend_load_all() where to find backend DLLs
        .env("GGML_BACKEND_PATH", &bin_dir_str)
        .env("DYLD_LIBRARY_PATH", &bin_dir_str) // macOS
        .env("LD_LIBRARY_PATH", &bin_dir_str) // Linux
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            println!("[Sidecar] SPAWN ERROR: {}", e);
            e.to_string()
        })?;

    println!("[Sidecar] Spawned PID: {:?}", child.id());

    // Stream stdout
    if let Some(stdout) = child.stdout.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[llama.cpp] {}", line);
                let _ = app.emit("sidecar-log", &line);
            }
        });
    }

    // Stream stderr + detect early exit
    if let Some(stderr) = child.stderr.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("[llama.cpp] ERR: {}", line);
                let _ = app.emit("sidecar-log", format!("[ERR] {}", line));
                // Detect a fatal startup error and surface it immediately
                if line.contains("no backends are loaded")
                    || line.contains("failed to load model")
                    || line.contains("exiting due to")
                {
                    let _ = app.emit("sidecar-failed", &line);
                }
            }
        });
    }

    // Health-check loop — emits sidecar-ready or sidecar-failed
    {
        let app = app_handle.clone();
        let health_url = format!("http://localhost:{}/health", port);
        println!("[Sidecar] Health check target: {}", health_url);
        tokio::spawn(async move {
            for attempt in 1..=30u32 {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                match reqwest::get(&health_url).await {
                    Ok(r) if r.status().is_success() => {
                        println!("[Sidecar] llama-server ready after {}s", attempt * 2);
                        let _ = app.emit("sidecar-ready", true);
                        return;
                    }
                    _ => {
                        println!("[Sidecar] Waiting for server... attempt {}/30", attempt);
                    }
                }
            }
            println!("[Sidecar] llama-server did not become ready within 60s");
            let _ = app.emit("sidecar-failed", "Server did not respond within 60s");
        });
    }

    *sidecar_lock = Some(child);
    Ok("Sidecar starting".to_string())
}

#[tauri::command]
async fn stop_llamacpp_sidecar(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut sidecar_lock = state.sidecar_process.lock().await;
    if let Some(mut child) = sidecar_lock.take() {
        let _ = child.kill().await;
        println!("[Sidecar] Stopped.");
    }
    Ok(())
}

#[tauri::command]
async fn start_mlx_server(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    model_path: String,
    port: u16,
) -> Result<String, String> {
    let mut mlx_lock = state.mlx_process.lock().await;
    if let Some(mut child) = mlx_lock.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // Tauri inherits a minimal PATH — augment with common Python install locations
    let home = std::env::var("HOME").unwrap_or_default();
    let current_path = std::env::var("PATH").unwrap_or_default();
    let augmented_path = format!(
        "{home}/.local/bin:{home}/miniconda3/bin:{home}/miniconda3/condabin:{home}/anaconda3/bin:{home}/.pyenv/shims:{home}/.pyenv/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:{current_path}"
    );

    // Resolve the real binary path via login shell (handles conda/pyenv/homebrew)
    let resolved_bin = tokio::process::Command::new("/bin/zsh")
        .args(["-l", "-c", "which mlx_lm.server 2>/dev/null"])
        .env("PATH", &augmented_path)
        .output()
        .await
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mlx_lm.server".to_string());

    println!(
        "[MLX] Resolved binary: '{}' — model: {}, port: {}",
        resolved_bin, model_path, port
    );

    let mut child = tokio::process::Command::new(&resolved_bin)
        .args([
            "--model",
            &model_path,
            "--port",
            &port.to_string(),
            "--trust-remote-code",
        ])
        .env("PATH", &augmented_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to start '{}': {}. Install with: pip install mlx-lm",
                resolved_bin, e
            )
        })?;

    if let Some(stdout) = child.stdout.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[MLX] {}", line);
                let _ = app.emit("mlx-log", &line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[MLX ERR] {}", line);
                let _ = app.emit("mlx-log", &line);
            }
        });
    }

    // Poll until server is accepting connections, then emit mlx-ready
    {
        let app = app_handle.clone();
        let health_url = format!("http://localhost:{}/v1/models", port);
        tokio::spawn(async move {
            for attempt in 1..=60u32 {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                match reqwest::get(&health_url).await {
                    Ok(r) if r.status().is_success() => {
                        println!("[MLX] Server ready after {}s", attempt * 2);
                        let _ = app.emit("mlx-ready", true);
                        return;
                    }
                    _ => {
                        println!("[MLX] Waiting for server... attempt {}/60", attempt);
                    }
                }
            }
            println!("[MLX] Server did not become ready within 120s");
            let _ = app.emit(
                "mlx-log",
                "[MLX] Server did not respond within 120s — check logs",
            );
        });
    }

    println!("[MLX] Server spawned (PID: {:?})", child.id());
    *mlx_lock = Some(child);
    Ok(format!("MLX server starting on port {}", port))
}

#[tauri::command]
fn get_mlx_cache_dir() -> String {
    std::env::var("HF_HUB_CACHE")
        .or_else(|_| std::env::var("HF_HOME").map(|h| format!("{}/hub", h)))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{}/.cache/huggingface/hub", home)
        })
}

#[tauri::command]
fn check_mlx_models_cached(repo_ids: Vec<String>) -> Vec<bool> {
    let hub_dir = std::path::PathBuf::from(get_mlx_cache_dir());
    repo_ids
        .iter()
        .map(|repo_id| {
            let dir_name = format!("models--{}", repo_id.replace('/', "--"));
            hub_dir.join(&dir_name).exists()
        })
        .collect()
}

#[tauri::command]
async fn delete_mlx_model(repo_id: String) -> Result<String, String> {
    let dir_name = format!("models--{}", repo_id.replace('/', "--"));
    let cache_dir = std::path::PathBuf::from(get_mlx_cache_dir()).join(&dir_name);
    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir).map_err(|e| e.to_string())?;
        Ok(format!("Deleted: {}", cache_dir.display()))
    } else {
        Err(format!("Not found in cache: {}", cache_dir.display()))
    }
}

#[tauri::command]
async fn start_ollama(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    // Check if Ollama is already running externally
    if let Ok(r) = reqwest::get("http://localhost:11434/api/tags").await {
        if r.status().is_success() {
            let _ = app_handle.emit("ollama-ready", true);
            return Ok("Ollama already running".to_string());
        }
    }

    let mut ollama_lock = state.ollama_process.lock().await;
    // Kill any previously managed process
    if let Some(mut child) = ollama_lock.take() {
        let _ = child.kill().await;
    }

    // Find ollama binary
    #[cfg(windows)]
    let ollama_bin = {
        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidate = std::path::PathBuf::from(&local_app_data)
            .join("Programs")
            .join("Ollama")
            .join("ollama.exe");
        if candidate.exists() {
            candidate.to_string_lossy().to_string()
        } else {
            "ollama".to_string()
        }
    };
    #[cfg(not(windows))]
    let ollama_bin = "ollama".to_string();

    println!("[Ollama] Starting: {} serve", ollama_bin);

    let mut child = tokio::process::Command::new(&ollama_bin)
        .arg("serve")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start ollama: {}. Is Ollama installed?", e))?;

    if let Some(stdout) = child.stdout.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[Ollama] {}", line);
                let _ = app.emit("ollama-log", &line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[Ollama ERR] {}", line);
                let _ = app.emit("ollama-log", format!("[ERR] {}", line));
            }
        });
    }

    {
        let app = app_handle.clone();
        tokio::spawn(async move {
            for attempt in 1..=30u32 {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                match reqwest::get("http://localhost:11434/api/tags").await {
                    Ok(r) if r.status().is_success() => {
                        println!("[Ollama] Ready after {}s", attempt * 2);
                        let _ = app.emit("ollama-ready", true);
                        return;
                    }
                    _ => println!("[Ollama] Waiting... attempt {}/30", attempt),
                }
            }
            println!("[Ollama] Did not become ready within 60s");
            let _ = app.emit("ollama-failed", "Ollama did not respond within 60s");
        });
    }

    *ollama_lock = Some(child);
    Ok("Ollama starting".to_string())
}

#[tauri::command]
async fn stop_ollama(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut ollama_lock = state.ollama_process.lock().await;
    if let Some(mut child) = ollama_lock.take() {
        let _ = child.kill().await;
        println!("[Ollama] Stopped.");
    }
    Ok(())
}

#[tauri::command]
async fn stop_mlx_server(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut mlx_lock = state.mlx_process.lock().await;
    if let Some(mut child) = mlx_lock.take() {
        let _ = child.kill().await;
        println!("[MLX] Server stopped");
    }
    Ok(())
}

#[tauri::command]
async fn delete_files(paths: Vec<String>) -> Result<Vec<String>, String> {
    let mut results = Vec::new();
    for path in paths {
        match fs::remove_file(&path) {
            Ok(_) => results.push(format!("Deleted: {}", path)),
            Err(e) => results.push(format!("Error deleting {}: {}", path, e)),
        }
    }
    Ok(results)
}

#[tauri::command]
async fn get_app_data_dir(app_handle: tauri::AppHandle) -> Result<String, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

#[tauri::command]
async fn extract_pdf_native(path: String) -> Result<String, String> {
    app_log!("info", "Extracting PDF (Rust-native): {}", path);
    pdf_extract::extract_text(&path).map_err(|e| {
        app_log!("error", "PDF extraction failed: {} — {}", path, e);
        e.to_string()
    })
}

/// PDF metadata, merged from the legacy `/Info` dictionary and the
/// modern XMP packet. Where both are present, XMP wins (it's typically
/// richer and better-curated by publisher tooling); `/Info` fills any
/// gaps.
///
/// Fields are `Option` because real-world PDFs are inconsistent — most
/// academic PDFs have a Title and Author, fewer have a non-default
/// Producer-as-Author, and CreationDate quality varies wildly.
#[derive(serde::Serialize, Default, Clone, Debug)]
struct PdfMetadata {
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    keywords: Option<String>,
    /// Year extracted from CreationDate (preferred) or ModDate fallback.
    year: Option<i32>,
    /// Raw producer string — exposed so the frontend can decide whether
    /// to trust the metadata (academic-publisher producers are usually
    /// reliable; "Print to PDF" / generic OS dialogs less so).
    producer: Option<String>,
}

impl PdfMetadata {
    /// Merge `other` into `self`, taking `other`'s values where they're
    /// present. Used to layer XMP (preferred) on top of `/Info` (fallback).
    fn merge_in(&mut self, other: PdfMetadata) {
        if other.title.is_some() {
            self.title = other.title;
        }
        if other.author.is_some() {
            self.author = other.author;
        }
        if other.subject.is_some() {
            self.subject = other.subject;
        }
        if other.keywords.is_some() {
            self.keywords = other.keywords;
        }
        if other.year.is_some() {
            self.year = other.year;
        }
        if other.producer.is_some() {
            self.producer = other.producer;
        }
    }
}

#[tauri::command]
async fn extract_pdf_metadata(path: String) -> Result<PdfMetadata, String> {
    use lopdf::Document;
    app_log!("info", "Reading PDF metadata: {}", path);

    let doc = Document::load(&path).map_err(|e| {
        app_log!("error", "lopdf load failed for {}: {}", path, e);
        format!("lopdf load failed: {e}")
    })?;

    // Start with the legacy Info dict and layer XMP on top — XMP fields,
    // when present, are typically better-curated by publisher tooling
    // (Springer / Elsevier / IEEE all write good XMP). Info fills gaps.
    let mut meta = read_info_dict(&doc);
    if let Some(xmp) = read_xmp_packet(&doc) {
        meta.merge_in(xmp);
    }
    Ok(meta)
}

fn read_info_dict(doc: &lopdf::Document) -> PdfMetadata {
    use lopdf::Object;
    let info_id = match doc.trailer.get(b"Info") {
        Ok(Object::Reference(r)) => *r,
        _ => return PdfMetadata::default(),
    };
    let dict = match doc.get_object(info_id) {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => return PdfMetadata::default(),
    };

    let read_str = |key: &[u8]| -> Option<String> {
        dict.get(key).ok().and_then(|o| {
            o.as_str()
                .ok()
                .and_then(decode_pdf_string)
                .filter(|s| !s.trim().is_empty())
        })
    };

    let creation = read_str(b"CreationDate");
    let mod_date = read_str(b"ModDate");
    let year = creation
        .as_deref()
        .and_then(parse_pdf_date_year)
        .or_else(|| mod_date.as_deref().and_then(parse_pdf_date_year));

    PdfMetadata {
        title: read_str(b"Title"),
        author: read_str(b"Author"),
        subject: read_str(b"Subject"),
        keywords: read_str(b"Keywords"),
        year,
        producer: read_str(b"Producer"),
    }
}

/// Locate the catalog's `/Metadata` stream and parse its XMP payload.
/// Returns `None` when the PDF has no XMP packet, when decompression
/// fails, or when the XML doesn't expose any of the dc:/xmp: fields
/// we know about. Failures are non-fatal — a missing XMP just means
/// the caller falls back to the /Info dict.
fn read_xmp_packet(doc: &lopdf::Document) -> Option<PdfMetadata> {
    use lopdf::Object;
    // Catalog: trailer/Root → dict → /Metadata stream.
    let root_id = match doc.trailer.get(b"Root").ok()? {
        Object::Reference(r) => *r,
        _ => return None,
    };
    let catalog = match doc.get_object(root_id).ok()? {
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let meta_id = match catalog.get(b"Metadata").ok()? {
        Object::Reference(r) => *r,
        _ => return None,
    };
    let stream = match doc.get_object(meta_id).ok()? {
        Object::Stream(s) => s,
        _ => return None,
    };
    let bytes = stream
        .decompressed_content()
        .ok()
        .unwrap_or_else(|| stream.content.clone());
    if bytes.is_empty() {
        return None;
    }
    parse_xmp_xml(&bytes)
}

/// Minimal XMP/RDF walker. We only care about a handful of fields and
/// the document layout is well-constrained, so a state-machine over
/// `quick-xml` events stays much smaller than pulling in a full RDF
/// parser. Handles the typical wrapping:
///
/// ```xml
/// <dc:title><rdf:Alt><rdf:li xml:lang="x-default">…</rdf:li></rdf:Alt></dc:title>
/// <dc:creator><rdf:Seq><rdf:li>…</rdf:li><rdf:li>…</rdf:li></rdf:Seq></dc:creator>
/// <xmp:CreateDate>2023-03-15T10:30:00Z</xmp:CreateDate>
/// ```
fn parse_xmp_xml(xml: &[u8]) -> Option<PdfMetadata> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Field {
        Title,
        Creator,
        Subject,
        Description,
        Date,
    }

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut stack: Vec<Field> = Vec::new();
    let mut out = PdfMetadata::default();
    let mut creators: Vec<String> = Vec::new();
    let mut subjects: Vec<String> = Vec::new();

    let field_for = |prefix: Option<&[u8]>, local: &[u8]| -> Option<Field> {
        match (prefix, local) {
            (Some(b"dc"), b"title") => Some(Field::Title),
            (Some(b"dc"), b"creator") => Some(Field::Creator),
            (Some(b"dc"), b"subject") => Some(Field::Subject),
            (Some(b"dc"), b"description") => Some(Field::Description),
            (Some(b"xmp"), b"CreateDate")
            | (Some(b"xmp"), b"ModifyDate")
            | (Some(b"xmp"), b"MetadataDate") => Some(Field::Date),
            _ => None,
        }
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let prefix = e.name().prefix().map(|p| p.as_ref().to_vec());
                let local = e.local_name().as_ref().to_vec();
                if let Some(f) = field_for(prefix.as_deref(), &local) {
                    stack.push(f);
                }
            }
            Ok(Event::End(e)) => {
                let prefix = e.name().prefix().map(|p| p.as_ref().to_vec());
                let local = e.local_name().as_ref().to_vec();
                if field_for(prefix.as_deref(), &local).is_some() {
                    stack.pop();
                }
            }
            Ok(Event::Text(e)) => {
                // quick-xml 0.38: xml_content() decodes the bytes and
                // unescapes XML entities (&amp; &lt; etc.) in one step.
                let Ok(decoded) = e.xml_content() else {
                    continue;
                };
                let trimmed = decoded.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Some(field) = stack.last() else {
                    continue;
                };
                match field {
                    Field::Title => {
                        if out.title.is_none() {
                            out.title = Some(trimmed.to_owned());
                        }
                    }
                    Field::Creator => creators.push(trimmed.to_owned()),
                    Field::Subject => subjects.push(trimmed.to_owned()),
                    Field::Description => {
                        if out.subject.is_none() {
                            out.subject = Some(trimmed.to_owned());
                        }
                    }
                    Field::Date => {
                        if out.year.is_none() {
                            // ISO-8601 dates lead with YYYY — same as the
                            // PDF /Info dict path. Reuse the same parser.
                            if let Some(y) = parse_pdf_date_year(trimmed) {
                                out.year = Some(y);
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }

    if !creators.is_empty() {
        out.author = Some(creators.join(" and "));
    }
    if !subjects.is_empty() {
        out.keywords = Some(subjects.join(", "));
    }

    if out.title.is_none()
        && out.author.is_none()
        && out.subject.is_none()
        && out.keywords.is_none()
        && out.year.is_none()
    {
        return None;
    }
    Some(out)
}

/// Decode a raw PDF string. Handles three real-world cases:
/// - UTF-16BE with BOM (FE FF …): the most common modern producer choice.
/// - PDFDocEncoding (legacy 8-bit superset of WinANSI): fallback when no BOM.
/// - Plain UTF-8: some recent tools write this directly.
fn decode_pdf_string(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    // UTF-16BE BOM
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let pairs: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&pairs).ok();
    }
    // UTF-8 — accept if the bytes are valid.
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Some(s.to_owned());
    }
    // PDFDocEncoding fallback: lossy ASCII for now (covers Title/Author for
    // most English/European-language PDFs). A full PDFDocEncoding table
    // would handle the eight characters that diverge from Latin-1; not
    // worth the table for v1.
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Parse the year from a PDF date string. PDF dates look like:
///   D:YYYYMMDDHHmmSSOHH'mm'
/// where everything after YYYY is optional. We just need the year.
fn parse_pdf_date_year(s: &str) -> Option<i32> {
    let s = s.trim_start_matches("D:").trim();
    if s.len() < 4 {
        return None;
    }
    s[..4].parse::<i32>().ok().filter(|&y| (1000..=9999).contains(&y))
}

#[cfg(test)]
mod pdf_metadata_tests {
    use super::*;

    #[test]
    fn pdf_date_year_modern_format() {
        assert_eq!(parse_pdf_date_year("D:20240315133045+02'00'"), Some(2024));
        assert_eq!(parse_pdf_date_year("D:1999"), Some(1999));
        assert_eq!(parse_pdf_date_year("D:2025Z"), Some(2025));
    }

    #[test]
    fn pdf_date_year_without_d_prefix() {
        // Some producers emit a date without the "D:" sentinel.
        assert_eq!(parse_pdf_date_year("20240315"), Some(2024));
    }

    #[test]
    fn pdf_date_year_rejects_garbage() {
        assert_eq!(parse_pdf_date_year(""), None);
        assert_eq!(parse_pdf_date_year("D:"), None);
        assert_eq!(parse_pdf_date_year("not-a-date"), None);
        // Out-of-range year keeps None — guards against producer bugs that
        // emit "0000…" or unix-epoch ms instead of a year.
        assert_eq!(parse_pdf_date_year("D:0000"), None);
    }

    #[test]
    fn pdf_string_decode_utf16be_bom() {
        // "Hello" in UTF-16BE with BOM
        let bytes = b"\xFE\xFF\x00H\x00e\x00l\x00l\x00o";
        assert_eq!(decode_pdf_string(bytes), Some("Hello".to_string()));
    }

    #[test]
    fn pdf_string_decode_utf8() {
        assert_eq!(
            decode_pdf_string("Müller & Co.".as_bytes()),
            Some("Müller & Co.".to_string())
        );
    }

    #[test]
    fn pdf_string_decode_empty_returns_none() {
        assert_eq!(decode_pdf_string(b""), None);
    }

    #[test]
    fn xmp_parses_dc_alt_title_and_seq_creator() {
        let xml = br#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"
                     xmlns:xmp="http://ns.adobe.com/xap/1.0/">
      <dc:title>
        <rdf:Alt><rdf:li xml:lang="x-default">A Theory of Everything</rdf:li></rdf:Alt>
      </dc:title>
      <dc:creator>
        <rdf:Seq>
          <rdf:li>Smith, John</rdf:li>
          <rdf:li>Doe, Jane</rdf:li>
        </rdf:Seq>
      </dc:creator>
      <xmp:CreateDate>2023-03-15T10:30:00Z</xmp:CreateDate>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>"#;
        let m = parse_xmp_xml(xml).expect("XMP must parse");
        assert_eq!(m.title.as_deref(), Some("A Theory of Everything"));
        assert_eq!(m.author.as_deref(), Some("Smith, John and Doe, Jane"));
        assert_eq!(m.year, Some(2023));
    }

    #[test]
    fn xmp_parses_subject_and_keywords() {
        let xml = br#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:description>
        <rdf:Alt><rdf:li xml:lang="x-default">An abstract about cats and physics.</rdf:li></rdf:Alt>
      </dc:description>
      <dc:subject>
        <rdf:Bag>
          <rdf:li>cats</rdf:li>
          <rdf:li>physics</rdf:li>
        </rdf:Bag>
      </dc:subject>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>"#;
        let m = parse_xmp_xml(xml).expect("XMP must parse");
        assert_eq!(
            m.subject.as_deref(),
            Some("An abstract about cats and physics.")
        );
        assert_eq!(m.keywords.as_deref(), Some("cats, physics"));
    }

    #[test]
    fn xmp_returns_none_when_no_known_fields() {
        // Only contains pdf:Producer which we don't read from XMP — Info
        // dict covers Producer. Should return None to signal "nothing
        // to merge".
        let xml = br#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
      <pdf:Producer>Acrobat Distiller 11</pdf:Producer>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>"#;
        assert!(parse_xmp_xml(xml).is_none());
    }

    #[test]
    fn xmp_parser_resilient_to_truncated_input() {
        // Real-world XMP packets sometimes get cut off mid-stream. We
        // should still return whatever we managed to parse rather than
        // panic.
        let xml = br#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:title>
        <rdf:Alt><rdf:li xml:lang="x-default">Stub</rdf:li></rdf:Alt>
      </dc:title>"#;
        // Truncated: missing closing tags. quick-xml returns Err on EOF
        // mid-element; parse_xmp_xml maps that to None (no partial
        // metadata, since we can't trust what we collected up to a parse
        // failure).
        let _ = parse_xmp_xml(xml);
    }

    #[test]
    fn merge_in_xmp_wins_when_present() {
        let mut info = PdfMetadata {
            title: Some("Info Title".into()),
            author: Some("Info Author".into()),
            year: Some(2020),
            ..Default::default()
        };
        let xmp = PdfMetadata {
            title: Some("XMP Title".into()),
            year: None, // XMP didn't carry a date — Info should win
            ..Default::default()
        };
        info.merge_in(xmp);
        assert_eq!(info.title.as_deref(), Some("XMP Title"));
        // Author wasn't touched by XMP → Info value persists
        assert_eq!(info.author.as_deref(), Some("Info Author"));
        // Year wasn't touched by XMP → Info value persists
        assert_eq!(info.year, Some(2020));
    }
}

#[tauri::command]
fn scan_folder(folder_path: String, extensions: Vec<String>) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    let path = Path::new(&folder_path);
    if path.is_file() {
        if let Some(ext) = path.extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if extensions.iter().any(|e| e == &ext_lower) {
                let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                entries.push(FileEntry {
                    path: folder_path.clone(),
                    size,
                });
            }
        }
    } else if path.is_dir() {
        scan_dir_recursive(path, &extensions, &mut entries).map_err(|e| e.to_string())?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
    } else {
        return Err(format!("Path does not exist: {}", folder_path));
    }
    println!(
        "[Rust] scan_folder: found {} files in/at {}",
        entries.len(),
        folder_path
    );
    Ok(entries)
}

fn scan_dir_recursive(
    dir: &Path,
    extensions: &[String],
    entries: &mut Vec<FileEntry>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if path.is_dir() {
            scan_dir_recursive(&path, extensions, entries)?;
        } else if let Some(ext) = path.extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if extensions.iter().any(|e| e == &ext_lower) {
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                entries.push(FileEntry {
                    path: path.to_string_lossy().into_owned(),
                    size,
                });
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchExecutionPayload {
    items: Vec<BatchExecutionItem>,
    save_txt: bool,
    mode: String, // "move", "copy", "script_move", "script_copy"
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchExecutionItem {
    id: String,
    original_path: String,
    target_path: String,
    extracted_text: Option<String>,
    /// LanceDB doc_id — set when the document was indexed.  If present and the
    /// move succeeds, the index location is updated automatically.
    doc_id: Option<String>,
    /// Pre-built `crisp+local://...` URI for the destination path.  The
    /// frontend computes this using the same user/machine UUIDs stored in
    /// app settings.
    new_location_uri: Option<String>,
}

#[derive(serde::Serialize)]
struct BatchExecutionResult {
    success: bool,
    error: Option<String>,
}

#[tauri::command]
async fn execute_batch(
    state: tauri::State<'_, AppState>,
    payload: BatchExecutionPayload,
) -> Result<std::collections::HashMap<String, BatchExecutionResult>, String> {
    app_log!("info", "execute_batch: {} items, mode={}", payload.items.len(), payload.mode);
    let mut results = std::collections::HashMap::new();
    let is_script_mode = payload.mode.starts_with("script_");
    let mut script_content = String::new();

    // Windows detection for script extension
    let is_windows = std::env::consts::OS == "windows";
    let script_name = if is_windows {
        "sort_files.bat"
    } else {
        "sort_files.sh"
    };

    if is_script_mode {
        if is_windows {
            script_content.push_str("@echo off\n");
        } else {
            script_content.push_str("#!/bin/bash\n");
        }
    }

    for item in &payload.items {
        let src = Path::new(&item.original_path);
        let dest = Path::new(&item.target_path);

        if is_script_mode {
            // Script generation mode
            let cmd = if payload.mode == "script_move" {
                if is_windows {
                    format!("move \"{}\" \"{}\"\n", item.original_path, item.target_path)
                } else {
                    format!("mv \"{}\" \"{}\"\n", item.original_path, item.target_path)
                }
            } else if is_windows {
                format!("copy \"{}\" \"{}\"\n", item.original_path, item.target_path)
            } else {
                format!("cp \"{}\" \"{}\"\n", item.original_path, item.target_path)
            };
            script_content.push_str(&cmd);
            results.insert(
                item.id.clone(),
                BatchExecutionResult {
                    success: true,
                    error: None,
                },
            );
            continue;
        }

        // Direct execution mode
        if !src.exists() {
            results.insert(
                item.id.clone(),
                BatchExecutionResult {
                    success: false,
                    error: Some("SOURCE_NOT_FOUND".to_string()),
                },
            );
            continue;
        }

        if let Some(parent) = dest.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                results.insert(
                    item.id.clone(),
                    BatchExecutionResult {
                        success: false,
                        error: Some(format!("NOT_WRITABLE: {}", e)),
                    },
                );
                continue;
            }
        }

        // 1. Save TXT if requested
        if payload.save_txt {
            if let Some(text) = &item.extracted_text {
                let txt_path = dest.with_extension("txt");
                if let Err(e) = fs::write(txt_path, text) {
                    println!("Warning: Failed to save .txt for {}: {}", item.id, e);
                }
            }
        }

        // 2. Perform file operation — with locked-file copy fallback for move
        let exec_result = match payload.mode.as_str() {
            "copy" => match fs::copy(src, dest) {
                Ok(_) => BatchExecutionResult {
                    success: true,
                    error: None,
                },
                Err(e) => BatchExecutionResult {
                    success: false,
                    error: Some(e.to_string()),
                },
            },
            _ => match fs::rename(src, dest) {
                Ok(()) => BatchExecutionResult {
                    success: true,
                    error: None,
                },
                Err(ref e) if e.raw_os_error() == Some(32) => {
                    // File locked by another process (os error 32) — try copy as fallback
                    match fs::copy(src, dest) {
                        Ok(_) => BatchExecutionResult {
                            success: true,
                            error: Some("COPY_FALLBACK".to_string()),
                        },
                        Err(_) => BatchExecutionResult {
                            success: false,
                            error: Some("LOCKED".to_string()),
                        },
                    }
                }
                Err(e) => BatchExecutionResult {
                    success: false,
                    error: Some(e.to_string()),
                },
            },
        };
        // P11: if the move succeeded and the document was indexed, update the location URI.
        if exec_result.success {
            if let (Some(doc_id), Some(new_uri)) = (&item.doc_id, &item.new_location_uri) {
                let lock = state.index.lock().await;
                if lock.config.enabled {
                    if let Some(backend) = lock.backend.clone() {
                        drop(lock);
                        if let Err(e) = backend.update_location(doc_id, new_uri).await {
                            println!("[index] update_location failed for {}: {}", doc_id, e);
                        }
                    }
                }
            }
        }
        results.insert(item.id.clone(), exec_result);
    }

    if is_script_mode && !payload.items.is_empty() {
        // Save the generated script to the first item's parent directory or a common root if possible.
        // For simplicity, we'll try to put it in the first item's destination parent.
        if let Some(first_item) = payload.items.first() {
            let dest = Path::new(&first_item.target_path);
            if let Some(parent) = dest.parent() {
                let script_path = parent.join(script_name);
                if let Err(e) = fs::write(&script_path, script_content) {
                    return Err(format!("Failed to save script to {:?}: {}", script_path, e));
                }
            }
        }
    }

    Ok(results)
}

#[tauri::command]
async fn download_file(
    window: tauri::Window,
    id: String,
    url: String,
    path: String,
) -> Result<(), String> {
    let response = reqwest::get(url).await.map_err(|e| e.to_string())?;
    let total_size = response
        .content_length()
        .ok_or("Failed to get content length")?;

    let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        let _ = window.emit(
            "download-progress",
            DownloadProgress {
                id: id.clone(),
                received: downloaded,
                total: total_size,
            },
        );
    }
    Ok(())
}

#[tauri::command]
async fn run_mistralrs_query(
    state: tauri::State<'_, AppState>,
    model_path: String,
    prompt: String,
    max_tokens: Option<usize>,
    no_thinking: Option<bool>,
) -> Result<String, String> {
    let mut model_lock = state.model.lock().await;
    let mut current_path_lock = state.current_model_path.lock().await;

    // Check if we need to load or swap the model
    let needs_load = match &*current_path_lock {
        Some(path) if path == &model_path => model_lock.is_none(),
        _ => true,
    };

    if needs_load {
        app_log!("info", "Loading LLM model: {}", model_path);

        let device = best_device(false).map_err(|e| e.to_string())?;
        app_log!("info", "Target hardware device: {:?}", device);

        if model_path.starts_with("\\\\") {
            println!(
                "[mistral.rs] WARNING: Model path appears to be a UNC network share ('{}').",
                model_path
            );
            println!("[mistral.rs] This will SEVERELY degrade performance even if synced. Please use a local drive (e.g. C:\\).");
        }

        let model = if model_path.ends_with(".gguf") && Path::new(&model_path).exists() {
            // Local GGUF file
            let path = Path::new(&model_path);
            let parent = path
                .parent()
                .ok_or("Invalid model path")?
                .to_str()
                .ok_or("Non-UTF8 path")?
                .to_string();
            let filename = path
                .file_name()
                .ok_or("Invalid model filename")?
                .to_str()
                .ok_or("Non-UTF8 filename")?
                .to_string();

            println!(
                "[mistral.rs] Loading local GGUF: ID='{}', File='{}'",
                parent, filename
            );
            GgufModelBuilder::new(parent, vec![filename])
                .with_device(device)
                .with_logging()
                .with_throughput_logging()
                .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                .map_err(|e| e.to_string())?
                .build()
                .await
        } else {
            // Assume it's an HF Repo ID or URL that mistralrs can handle
            let parts: Vec<&str> = model_path.split('/').collect();
            if parts.len() >= 3 && model_path.contains(".gguf") {
                let filename = parts.last().unwrap().to_string();
                let repo_id = parts[..parts.len() - 1].join("/");
                println!(
                    "[mistral.rs] Loading remote HF GGUF: Repo='{}', File='{}'",
                    repo_id, filename
                );
                GgufModelBuilder::new(repo_id, vec![filename])
                    .with_device(device)
                    .with_logging()
                    .with_throughput_logging()
                    .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                    .map_err(|e| e.to_string())?
                    .build()
                    .await
            } else {
                println!(
                    "[mistral.rs] Loading as TextModel (Repo ID): {}",
                    model_path
                );
                mistralrs::TextModelBuilder::new(model_path.clone())
                    .with_device(device)
                    .with_logging()
                    .with_throughput_logging()
                    .with_isq(IsqType::Q4K) // Ensure remote models are quantized
                    .build()
                    .await
            }
        }
        .map_err(|e| {
            app_log!("error", "LLM load failed: {}", e);
            e.to_string()
        })?;

        *model_lock = Some(Arc::new(model));
        *current_path_lock = Some(model_path.clone());
        app_log!("info", "LLM model loaded successfully");
    }

    // We can unwrap here because we ensured it's Some above
    let model = model_lock.as_ref().unwrap();

    let max_len = max_tokens.unwrap_or(512);
    // User requested thinking=false by default for performance
    let thinking = if let Some(nt) = no_thinking {
        !nt
    } else {
        false
    };

    let request = RequestBuilder::from(
        TextMessages::new().add_message(TextMessageRole::User, prompt.clone()),
    )
    .set_sampler_max_len(max_len)
    .enable_thinking(thinking);

    println!(
        "[mistral.rs] Sending chat request (prompt len={}, max_tokens={}, thinking={})...",
        prompt.len(),
        max_len,
        thinking
    );
    let start_time = std::time::Instant::now();
    let response = model.send_chat_request(request).await.map_err(|e| {
        app_log!("error", "LLM query failed: {}", e);
        e.to_string()
    })?;
    let duration = start_time.elapsed();

    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .cloned()
        .unwrap_or_default();
    let total_tokens = response.usage.completion_tokens;
    let tps = if duration.as_secs_f32() > 0.0 {
        total_tokens as f32 / duration.as_secs_f32()
    } else {
        0.0
    };

    println!("[mistral.rs] Query complete in {:?}. Response length: {}. Usage: P={}, C={}. Speed: {:.2} t/s", 
        duration,
        content.len(),
        response.usage.prompt_tokens,
        total_tokens,
        tps
    );

    Ok(content)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    initialize_logging();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // Store global handle so app_log!() works from any thread.
            if let Ok(mut h) = APP_HANDLE.lock() {
                *h = Some(app.handle().clone());
            }
            app_log!("info", "CrispSorter v{} starting", env!("CARGO_PKG_VERSION"));

            // Initialise job queue + store data_dir now that it is known.
            if let Ok(data_dir) = app.path().app_data_dir() {
                let state: tauri::State<'_, AppState> = app.state();
                // Store data_dir for drive_* commands and similar.
                // Use try_lock so we don't block the sync setup hook.
                if let Ok(mut dd) = state.data_dir.try_lock() {
                    *dd = Some(data_dir.clone());
                }
                match jobs::JobQueue::open_or_create(&data_dir) {
                    Ok(q) => {
                        if let Ok(mut guard) = state.job_queue.lock() {
                            *guard = Some(q);
                        }
                    }
                    Err(e) => {
                        app_log!("error", "Failed to open job queue: {e}");
                    }
                }

                // P13.5 follow-up — load persisted IndexConfig from
                // <data_dir>/index_config.json so `index_get_config`
                // returns the user's saved settings on first call,
                // not the in-memory defaults from IndexState::disabled().
                // Falls back to default silently when the file is missing
                // (fresh install) — see config_persist::load doc.
                //
                // tokio::sync::Mutex::blocking_lock is fine in the
                // synchronous setup hook — the runtime isn't driving
                // any tasks against state.index at this moment.
                // try_lock's `MutexGuard<'_, _>` was hitting an
                // E0597 because the temporary held a borrow longer
                // than `state` lived; blocking_lock returns a guard
                // with the same lifetime but the borrow checker
                // accepts it because it's not a Result<…> temporary
                // ladder.
                let persisted = index::config_persist::load(&data_dir);
                {
                    let mut idx_lock = state.index.blocking_lock();
                    idx_lock.config = persisted;
                }
                // P13.6 Step 5 — apply audio_asr_backend from the
                // persisted config to the shared extractors::audio
                // handle.  Set-once via OnceLock; subsequent
                // index_set_config UI submissions won't retroactively
                // change the loaded handle (restart required).  Same
                // constraint the existing embedder has.
                {
                    let idx_lock = state.index.blocking_lock();
                    extractors::audio::set_audio_asr_backend_override(
                        &idx_lock.config.audio_asr_backend,
                    );
                }
                app_log!("info", "Loaded persisted IndexConfig from {}", data_dir.display());

                // P13.7 Stage J — background drain task for the
                // cb_manifest_push outbox.  Runs every 30s while
                // the app is open AND the user has opted in via
                // `cloud_backup_push_manifests_enabled`.  Drain is
                // a no-op when the outbox is empty or the URL/
                // token is missing — cheap to leave running.
                //
                // Snapshot the handle so the task can read state
                // without owning it; tokio::spawn lifts ownership
                // into the runtime.
                let drain_handle = app.handle().clone();
                tokio::spawn(async move {
                    use std::time::Duration;
                    // 30s feels right: fresh enough that a user
                    // who pushes a manifest then opens cloud-backup
                    // search sees the row within seconds; not so
                    // tight that an idle app burns CPU.  Operators
                    // who want immediate flush can hit the
                    // `crispsorter sync cloud-backup drain` CLI
                    // or the Settings → "Sync now" button.
                    let mut ticker = tokio::time::interval(Duration::from_secs(30));
                    ticker.set_missed_tick_behavior(
                        tokio::time::MissedTickBehavior::Skip,
                    );
                    loop {
                        ticker.tick().await;
                        if let Some(state) = drain_handle.try_state::<AppState>() {
                            let (enabled, url_opt) = {
                                let idx = state.index.lock().await;
                                (
                                    idx.config.cloud_backup_push_manifests_enabled,
                                    idx.config.cloud_backup_url.clone(),
                                )
                            };
                            if !enabled { continue; }
                            let Some(url) = url_opt.filter(|s| !s.is_empty()) else { continue };
                            let Ok(Some(token)) = crate::sync::secret::get_token_for_url(&url) else { continue };
                            let Ok(cli) = crate::sync::cloud_backup::CloudBackupClient::new(&url, &token) else { continue };
                            let data_dir = state.data_dir.lock().await.clone();
                            let Some(data_dir) = data_dir else { continue };
                            let Ok(mgr) = crate::sync::SyncManager::open(&data_dir) else { continue };
                            // Drain up to 200 entries per tick; if more
                            // accumulate, the next tick picks them up.
                            match mgr.drain_cb_outbox(&cli, 200).await {
                                Ok((pushed, failed)) if pushed > 0 || failed > 0 => {
                                    app_log!(
                                        "info",
                                        "cb-api outbox drain: pushed={pushed} failed={failed}"
                                    );
                                }
                                _ => {}  // nothing to drain or transient error
                            }
                            // Stage U — also drain cb_file_upload entries (thin-client mode).
                            let thin_enabled = {
                                state.index.lock().await.config.local_extraction_enabled == false
                            };
                            if thin_enabled {
                                match mgr.drain_cb_file_uploads(&cli, 8).await {
                                    Ok((up, fail)) if up > 0 || fail > 0 => {
                                        app_log!(
                                            "info",
                                            "cb-api file-upload drain: uploaded={up} failed={fail}"
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                });
            }

            // P13.7 Stage P — hourly LRU purge: when the user has set
            // `local_max_size_bytes`, trim the lance dir down to that cap.
            // Runs every 3600 s; cheap no-op when no cap is configured or
            // the index is already within bounds.
            {
                let purge_handle = app.handle().clone();
                tokio::spawn(async move {
                    use std::time::Duration;
                    let mut ticker = tokio::time::interval(Duration::from_secs(3600));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        ticker.tick().await;
                        if let Some(state) = purge_handle.try_state::<AppState>() {
                            let (max_bytes_opt, data_dir_opt) = {
                                let idx = state.index.lock().await;
                                (
                                    idx.config.local_max_size_bytes,
                                    state.data_dir.try_lock().ok().and_then(|g| g.clone()),
                                )
                            };
                            let (Some(max_bytes), Some(data_dir)) = (max_bytes_opt, data_dir_opt) else { continue };
                            let lance_dir = data_dir.join("lance");
                            if crate::index::local_index::dir_size_bytes(&lance_dir) <= max_bytes { continue; }
                            match crate::index::LocalIndex::open_or_create(&data_dir, 1024).await {
                                Ok(local) => {
                                    match local.purge_to_size(&lance_dir, max_bytes).await {
                                        Ok((s, d, r)) if s > 0 || d > 0 => {
                                            app_log!("info",
                                                "index purge: stripped={s} deleted={d} reclaimed={r}B");
                                        }
                                        Err(e) => {
                                            app_log!("error", "index purge failed: {e}");
                                        }
                                        _ => {}
                                    }
                                }
                                Err(e) => app_log!("error", "index purge open failed: {e}"),
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .manage(AppState {
            model: Mutex::new(None),
            current_model_path: Mutex::new(None),
            sidecar_process: Mutex::new(None),
            mlx_process: Mutex::new(None),
            ollama_process: Mutex::new(None),
            index: Mutex::new(index::IndexState::disabled()),
            asr: Mutex::new(None),
            tts_process: Mutex::new(None),
            watcher: Mutex::new(watcher::WatcherState::new()),
            bg_ingest: Arc::new(Mutex::new(bg_ingest::BackgroundIngest::new())),
            foreground_active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            job_queue: Arc::new(std::sync::Mutex::new(None)),
            data_dir: tokio::sync::Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_logs,
            frontend_log,
            execute_batch,
            scan_folder,
            download_file,
            run_mistralrs_query,
            start_llamacpp_sidecar,
            stop_llamacpp_sidecar,
            start_mlx_server,
            stop_mlx_server,
            start_ollama,
            stop_ollama,
            get_mlx_cache_dir,
            check_mlx_models_cached,
            delete_mlx_model,
            delete_files,
            extract_pdf_native,
            extract_pdf_metadata,
            get_app_data_dir,
            index::tauri_commands::index_search,
            index::translate_commands::translate_text,
            index::tauri_commands::index_ingest_document,
            index::tauri_commands::index_ingest_batch,
            index::tauri_commands::index_ingest_l1,
            index::tauri_commands::index_promote_l2,
            index::tauri_commands::index_import_caf,
            index::tauri_commands::index_export_caf,
            index::tauri_commands::index_export_cidx,
            index::tauri_commands::index_open_cidx,
            sync::tauri_commands::sync_status,
            sync::tauri_commands::sync_push,
            sync::tauri_commands::sync_pull,
            sync::tauri_commands::sync_enqueue,
            sync::tauri_commands::sync_clear_failed,
            // P13.7 Step 5 — cloud-backup HTTP API target
            sync::tauri_commands::sync_cb_status,
            sync::tauri_commands::sync_cb_set_token,
            sync::tauri_commands::sync_cb_clear_token,
            sync::tauri_commands::sync_cb_manifest_push,
            sync::tauri_commands::sync_cb_manifest_pull,
            sync::tauri_commands::sync_cb_embeddings_push,
            sync::tauri_commands::sync_cb_search,
            sync::tauri_commands::sync_cb_upload_file,
            sync::tauri_commands::sync_cb_download_file,
            sync::tauri_commands::sync_cb_drain,
            sync::tauri_commands::sync_cb_embed_query,
            sync::tauri_commands::sync_cb_embed_models,
            sync::tauri_commands::sync_cb_v2_search,
            sync::tauri_commands::sync_cb_partition,
            sync::tauri_commands::sync_status_all,
            sync::tauri_commands::sync_cb_backup_shards,
            sync::tauri_commands::sync_cb_import_from_manifest_db,
            sync::tauri_commands::sync_federated_search,
            sync::tauri_commands::sync_cb_admin_mint,
            sync::tauri_commands::sync_cb_admin_revoke,
            sync::tauri_commands::sync_cb_admin_list_keys,
            sync::tauri_commands::sync_cb_extract_status,
            sync::tauri_commands::sync_skeleton_search,
            drives::tauri_commands::drive_list,
            drives::tauri_commands::drive_create,
            drives::tauri_commands::drive_update,
            drives::tauri_commands::drive_delete,
            drives::tauri_commands::drive_list_dir,
            drives::tauri_commands::drive_stat,
            images::tauri_commands::images_list,
            images::tauri_commands::images_default_extensions,
            images::tauri_commands::images_thumbnail,
            images::tauri_commands::images_exif,
            images::tauri_commands::images_duplicates,
            images::tauri_commands::images_near_duplicates,
            images::crisplens::tauri_commands::images_crisplens_settings_get,
            images::crisplens::tauri_commands::images_crisplens_settings_set,
            images::crisplens::tauri_commands::images_crisplens_session_status,
            images::crisplens::tauri_commands::images_crisplens_login,
            images::crisplens::tauri_commands::images_crisplens_logout,
            images::crisplens::tauri_commands::images_crisplens_status,
            images::crisplens::tauri_commands::images_crisplens_watchfolders,
            images::crisplens::tauri_commands::images_crisplens_people,
            images::crisplens::tauri_commands::images_crisplens_image_faces,
            images::crisplens::tauri_commands::images_crisplens_search,
            images::crisplens::tauri_commands::images_crisplens_image_by_hash,
            images::crisplens::tauri_commands::images_crisplens_image_by_local_path,
            images::crisplens::tauri_commands::images_crisplens_image_push,
            index::tauri_commands::index_ingest_cb_manifest,
            index::tauri_commands::index_promote_cb_archive,
            index::tauri_commands::index_lookup_cb_file,
            index::tauri_commands::index_ingest_drive_manifest,
            index::tauri_commands::index_promote_drive_archive,
            index::tauri_commands::index_mount_cidx,
            index::tauri_commands::index_unmount_cidx,
            index::tauri_commands::index_query_cidx_documents,
            index::tauri_commands::index_list_failed_extractions,
            index::tauri_commands::index_retry_all_failed,
            index::tauri_commands::index_ingest_path,
            index::tauri_commands::index_update_location,
            index::tauri_commands::index_update_location_by_path,
            index::tauri_commands::index_retry_extraction,
            index::tauri_commands::index_build_ivf_pq,
            index::tauri_commands::index_build_scalar_index,
            index::tauri_commands::index_list_mounted_volumes,
            index::tauri_commands::index_volume_id_for_path,
            index::tauri_commands::index_folder_children,
            index::tauri_commands::index_queue_depth,
            index::tauri_commands::index_get_config,
            index::tauri_commands::index_set_config,
            index::tauri_commands::index_init,
            index::tauri_commands::index_is_ready,
            index::tauri_commands::index_stats,
            index::tauri_commands::index_list_documents,
            index::tauri_commands::index_query_documents,
            index::tauri_commands::index_delete_document,
            index::tauri_commands::index_audio_promote_l3,
            index::tauri_commands::index_image_promote_l3,
            index::tauri_commands::index_capabilities,
            index::tauri_commands::index_model_download_mb,
            index::tauri_commands::embedder_registry_list,
            index::tauri_commands::embedder_download_registry_model,
            index::tauri_commands::index_benchmark_embedder,
            asr_transcribe,
            audio_extract_text,
            audio_metadata,
            tts_speak,
            tts_stop,
            watch_start,
            watch_stop_one,
            watch_stop_all,
            watch_list,
            volume_list_mounted,
            file_sha256,
            catalog_load_caf,
            catalog_save_caf,
            catalog_scan_dir,
            catalog_metadata,
            catalog_find_duplicates,
            catalog_generate_deletion_script,
            catalog_set_active,
            catalog_search,
            catalog_active_list,
            catalog_export_sorted,
            bg_ingest_start,
            bg_ingest_status,
            bg_ingest_pause,
            bg_ingest_resume,
            bg_ingest_cancel,
            bg_ingest_clear,
            bg_ingest_set_ocr,
            jobs::tauri_commands::jobs_create,
            jobs::tauri_commands::jobs_list,
            jobs::tauri_commands::jobs_get,
            jobs::tauri_commands::jobs_set_status,
            jobs::tauri_commands::jobs_delete,
            jobs::tauri_commands::jobs_add_files,
            jobs::tauri_commands::jobs_claim_batch,
            jobs::tauri_commands::jobs_mark_done,
            jobs::tauri_commands::jobs_mark_error,
            jobs::tauri_commands::jobs_mark_skipped,
            jobs::tauri_commands::jobs_set_doc_id,
            jobs::tauri_commands::jobs_reclaim,
            jobs::tauri_commands::jobs_pending_count,
            jobs::tauri_commands::jobs_list_files,
            jobs::tauri_commands::jobs_remove_file,
            jobs::tauri_commands::jobs_remove_files_by_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
