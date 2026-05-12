//! Per-filetype text-extraction registry.
//!
//! Phase 7.4.1 of PLAN P7. This module provides a single uniform entry
//! point `extract_text_from_path` that dispatches to a concrete
//! extractor based on the file's extension. The result is an
//! `ExtractedDocument` carrying the document's plain-text body plus
//! any headings the extractor was able to lift out — both fed into
//! the existing index ingest pipeline (full_text → embedding + body
//! field; headings_text → boosted Tantivy field).
//!
//! The registry is intentionally trait-free for now: dispatch is a
//! single match on the extension. The trait abstraction would be
//! useful if extractors needed to share state or be hot-swappable
//! at runtime, but neither is true today — every extractor is a
//! pure function file-path-in / text-out. We can promote to a trait
//! the moment we need that.
//!
//! Currently supported file types:
//! * **PDF** via the existing `pdf-extract` dep — same code path the
//!   `extract_pdf_native` Tauri command already uses.
//! * **Text + source** — UTF-8 read, no transformation. Covers .txt,
//!   .md, .rst, .log, .csv, .tsv, .json, .yaml, .toml, .xml, .html,
//!   plus most source-code extensions.
//! * **HTML** — basic tag-stripping via the regex crate. Lower
//!   fidelity than scraper but zero new heavy deps.
//!
//! Deferred to follow-ups (heavier deps): docx (zip + xml-rs), epub
//! (epub crate), rtf (rtf-grimoire). Once those land, this module
//! grows new dispatch arms; the public API stays the same.

use anyhow::{Context, Result};
use std::path::Path;

pub mod audio;
pub mod html;
pub mod ocr;
pub mod ocr_ocrs;
pub mod ocr_paddle;
pub mod pdf;
pub mod text;
pub mod text_lid;

/// One file's extracted text + structural breadcrumbs.
#[derive(Debug, Clone, Default)]
pub struct ExtractedDocument {
    /// Plain text body. Suitable as input to the embedder + as the
    /// `full_text` column in the documents table.
    pub full_text: String,
    /// Headings the extractor was able to find. Joined with newlines
    /// and fed into the boosted `headings_text` Tantivy field.
    pub headings: Vec<String>,
    /// Lowercased extension that was used for dispatch (e.g. `"pdf"`).
    /// Useful for downstream code that wants to differentiate by
    /// origin without re-parsing the path.
    pub ext: String,
    /// ISO 639-1 source language detected by the post-dispatch text-LID
    /// pass (P13.5 Phase 7), normalised through
    /// [`text_lid::normalise_to_iso_639_1`].  `None` when LID wasn't
    /// run (no model supplied) or wasn't able to map the detected
    /// label (long-tail language without an ISO 639-1 assignment).
    /// bg_ingest uses this as a fallback when the caller's
    /// `RawDocument.language` is empty.
    pub language: Option<String>,
}

/// Image extensions that OCR can handle. Surface them to `supported`
/// only when the caller opts in via `extract_text_from_path_with_opts`
/// — the no-OCR default would silently produce empty text for these
/// otherwise.
pub const OCR_IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp",
];

/// Extension classes the registry knows how to handle. Used as the
/// dispatch key so the match arms read top-down by category.
pub fn supported(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "pdf"
            // Plain text + light markup
            | "txt" | "md" | "markdown" | "rst" | "log"
            | "csv" | "tsv" | "json" | "jsonl"
            | "yaml" | "yml" | "toml" | "xml"
            // HTML gets its own arm (tag-strip), but list it here too
            // so callers can pre-filter accept lists with `supported`.
            | "html" | "htm"
            // Source code (UTF-8 read)
            | "rs" | "py" | "js" | "ts" | "tsx" | "jsx"
            | "svelte" | "vue"
            | "go" | "java" | "kt" | "swift" | "scala"
            | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp"
            | "rb" | "php" | "lua" | "r"
            | "sh" | "bash" | "zsh" | "fish"
            | "sql" | "graphql"
    )
}

/// Which OCR tier to try, in descending quality order.
///
/// `Auto` = try the best available tier at runtime:
///   Tier 3 (PaddleOCR) if compiled in → Tier 2 (ocrs) if models present →
///   Tier 1 (Tesseract) if installed → nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OcrTier {
    /// Pick the best available tier automatically.
    #[default]
    Auto,
    /// Tier 1 — Tesseract shell-out.
    Tier1,
    /// Tier 2 — ocrs (pure Rust, Latin-script).
    Tier2,
    /// Tier 3 — PaddleOCR via usls (requires `paddle-ocr` feature).
    Tier3,
}

