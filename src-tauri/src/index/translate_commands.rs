//! On-demand text translation Tauri command (P13.5 Phase 8 consumer
//! side, on-demand surface).
//!
//! Wraps the upstream `crispasr::Session::translate_text` (Phase 5)
//! and `crispasr::text_detect_language` (Phase 7 upstream) into a
//! single Tauri command the search-results UI calls when the user
//! clicks "Translate to …" on a hit:
//!
//! ```ts
//! const { translated_text, source_lang, cached } = await invoke(
//!     'translate_text',
//!     {
//!         input: {
//!             text: chunk.snippet,         // or chunk.full_text if available
//!             source_lang: chunk.language, // optional — runs text-LID otherwise
//!             target_lang: 'en',
//!             mt_backend: 'm2m100',        // optional, default m2m100
//!             mt_model: null,              // optional explicit model path
//!             lid_model: null,             // optional — auto-resolves CLD3
//!                                          //   from the CrispASR registry when
//!                                          //   source_lang is also null
//!             max_tokens: 0,               // 0 = upstream default (200 for m2m100)
//!         },
//!     }
//! );
//! ```
//!
//! ## Caching
//!
//! Repeated clicks on the same chunk (or the same chunk content
//! reaching from a different doc) hit a SQLite cache in
//! `crisp_jobs.db` (always opened at startup; cheapest place to
//! host translation results alongside the schema-migration ledger).
//! Key shape: `(text_hash, source_lang, target_lang, backend)` so
//! the same text translated through two backends or to two targets
//! lives as separate rows.
//!
//! Cache uses `CREATE TABLE IF NOT EXISTS` (not the migration
//! framework) — adding a new table is idempotent; the framework is
//! for evolving existing schemas.
//!
//! ## Model paths
//!
//! Both the MT model and the LID model are passed per-call from the
//! frontend.  Persisting them in `IndexConfig` (so the user sets
//! them once in Settings) is a follow-up — the on-demand UI can
//! plumb its own per-app-install defaults today and we don't have
//! to commit to a settings-schema migration yet.

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::State;

use crate::AppState;

/// Input to [`translate_text`].  Matches the TypeScript shape on
/// the frontend side — every field is owned `Option<String>` so
/// the frontend doesn't have to construct enums.
#[derive(Debug, Clone, Deserialize)]
pub struct TranslateInput {
    /// Source text to translate.  Required.  Empty / whitespace-only
    /// errors before any model load.
    pub text: String,
    /// ISO 639-1 source language hint.  When `None`, the command
    /// runs text-LID via `lid_model` to detect it; that case
    /// requires `lid_model` to be set.
    #[serde(default)]
    pub source_lang: Option<String>,
    /// ISO 639-1 target language. Required.
    pub target_lang: String,
    /// MT backend name (`m2m100` / `m2m100-wmt21` / `madlad` /
    /// `gemma4-e2b`).  `None` defaults to `m2m100` (any-to-any 100
    /// langs, the broadest-coverage option).
    #[serde(default)]
    pub mt_backend: Option<String>,
    /// Explicit MT model file path.  `None` uses the upstream
    /// CrispASR registry auto-download path; the cache lands in
    /// `<data-dir>/models/`.
    #[serde(default)]
    pub mt_model: Option<String>,
    /// Text-LID model file path.  Required when `source_lang` is
    /// `None`; ignored otherwise.  CrispASR ships three options
    /// today: `lid-cld3` (440 KB, 109 langs ISO 639-1, Apache-2.0),
    /// `lid-glotlid` (250 MB, 2102 labels ISO 639-3+script,
    /// Apache-2.0), `lid-fasttext176` (63 MB, 176 langs,
    /// CC-BY-SA-3.0).
    #[serde(default)]
    pub lid_model: Option<String>,
    /// Decoder max-tokens cap.  `None` or `0` → upstream default
    /// (200 for m2m100).  Set higher for long chunks.
    #[serde(default)]
    pub max_tokens: Option<i32>,
}

