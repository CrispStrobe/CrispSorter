//! Text-language identification consumer (P13.5 Phase 7, layer 1).
//!
//! Thin shim over upstream `crispasr::text_detect_language` (the
//! safe Rust wrapper landed in CrispASR `ee5e7cd8` exposing the
//! internal `text_lid_dispatch` façade — CLD3 + GlotLID-V3 + LID-176
//! fastText, routed by the loaded GGUF's `general.architecture`).
//!
//! Lives under `extractors/` because every concrete extractor
//! (`pdf.rs`, `text.rs`, `html.rs`, `audio.rs`, …) eventually calls
//! into this when the caller opts in via `ExtractOptions.text_lid_model`
//! — that plumbing is a follow-up slice; this module is just the
//! always-compile safe wrapper + the always-compile feature probe so
//! Phase 8's on-demand Tauri command can already use it.
//!
//! ## Label format
//!
//! Returns whatever label space the loaded GGUF speaks (NOT a
//! normalised ISO 639-1) — see `crispasr::TextLidResult` doc for
//! details.  CrispSorter's `asr::lang::Language` newtype is strict
//! 2-letter ISO 639-1; this module exposes [`normalise_to_iso_639_1`]
//! to bridge the two when the caller needs to feed into the
//! [`crate::asr::lang::route`] decision function.

use anyhow::Result;
use std::path::Path;

/// Result of [`detect_language`].  Carries the raw model label
/// (whatever CLD3 / fastText returns — `"en"`, `"zh-Latn"`,
/// `"eng_Latn"`) plus the posterior probability on it.  Callers
/// that need a strict ISO 639-1 code feed `label` through
/// [`normalise_to_iso_639_1`].
#[derive(Debug, Clone, PartialEq)]
pub struct TextLidResult {
    pub label: String,
    pub confidence: f32,
}

/// `true` when the `crispasr` cargo feature is compiled in.
/// Mirrors the [`crate::extractors::audio::is_audio_extraction_available`]
/// probe — pipeline code (Phase 7 layer 2) calls this BEFORE
/// dispatching so we can downgrade-without-language rather than
/// surface a generic extraction error.
pub fn is_text_lid_available() -> bool {
    cfg!(feature = "crispasr")
}

/// Detect the language of `text` via a pre-downloaded text-LID GGUF
/// (`model_path`).  Model resolution is the caller's responsibility
/// today — the registry-driven auto-resolution path is a Phase 7
/// layer-2 follow-up (needs a uniform LID-model naming convention
/// that CrispASR's registry currently doesn't expose).
#[cfg(feature = "crispasr")]
pub fn detect_language(
    text: &str,
    model_path: &Path,
    n_threads: i32,
) -> Result<TextLidResult> {
    if text.trim().is_empty() {
        anyhow::bail!("text-LID input is empty / whitespace-only");
    }
    if !model_path.exists() {
        anyhow::bail!("text-LID model not found at {}", model_path.display());
    }
    let model_path_str = model_path.to_string_lossy().into_owned();
    let result = crispasr::text_detect_language(text, &model_path_str, n_threads)
        .map_err(|e| anyhow::anyhow!("crispasr::text_detect_language: {e}"))?;
    Ok(TextLidResult {
        label: result.label,
        confidence: result.confidence,
    })
}

/// Stub for builds without the `crispasr` feature.  Same shape as
/// the audio extractor's stub: clear --features hint.
#[cfg(not(feature = "crispasr"))]
pub fn detect_language(
    _text: &str,
    _model_path: &Path,
    _n_threads: i32,
) -> Result<TextLidResult> {
    anyhow::bail!(
        "text-LID requires the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    )
}

/// Known text-LID model presets exposed by CrispASR's registry.
/// Each maps to a GGUF that [`resolve_lid_model`] can auto-download
/// on first use — same path the ASR side uses for whisper-base etc.
///
/// CLD3 is the most pragmatic default for ISO 639-1 output (109
/// labels, 440 KB).  GlotLID + LID-176 emit ISO 639-3 + script
/// (`eng_Latn`-style) and are bigger but cover the long tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LidPreset {
    /// `lid-cld3` — Google CLD3 GGUF, 109 ISO 639-1 labels, ~440 KB,
    /// Apache-2.0.  Smallest viable LID; the default fallback when
    /// the on-demand `translate_text` command isn't given an
    /// explicit `lid_model` path.
    Cld3,
    /// `lid-glotlid` — GlotLID-V3 fastText GGUF, 2102 labels (ISO
    /// 639-3 + script), ~250 MB, Apache-2.0.  Best coverage for
    /// low-resource languages.
    Glotlid,
    /// `lid-fasttext176` — Facebook LID-176 fastText GGUF, 176
    /// labels (ISO 639-1), ~63 MB, CC-BY-SA-3.0 (note the
    /// share-alike obligation if you publish derived work).
    Fasttext176,
}

