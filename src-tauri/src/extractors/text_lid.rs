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