/// Response shape returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct TranslateResponse {
    /// The translated text.
    pub translated_text: String,
    /// ISO 639-1 source language — what the caller passed, OR what
    /// LID detected.  The frontend can show this so the user knows
    /// which direction the translation ran in.
    pub source_lang: String,
    /// Echoed back so the frontend doesn't need to remember.
    pub target_lang: String,
    /// Backend that actually did the translation.  Useful when the
    /// frontend's per-pair selection logic (m2m100 vs wmt21 vs
    /// madlad) lives in TS and wants to confirm.
    pub backend: String,
    /// `true` when the translation came from the SQLite cache,
    /// `false` when the MT model was actually invoked.  Lets the UI
    /// distinguish "instant" from "had to load m2m100" for
    /// progress display.
    pub cached: bool,
}

/// Tauri command — translate `input.text` from `source_lang` (or
/// detected) to `target_lang` via the configured MT backend.
///
/// Errors when:
/// - `text` is empty / whitespace-only;
/// - `source_lang` is None AND `lid_model` is None;
/// - the LID model file doesn't exist (when LID is needed);
/// - the MT backend isn't MT-capable (e.g. user picked `whisper`);
/// - any of the language codes don't parse as ISO 639-1;
/// - the data dir isn't set in [`AppState`] (means startup didn't
///   finish — pure UI-race guard).
#[tauri::command]
pub async fn translate_text(
    state: State<'_, AppState>,
    input: TranslateInput,
) -> Result<TranslateResponse, String> {
    crate::ensure_intended_purpose(&state, "translate_text").await?;
    translate_text_impl(state, input).await.map_err(|e| format!("{e:#}"))
}