impl LidPreset {
    /// The CrispASR registry key for this preset.  Stable string
    /// the user can also pass through CLI flags.
    pub fn registry_name(self) -> &'static str {
        match self {
            LidPreset::Cld3 => "lid-cld3",
            LidPreset::Glotlid => "lid-glotlid",
            LidPreset::Fasttext176 => "lid-fasttext176",
        }
    }

    /// Parse a registry name back into the enum.  Returns `None` for
    /// unrecognised inputs.  Symmetric with [`Self::registry_name`].
    pub fn from_registry_name(s: &str) -> Option<Self> {
        match s {
            "lid-cld3" => Some(LidPreset::Cld3),
            "lid-glotlid" => Some(LidPreset::Glotlid),
            "lid-fasttext176" => Some(LidPreset::Fasttext176),
            _ => None,
        }
    }
}

/// Auto-resolve a text-LID model via CrispASR's registry, downloading
/// to `cache_dir` on first use.  Subsequent calls return the cached
/// path without re-fetching.  Mirrors `Asr::load`'s
/// `registry_lookup → cache_ensure_file` chain.
///
/// `cache_dir` is the same per-app-data `models/` directory the ASR
/// side uses — the on-demand caller resolves this from
/// `AppState.data_dir`, the extractor uses its own helper.
///
/// The two registry calls are sync (CrispASR's contract), so this
/// wraps them in `tokio::task::spawn_blocking` to avoid blocking the
/// async caller's executor.
#[cfg(feature = "crispasr")]
pub async fn resolve_lid_model(
    preset: LidPreset,
    cache_dir: &Path,
) -> Result<std::path::PathBuf> {
    let name = preset.registry_name().to_string();
    let cache = cache_dir.to_string_lossy().into_owned();
    let path = tokio::task::spawn_blocking(move || -> Result<String> {
        let entry = crispasr::registry_lookup(&name)
            .map_err(|e| anyhow::anyhow!("registry_lookup {name}: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "text-LID preset {name} not in CrispASR registry — \
                     this is a build-time bug if the preset enum claims it"
                )
            })?;
        let p = crispasr::cache_ensure_file(&entry.filename, &entry.url, false, Some(&cache))
            .map_err(|e| anyhow::anyhow!("cache_ensure_file for {}: {e}", entry.filename))?
            .ok_or_else(|| {
                anyhow::anyhow!("cache returned no path for {}", entry.filename)
            })?;
        Ok(p)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking joined unexpectedly: {e}"))??;
    Ok(std::path::PathBuf::from(path))
}

/// Stub for non-crispasr builds.  Same shape as the rest of the
/// extractor's stubs — clear --features hint.
#[cfg(not(feature = "crispasr"))]
pub async fn resolve_lid_model(
    _preset: LidPreset,
    _cache_dir: &Path,
) -> Result<std::path::PathBuf> {
    anyhow::bail!(
        "text-LID model auto-resolution requires the `crispasr` cargo feature"
    )
}

/// Best-effort normalisation of a CrispASR text-LID label to an
/// ISO 639-1 two-letter code.
///
/// Handles the four shapes the dispatcher emits today:
///
/// 1. **CLD3 plain two-letter** (`"en"`, `"de"`) — pass through unchanged.
/// 2. **CLD3 with script tag** (`"zh-Latn"`) — strip the `-Latn` /
///    `-Hans` / `-Hant` suffix and keep the 2-letter prefix.
/// 3. **fastText ISO 639-3 + script** (`"eng_Latn"`, `"sco_Latn"`) —
///    look up the 3-letter prefix in [`ISO_639_3_TO_1`].
/// 4. **fastText ISO 639-3 only** (`"eng"`) — same 3-to-2 lookup.
///
/// Returns `None` for labels that don't map (e.g. fastText's
/// long-tail languages without a 2-letter ISO 639-1 assignment —
/// `"yue"` Cantonese, `"hbs"` Serbo-Croatian, …).  Callers should
/// either fall back to the raw label or surface "unknown
/// language" to the user.
///
/// The 3-to-1 mapping table is intentionally limited to the
/// languages CrispSorter cares about today (the parakeet EU-25 set
/// + the granite 6 + a few extras commonly requested).  Adding
/// languages is a one-line table entry — full ISO 639 coverage
/// would be a 2000-line table.
pub fn normalise_to_iso_639_1(label: &str) -> Option<String> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Split on `_` (fastText) and `-` (CLD3 script tag).  Take the
    // language part (before the separator); the script tag is
    // discarded for the 2-letter normalisation.  Languages that need
    // the script distinction (zh-Latn vs zh-Hans) must consume the
    // raw label upstream — we'd lose info here.
    let lang_part = trimmed
        .split(|c| c == '_' || c == '-')
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();

    match lang_part.len() {
        2 => Some(lang_part),
        3 => ISO_639_3_TO_1
            .iter()
            .find(|(three, _)| *three == lang_part.as_str())
            .map(|(_, two)| (*two).to_string()),
        _ => None,
    }
}