/// Which recognition language model to use for PaddleOCR Tier 3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OcrRecLang {
    /// Automatically detect from filename heuristics.
    #[default]
    Auto,
    /// Latin-script languages (EN, DE, FR, …) — `ppocr_rec_v4_en`.
    Latin,
    /// CJK (Chinese, Japanese, Korean) — `ppocr_rec_v4_ch`.
    Cjk,
}

/// PLAN P7.8 + P13.5 Phase 7 options for the extractor dispatcher.
///
/// `Copy` was dropped when [`Self::text_lid_model`] (a `PathBuf`)
/// landed; the existing call sites pass `ExtractOptions` by value
/// into `spawn_blocking` (which moves anyway) or take it by reference
/// in tests, so the loss of `Copy` is a no-op.
#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    /// Run OCR on image extensions (png/jpg/tiff/…) and on PDFs whose
    /// text layer is empty after the regular `pdf::extract` pass.
    /// Off by default — OCR is CPU-heavy and most catalogs don't need it.
    pub try_ocr: bool,
    /// PDFs with fewer than this many extracted characters fall through
    /// to OCR if `try_ocr` is on.
    pub ocr_pdf_min_chars: usize,
    /// Which OCR tier to use. Default `Auto` picks the best available.
    pub ocr_tier: OcrTier,
    /// Which recognition language model to use for PaddleOCR.
    /// `Auto` uses the filename path to guess CJK vs. Latin.
    pub ocr_rec_lang: OcrRecLang,
    /// P13.5 Phase 7: path to a CrispASR text-LID GGUF
    /// (`lid-cld3` / `lid-glotlid` / `lid-fasttext176`).  When set,
    /// the dispatcher runs LID over the extracted `full_text` and
    /// writes the detected ISO 639-1 code into
    /// [`ExtractedDocument::language`].  `None` (default) skips LID
    /// — current behaviour, zero overhead.
    pub text_lid_model: Option<std::path::PathBuf>,
}

/// Run the appropriate extractor for `path`. Returns an empty
/// `ExtractedDocument` for unsupported extensions rather than erroring
/// — callers can pre-filter via `supported(ext)` if they want to skip.
pub fn extract_text_from_path(path: &Path) -> Result<ExtractedDocument> {
    extract_text_from_path_with_opts(
        path,
        ExtractOptions {
            try_ocr: false,
            ocr_pdf_min_chars: 50,
            ocr_tier: OcrTier::Auto,
            ocr_rec_lang: OcrRecLang::Auto,
            text_lid_model: None,
        },
    )
}