/// Inner implementation — kept anyhow-typed for the `?` ergonomics;
/// the outer `#[tauri::command]` wrapper converts to `String` at
/// the FFI boundary.
async fn translate_text_impl(
    state: State<'_, AppState>,
    input: TranslateInput,
) -> Result<TranslateResponse> {
    if input.text.trim().is_empty() {
        anyhow::bail!("text is empty / whitespace-only — nothing to translate");
    }
    let target_lang = input.target_lang.trim().to_ascii_lowercase();
    if target_lang.len() != 2 || !target_lang.chars().all(|c| c.is_ascii_alphabetic()) {
        anyhow::bail!(
            "target_lang must be a two-letter ISO 639-1 code, got {:?}",
            input.target_lang
        );
    }

    // ── 1. Resolve source language ───────────────────────────────────
    let source_lang = match input.source_lang.as_deref() {
        Some(s) if !s.trim().is_empty() => {
            let normalized = s.trim().to_ascii_lowercase();
            if normalized.len() != 2 || !normalized.chars().all(|c| c.is_ascii_alphabetic()) {
                anyhow::bail!(
                    "source_lang must be a two-letter ISO 639-1 code, got {s:?}"
                );
            }
            normalized
        }
        _ => {
            // No hint — must run LID.  Two paths:
            //   1. Caller supplied an explicit `lid_model` path.
            //      Use that file directly (matches the original
            //      contract of the command).
            //   2. No path — fall back to auto-resolving the CLD3
            //      preset via the CrispASR registry.  CLD3 is the
            //      smallest viable LID (440 KB) and emits ISO 639-1
            //      directly so the downstream normalise_to_iso_639_1
            //      call is a no-op.  Bigger / more-accurate presets
            //      (GlotLID, LID-176) are explicit opt-ins via
            //      `lid_model` pointing at the cached path.
            let model_path = match input.lid_model.as_deref() {
                Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
                _ => {
                    // Auto-resolve CLD3 via the registry.  Needs the
                    // app data dir for the cache; falls back to the
                    // same per-OS helper the audio extractor uses.
                    let cache_dir = mt_cache_dir(&state).await?;
                    crate::extractors::text_lid::resolve_lid_model(
                        crate::extractors::text_lid::LidPreset::Cld3,
                        &cache_dir,
                    )
                    .await
                    .context("auto-resolving CLD3 LID model")?
                }
            };
            let result = crate::extractors::text_lid::detect_language(
                &input.text,
                &model_path,
                2,
            )
            .with_context(|| format!("running text-LID with model {}", model_path.display()))?;
            // Normalise CLD3 / fastText label space to ISO 639-1.
            // When normalisation fails (long-tail language without
            // an ISO 639-1 assignment), error rather than silently
            // mis-route — the user can pass --source-lang explicitly.
            crate::extractors::text_lid::normalise_to_iso_639_1(&result.label)
                .ok_or_else(|| {
                    anyhow!(
                        "text-LID detected {:?} (confidence {:.2}) but no ISO 639-1 \
                         mapping is available — pass source_lang explicitly",
                        result.label,
                        result.confidence
                    )
                })?
        }
    };

    if source_lang == target_lang {
        // Identity translation — short-circuit and just echo the
        // input.  Saves a model load + a cache write for the common
        // case of someone clicking "translate to en" on an English
        // hit by mistake.
        return Ok(TranslateResponse {
            translated_text: input.text.clone(),
            source_lang,
            target_lang,
            backend: input
                .mt_backend
                .as_deref()
                .unwrap_or("m2m100")
                .to_string(),
            cached: false,
        });
    }

    let backend = input
        .mt_backend
        .as_deref()
        .unwrap_or("m2m100")
        .to_string();
    let max_tokens = input.max_tokens.unwrap_or(0);

    // ── 2. Cache lookup ──────────────────────────────────────────────
    let ledger = job_queue_conn(&state)?;
    ensure_cache_table(&ledger)?;
    let text_hash = hash_text(&input.text);
    if let Some(cached) =
        cache_lookup(&ledger, &text_hash, &source_lang, &target_lang, &backend)?
    {
        return Ok(TranslateResponse {
            translated_text: cached,
            source_lang,
            target_lang,
            backend,
            cached: true,
        });
    }

    // ── 3. Run the translation ───────────────────────────────────────
    let cache_dir = mt_cache_dir(&state).await?;
    let mt_config = match input.mt_model.as_deref() {
        Some(p) => crate::asr::AsrConfig::with_model_path(&backend, p),
        None => crate::asr::AsrConfig::new(&backend),
    };
    let handle = get_or_init_mt_handle(mt_config.clone(), cache_dir);
    let translated = handle
        .translate_text(
            input.text.clone(),
            source_lang.clone(),
            target_lang.clone(),
            max_tokens,
        )
        .await
        .with_context(|| {
            format!(
                "translate via {} ({} → {})",
                mt_config.display_name(),
                source_lang,
                target_lang
            )
        })?;

    // ── 4. Cache write (best-effort; failure here doesn't fail the
    //         command — the user gets their translation either way) ──
    if let Err(e) = cache_insert(
        &ledger,
        &text_hash,
        &source_lang,
        &target_lang,
        &backend,
        &translated,
    ) {
        eprintln!("[translate] cache write failed (non-fatal): {e:#}");
    }

    Ok(TranslateResponse {
        translated_text: translated,
        source_lang,
        target_lang,
        backend,
        cached: false,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────

/// SHA-256 of the input text, base16-lowercase.  Used as the cache
/// key first component.  We hash the whole text rather than truncate
/// so two chunks differing only in their tail still get separate
/// cache entries.
fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Reach into `AppState` for the shared `crisp_jobs.db` connection.
/// Errors if the job queue hasn't been initialised yet (startup
/// race — the queue is opened in the Tauri setup hook, before any
/// command can fire, so this should never happen in practice).
fn job_queue_conn(state: &State<'_, AppState>) -> Result<Arc<Mutex<Connection>>> {
    let guard = state
        .job_queue
        .lock()
        .map_err(|e| anyhow!("job queue mutex poisoned: {e}"))?;
    let queue = guard
        .as_ref()
        .ok_or_else(|| anyhow!("job queue not initialised — startup hasn't completed"))?;
    Ok(queue.conn_arc())
}

/// Resolve the MT model cache dir from `AppState.data_dir`, falling
/// back to the per-OS app-data path the audio extractor uses if
/// the state value isn't set yet.
async fn mt_cache_dir(state: &State<'_, AppState>) -> Result<PathBuf> {
    let dd = state.data_dir.lock().await;
    if let Some(p) = dd.as_ref() {
        let dir = p.join("models");
        std::fs::create_dir_all(&dir).ok();
        return Ok(dir);
    }
    // Fallback path — should be rare (only fires if a command runs
    // before the setup hook stored data_dir into state).
    let fallback = default_app_data_dir().join("models");
    std::fs::create_dir_all(&fallback).ok();
    Ok(fallback)
}

#[cfg(target_os = "macos")]
fn default_app_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join("Library/Application Support/com.crispstrobe.crispsorter"))
        .unwrap_or_else(|| PathBuf::from("/tmp/crispsorter"))
}
#[cfg(target_os = "windows")]
fn default_app_data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|a| a.join("com.crispstrobe.crispsorter"))
        .unwrap_or_else(|| PathBuf::from("C:\\Temp\\crispsorter"))
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn default_app_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".local/share"))
        })
        .map(|d| d.join("com.crispstrobe.crispsorter"))
        .unwrap_or_else(|| PathBuf::from("/tmp/crispsorter"))
}