/// ISO 639-3 → ISO 639-1 mapping for the languages CrispSorter
/// cares about today.  Add entries as new languages come up — the
/// full table is ~2000 entries; this curated subset is the EU
/// parakeet-25 + the granite-6 + commonly-encountered others.
///
/// Keep alphabetised by 3-letter code so future additions land in a
/// predictable place.
static ISO_639_3_TO_1: &[(&str, &str)] = &[
    ("ara", "ar"),
    ("ben", "bn"),
    ("bul", "bg"),
    ("ces", "cs"),
    ("dan", "da"),
    ("deu", "de"),
    ("ell", "el"),
    ("eng", "en"),
    ("est", "et"),
    ("fin", "fi"),
    ("fra", "fr"),
    ("heb", "he"),
    ("hin", "hi"),
    ("hrv", "hr"),
    ("hun", "hu"),
    ("ita", "it"),
    ("jpn", "ja"),
    ("kor", "ko"),
    ("lav", "lv"),
    ("lit", "lt"),
    ("mlt", "mt"),
    ("nld", "nl"),
    ("nor", "no"),
    ("pol", "pl"),
    ("por", "pt"),
    ("ron", "ro"),
    ("rus", "ru"),
    ("slk", "sk"),
    ("slv", "sl"),
    ("spa", "es"),
    ("swe", "sv"),
    ("tha", "th"),
    ("tur", "tr"),
    ("ukr", "uk"),
    ("urd", "ur"),
    ("vie", "vi"),
    ("zho", "zh"),
];

#[cfg(test)]
mod tests {
    use super::*;

    // ── Probe ─────────────────────────────────────────────────────────

    #[test]
    fn availability_probe_matches_feature_flag() {
        assert_eq!(is_text_lid_available(), cfg!(feature = "crispasr"));
    }

    #[test]
    #[cfg(not(feature = "crispasr"))]
    fn detect_without_feature_errors_with_hint() {
        let err = detect_language("hello", Path::new("/nowhere.gguf"), 1)
            .expect_err("stub must error");
        let msg = err.to_string();
        assert!(msg.contains("crispasr"), "{msg}");
        assert!(msg.contains("--features"), "{msg}");
    }

    // ── Normalisation table ───────────────────────────────────────────

    #[test]
    fn normalise_passes_through_two_letter_codes() {
        // CLD3 plain output — every 2-letter code returns itself
        // unchanged (lowercased).
        for code in ["en", "de", "fr", "ja", "ZH", " es "] {
            let out = normalise_to_iso_639_1(code).unwrap_or_else(|| panic!("missing: {code}"));
            assert_eq!(out, code.trim().to_ascii_lowercase());
        }
    }

    #[test]
    fn normalise_strips_cld3_script_tag() {
        // CLD3's longer labels like "zh-Latn" / "zh-Hans" — script
        // tag carries information but we drop it for the strict
        // 2-letter normalisation.  Caller wanting script awareness
        // consumes the raw label.
        assert_eq!(normalise_to_iso_639_1("zh-Latn").as_deref(), Some("zh"));
        assert_eq!(normalise_to_iso_639_1("zh-Hans").as_deref(), Some("zh"));
        assert_eq!(normalise_to_iso_639_1("sr-Cyrl").as_deref(), Some("sr"));
    }

    #[test]
    fn normalise_maps_fasttext_three_letter() {
        // fastText emits "<3letter>_<script>" — the prefix is ISO
        // 639-3 which maps to ISO 639-1 via our curated table.
        let cases = [
            ("eng_Latn", "en"),
            ("deu_Latn", "de"),
            ("jpn_Jpan", "ja"),
            ("zho_Hans", "zh"),
            ("ukr_Cyrl", "uk"),
            ("ara_Arab", "ar"),
        ];
        for (lid_label, expected) in cases {
            assert_eq!(
                normalise_to_iso_639_1(lid_label).as_deref(),
                Some(expected),
                "label {lid_label:?} should map to {expected}",
            );
        }
    }