/// Variant that takes the OCR opt-in. Calling sites in bg_ingest +
/// CLI thread the user's catalog-level setting through here.
pub fn extract_text_from_path_with_opts(
    path: &Path,
    opts: ExtractOptions,
) -> Result<ExtractedDocument> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let result: Result<ExtractedDocument> = match ext.as_str() {
        "pdf" => {
            let mut doc = pdf::extract(path)?;
            doc.ext = ext.clone();
            // PLAN P7.8 — fall through to OCR if the PDF's text layer
            // is empty / near-empty (typical for scanned documents).
            if opts.try_ocr
                && doc.full_text.trim().chars().count() < opts.ocr_pdf_min_chars
            {
                if let Ok(mut ocr) = ocr::ocr_via_tesseract(path) {
                    ocr.ext = ext.clone();
                    Ok(ocr)
                } else {
                    Ok(doc)
                }
            } else {
                Ok(doc)
            }
        }
        "html" | "htm" => html::extract(path).map(|mut doc| {
            doc.ext = ext.clone();
            doc
        }),
        e if OCR_IMAGE_EXTS.contains(&e) && opts.try_ocr => {
            // PLAN P7.8 — tiered OCR for images.
            // Tier 3 (PaddleOCR, best quality, requires --features paddle-ocr)
            // → Tier 2 (ocrs, pure Rust, Latin-script)
            // → Tier 1 (Tesseract, system install).
            let want_tier3 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier3);
            let want_tier2 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier2);

            if want_tier3 && ocr_paddle::is_paddle_ocr_available() {
                if let Ok(mut doc) = ocr_paddle::ocr_via_paddle(path, opts.ocr_rec_lang) {
                    doc.ext = ext.clone();
                    return Ok(doc);
                }
            }
            if want_tier2 && ocr_ocrs::is_ocrs_available() {
                if let Ok(mut doc) = ocr_ocrs::ocr_via_ocrs(path) {
                    doc.ext = ext.clone();
                    return Ok(doc);
                }
            }
            ocr::ocr_via_tesseract(path).map(|mut doc| {
                doc.ext = ext.clone();
                doc
            })
        }
        e if audio::AUDIO_EXTS.contains(&e) => {
            // P13.5 slice B — audio / video → transcript.
            // Probe the feature flag first so the bg_ingest classifier
            // can downgrade to L2 metadata with a clear "feature off"
            // message rather than letting `audio::extract`'s actionable
            // stub error bubble through as a generic extraction
            // failure.  When the feature IS on, `extract` does the
            // decode-then-transcribe pipeline; first call also primes
            // the process-wide singleton ASR session.
            if !audio::is_audio_extraction_available() {
                Err(anyhow::anyhow!(
                    "audio extraction needs the `crispasr` cargo feature \
                     (rebuild with --features crispasr-metal / -cuda / -vulkan); \
                     skipped {}",
                    path.display()
                ))
            } else {
                audio::extract(path).map(|mut doc| {
                    doc.ext = ext.clone();
                    doc
                })
            }
        }
        e if supported(e) => text::extract(path).map(|mut doc| {
            doc.ext = ext.clone();
            doc
        }),
        _ => Err(anyhow::anyhow!(
            "no extractor for `.{ext}` ({})",
            path.display()
        )),
    };

    // ── P13.5 Phase 7: post-dispatch text-LID hook ──────────────────
    //
    // When the caller supplies a text-LID model path, run LID over
    // the extracted text and stash the detected ISO 639-1 code on
    // the document.  Errors here are non-fatal — extraction itself
    // succeeded, and a downstream search-side LanceDB row with no
    // `language` is fine; we just lose the language facet for this
    // document.  Logged so an admin watching the logs sees the
    // failure but it doesn't trip the bg_ingest failure classifier.
    let result = result.map(|mut doc| {
        if let Some(model_path) = opts.text_lid_model.as_deref() {
            // LID over a few hundred chars is plenty — models train
            // on 3–10 char-windows and the predictor is dominated by
            // n-gram frequencies.  Cap at 2000 chars to keep the
            // wall-clock bounded for huge inputs (50 MB transcripts,
            // EPUBs with all chapters concatenated).  A min-length
            // check skips tiny inputs where LID would be unreliable
            // anyway.
            let sample: String = doc.full_text.chars().take(2000).collect();
            let trimmed = sample.trim();
            if trimmed.len() >= 20 {
                match text_lid::detect_language(trimmed, model_path, 2) {
                    Ok(r) => {
                        doc.language = text_lid::normalise_to_iso_639_1(&r.label)
                            .or_else(|| {
                                // Fall back to the raw label when our
                                // 3-to-1 table doesn't cover it; a
                                // downstream filter or facet can still
                                // group by the raw string even though
                                // it isn't ISO 639-1.
                                Some(r.label)
                            });
                    }
                    Err(e) => {
                        eprintln!(
                            "[extractor] text-LID failed for {} (non-fatal): {e:#}",
                            path.display()
                        );
                    }
                }
            }
        }
        doc
    });

    result.with_context(|| format!("extracting {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_recognises_common_types() {
        assert!(supported("pdf"));
        assert!(supported("txt"));
        assert!(supported("md"));
        assert!(supported("rs"));
        assert!(supported("HTML")); // case-insensitive
        assert!(!supported("docx")); // deferred
        assert!(!supported("zip"));
        assert!(!supported(""));
    }

    #[test]
    fn extract_dispatches_on_extension() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("hello.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let doc = extract_text_from_path(&p).unwrap();
        assert_eq!(doc.full_text, "hello world");
        assert_eq!(doc.ext, "txt");
    }

    #[test]
    fn unknown_extension_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("file.xyz");
        std::fs::write(&p, b"opaque").unwrap();
        let res = extract_text_from_path(&p);
        assert!(res.is_err());
    }

    #[test]
    fn extract_options_default_skips_lid() {
        // Phase 7 contract: an `ExtractOptions::default()` (or the
        // existing `extract_text_from_path()` wrapper which doesn't
        // expose the new field) must NOT touch text-LID — zero
        // overhead on the no-LID path is the design goal.  Pin the
        // default value so a future "let's auto-enable LID for
        // convenience" PR can't slip in without flagging the
        // performance change here.
        let opts = ExtractOptions::default();
        assert!(opts.text_lid_model.is_none());
    }

    #[test]
    fn extract_without_lid_leaves_language_none() {
        // The post-dispatch LID hook is the only writer of
        // `ExtractedDocument.language`.  With no model configured,
        // every extractor's output must carry `language = None` so
        // bg_ingest's `item.language.or(extracted.language)` priority
        // chain correctly falls through to the item metadata.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("hello.txt");
        std::fs::write(&p, b"this is some english text").unwrap();
        let doc = extract_text_from_path(&p).unwrap();
        assert!(
            doc.language.is_none(),
            "text-LID hook must NOT fire without an opts.text_lid_model — got {:?}",
            doc.language,
        );
    }

    #[test]
    fn audio_extension_routes_to_audio_extractor() {
        // P13.5 slice B — the dispatch arm for AUDIO_EXTS must be
        // reachable from a plain `.wav` path.  This test verifies the
        // routing without actually running the decoder + ASR (which
        // needs a real audio file + the crispasr feature).
        //
        // Strategy: write an empty stub file with a known audio
        // extension and call `extract_text_from_path`.  The expected
        // result is NOT the "no extractor" error from the dispatch
        // fall-through (which would mean the dispatch arm broke);
        // it's either:
        //   * (no-feature) the audio module's "needs --features
        //     crispasr" stub error, OR
        //   * (with-feature) the audio module's decode error (since
        //     the stub file isn't a valid WAV).
        // Either case proves the file reached audio::extract instead
        // of falling through to the catch-all.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("not-actually-audio.wav");
        std::fs::write(&p, b"").unwrap();

        let res = extract_text_from_path(&p);
        let err = res.expect_err("must error — empty stub file isn't decodable");
        let msg = format!("{err:#}");
        // The catch-all error would say "no extractor for `.wav`" —
        // anything else means we successfully routed to audio.rs.
        assert!(
            !msg.contains("no extractor for `.wav`"),
            "dispatch fell through to the catch-all: {msg}"
        );
    }

    #[test]
    fn extract_options_defaults_are_safe() {
        let opts = ExtractOptions::default();
        assert!(!opts.try_ocr,                                "OCR off by default");
        assert_eq!(opts.ocr_pdf_min_chars, 0);                // Default<usize> is 0
        assert_eq!(opts.ocr_tier,     OcrTier::Auto);
        assert_eq!(opts.ocr_rec_lang, OcrRecLang::Auto);
    }

    #[test]
    fn ocr_tier_default_is_auto() {
        let t: OcrTier = Default::default();
        assert_eq!(t, OcrTier::Auto);
    }

    #[test]
    fn ocr_rec_lang_default_is_auto() {
        let l: OcrRecLang = Default::default();
        assert_eq!(l, OcrRecLang::Auto);
    }

    #[test]
    fn supported_handles_uppercase_extensions() {
        // Insensitivity is critical for files coming from Windows / camera ROMs.
        for ext in ["PDF", "Pdf", "MD", "Rs", "Html", "TXT"] {
            assert!(supported(ext), "should accept {ext}");
        }
    }

    #[test]
    fn supported_rejects_image_exts_unless_ocr() {
        // Image OCR is opt-in via try_ocr; supported() returns false so callers
        // pre-filtering accept lists don't surface them as text-extractable.
        for ext in OCR_IMAGE_EXTS {
            assert!(!supported(ext), "image ext {ext} must not be in supported()");
        }
    }

    #[test]
    fn extract_text_with_opts_no_ocr_skips_image() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("scan.png");
        std::fs::write(&p, b"\x89PNG").unwrap(); // not really a PNG but extension is enough
        let opts = ExtractOptions { try_ocr: false, ..Default::default() };
        let res = extract_text_from_path_with_opts(&p, opts);
        assert!(res.is_err(), "no-OCR + image must error (no extractor)");
    }

    #[test]
    fn ext_is_lowercased_on_dispatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("FILE.TXT");
        std::fs::write(&p, b"data").unwrap();
        let doc = extract_text_from_path(&p).unwrap();
        assert_eq!(doc.ext, "txt"); // not "TXT"
    }
}