/// Lazily construct the MT handle.  Same shape as the audio
/// extractor's `OnceLock<AsrHandle>` — first call constructs (cheap,
/// no model load yet); the actual session load is deferred to the
/// first `translate_text` call inside the handle's own Mutex.
///
/// Backend selection is fixed at the FIRST call's `AsrConfig`.
/// Subsequent calls with a different `mt_backend` value will still
/// route to whatever was loaded first — which is the right call for
/// the common case (one user picks one MT backend per session).
/// Per-call backend switching is a follow-up (would need a HashMap
/// of handles keyed by backend name).
fn get_or_init_mt_handle(
    config: crate::asr::AsrConfig,
    cache_dir: PathBuf,
) -> &'static crate::asr::AsrHandle {
    static HANDLE: OnceLock<crate::asr::AsrHandle> = OnceLock::new();
    HANDLE.get_or_init(|| crate::asr::AsrHandle::new(config, cache_dir))
}

const CACHE_TABLE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS translation_cache (
    text_hash       TEXT NOT NULL,
    source_lang     TEXT NOT NULL,
    target_lang     TEXT NOT NULL,
    backend         TEXT NOT NULL,
    translated_text TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (text_hash, source_lang, target_lang, backend)
);
"#;

fn ensure_cache_table(conn: &Arc<Mutex<Connection>>) -> Result<()> {
    let c = conn.lock().map_err(|e| anyhow!("conn mutex poisoned: {e}"))?;
    c.execute_batch(CACHE_TABLE_SCHEMA)
        .context("creating translation_cache table")?;
    Ok(())
}

fn cache_lookup(
    conn: &Arc<Mutex<Connection>>,
    text_hash: &str,
    source_lang: &str,
    target_lang: &str,
    backend: &str,
) -> Result<Option<String>> {
    let c = conn.lock().map_err(|e| anyhow!("conn mutex poisoned: {e}"))?;
    let mut stmt = c.prepare(
        "SELECT translated_text FROM translation_cache \
         WHERE text_hash = ? AND source_lang = ? AND target_lang = ? AND backend = ? \
         LIMIT 1",
    )
    .context("preparing translation_cache lookup")?;
    let mut rows = stmt
        .query(rusqlite::params![text_hash, source_lang, target_lang, backend])
        .context("executing translation_cache lookup")?;
    if let Some(row) = rows.next().context("reading translation_cache row")? {
        let t: String = row.get(0).context("decoding cache row")?;
        return Ok(Some(t));
    }
    Ok(None)
}