    #[test]
    fn normalise_returns_none_for_unmapped_three_letter() {
        // fastText's long-tail languages without an ISO 639-1
        // assignment — caller falls back to the raw label.
        // `yue` = Cantonese (no 639-1), `cmn` = Mandarin (no 639-1
        // separate from zh in our table), `nob` = Norwegian Bokmal.
        for label in ["yue_Hant", "cmn", "nob_Latn"] {
            assert!(
                normalise_to_iso_639_1(label).is_none(),
                "{label} should NOT map (not in our curated 3-to-1 table)"
            );
        }
    }

    #[test]
    fn normalise_rejects_garbage() {
        // Empty / whitespace / non-letter shapes — nothing to map.
        for bad in ["", "   ", "1234", "_", "-", "abcd"] {
            assert!(
                normalise_to_iso_639_1(bad).is_none(),
                "garbage {bad:?} should not map",
            );
        }
    }

    #[test]
    fn iso_table_is_alphabetised() {
        // Drift guard for the curated mapping — additions in the
        // wrong slot break readability and increase the chance of
        // accidental duplicates.  Alphabetise on the 3-letter code.
        for win in ISO_639_3_TO_1.windows(2) {
            assert!(
                win[0].0 < win[1].0,
                "ISO_639_3_TO_1 not alphabetised: {:?} comes before {:?}",
                win[0].0,
                win[1].0,
            );
        }
    }

    #[test]
    fn iso_table_has_no_duplicate_three_letter_keys() {
        // Each 3-letter code maps to exactly one 2-letter code.
        // Duplicates would mean the runtime lookup returns whichever
        // entry happened to be first, which is brittle.
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for (three, _) in ISO_639_3_TO_1 {
            assert!(seen.insert(*three), "duplicate 3-letter key: {three}");
        }
    }

    #[test]
    fn lid_preset_registry_names_round_trip() {
        // Drift guard: if a new preset lands, both directions must
        // know it.  Asymmetric mapping would cause auto-resolution
        // to silently miss a preset the enum claims.
        for preset in [LidPreset::Cld3, LidPreset::Glotlid, LidPreset::Fasttext176] {
            let name = preset.registry_name();
            assert_eq!(
                LidPreset::from_registry_name(name),
                Some(preset),
                "round-trip failed for {preset:?} → {name:?}"
            );
        }
    }

    #[test]
    fn lid_preset_from_registry_name_rejects_unknown() {
        for bogus in ["lid-cld4", "not-a-lid", "", "lid"] {
            assert!(
                LidPreset::from_registry_name(bogus).is_none(),
                "from_registry_name({bogus:?}) should be None",
            );
        }
    }

    #[test]
    fn lid_preset_registry_names_are_crispasr_canonical() {
        // These strings are the exact registry keys the CrispASR
        // README + crispasr_model_registry.cpp use.  Drift on either
        // side breaks auto-resolution silently (registry_lookup
        // returns None → "not in CrispASR registry" error).
        assert_eq!(LidPreset::Cld3.registry_name(), "lid-cld3");
        assert_eq!(LidPreset::Glotlid.registry_name(), "lid-glotlid");
        assert_eq!(LidPreset::Fasttext176.registry_name(), "lid-fasttext176");
    }

    #[test]
    fn iso_table_covers_parakeet_eu_25_via_three_letter() {
        // The parakeet-25 EU set we curated in asr/lang.rs uses ISO
        // 639-1 directly.  Every code in that set must reverse-look
        // up to at least one fastText 3-letter (so a fastText LID
        // output for a parakeet-supported language can normalise into
        // routing).  This is a one-way check (3→1); the 1→3 mapping
        // isn't unique (jpn / nip both → ja in the wider standard).
        let parakeet_25 = [
            "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el",
            "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es",
            "sv", "uk", "ru",
        ];
        let mapped_to_set: std::collections::HashSet<&&str> =
            ISO_639_3_TO_1.iter().map(|(_, two)| two).collect();
        for code in parakeet_25 {
            assert!(
                mapped_to_set.contains(&code),
                "parakeet EU-25 code {code:?} has no 3-letter mapping in ISO_639_3_TO_1 — \
                 fastText output for {code:?} won't normalise",
            );
        }
    }
}