fn cache_insert(
    conn: &Arc<Mutex<Connection>>,
    text_hash: &str,
    source_lang: &str,
    target_lang: &str,
    backend: &str,
    translated: &str,
) -> Result<()> {
    let c = conn.lock().map_err(|e| anyhow!("conn mutex poisoned: {e}"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    c.execute(
        "INSERT OR REPLACE INTO translation_cache \
         (text_hash, source_lang, target_lang, backend, translated_text, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        rusqlite::params![text_hash, source_lang, target_lang, backend, translated, now_ms],
    )
    .context("inserting translation_cache row")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_conn() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))
    }

    #[test]
    fn ensure_cache_table_is_idempotent() {
        let c = fresh_conn();
        ensure_cache_table(&c).unwrap();
        ensure_cache_table(&c).unwrap();
        ensure_cache_table(&c).unwrap();
    }

    #[test]
    fn cache_round_trip_inserts_and_reads_back() {
        let c = fresh_conn();
        ensure_cache_table(&c).unwrap();
        let hash = hash_text("Bok, kako si?");
        cache_insert(&c, &hash, "bs", "en", "m2m100", "Hello, how are you?").unwrap();
        let got = cache_lookup(&c, &hash, "bs", "en", "m2m100").unwrap();
        assert_eq!(got.as_deref(), Some("Hello, how are you?"));
    }

    #[test]
    fn cache_miss_returns_none() {
        let c = fresh_conn();
        ensure_cache_table(&c).unwrap();
        let got = cache_lookup(&c, &hash_text("nope"), "de", "en", "m2m100").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn cache_keys_distinguish_backend() {
        // Same text + same direction, but different backends — two
        // separate cache entries.  Critical because m2m100 and
        // wmt21 might produce different outputs and the user might
        // want to A/B them.
        let c = fresh_conn();
        ensure_cache_table(&c).unwrap();
        let hash = hash_text("Bonjour");
        cache_insert(&c, &hash, "fr", "en", "m2m100", "Hello (m2m)").unwrap();
        cache_insert(&c, &hash, "fr", "en", "m2m100-wmt21", "Hello (wmt21)").unwrap();
        assert_eq!(
            cache_lookup(&c, &hash, "fr", "en", "m2m100").unwrap().as_deref(),
            Some("Hello (m2m)"),
        );
        assert_eq!(
            cache_lookup(&c, &hash, "fr", "en", "m2m100-wmt21").unwrap().as_deref(),
            Some("Hello (wmt21)"),
        );
    }

    #[test]
    fn cache_keys_distinguish_direction() {
        // Same text translated to different target languages — two
        // separate entries.  Same key for source/target swap is
        // unusual but the cache should support it.
        let c = fresh_conn();
        ensure_cache_table(&c).unwrap();
        let hash = hash_text("hello");
        cache_insert(&c, &hash, "en", "de", "m2m100", "hallo").unwrap();
        cache_insert(&c, &hash, "en", "fr", "m2m100", "bonjour").unwrap();
        assert_eq!(
            cache_lookup(&c, &hash, "en", "de", "m2m100").unwrap().as_deref(),
            Some("hallo"),
        );
        assert_eq!(
            cache_lookup(&c, &hash, "en", "fr", "m2m100").unwrap().as_deref(),
            Some("bonjour"),
        );
    }

    #[test]
    fn cache_insert_or_replace_overwrites() {
        // Re-translating the same input (e.g. user changed the
        // model file behind the scenes) replaces rather than
        // duplicates.  PRIMARY KEY semantics + INSERT OR REPLACE
        // gives us this for free; pin it with a test so future
        // schema edits don't accidentally drop the OR REPLACE.
        let c = fresh_conn();
        ensure_cache_table(&c).unwrap();
        let hash = hash_text("hello");
        cache_insert(&c, &hash, "en", "de", "m2m100", "hallo v1").unwrap();
        cache_insert(&c, &hash, "en", "de", "m2m100", "hallo v2").unwrap();
        assert_eq!(
            cache_lookup(&c, &hash, "en", "de", "m2m100").unwrap().as_deref(),
            Some("hallo v2"),
        );
    }

    #[test]
    fn hash_text_is_stable() {
        // Same input → same hash; different inputs → different hashes.
        // Underlying SHA-256 guarantees this; the test pins our
        // base16-lowercase encoding so a future swap to e.g. base64
        // doesn't silently invalidate every existing cache row.
        let a = hash_text("hello");
        let b = hash_text("hello");
        let c = hash_text("hello!");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64, "SHA-256 hex must be 64 chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
