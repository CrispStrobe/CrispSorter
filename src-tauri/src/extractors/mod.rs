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
pub mod layout;
pub mod math_ocr;
pub mod ocr;
pub mod ocr_crispembed;
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
    /// P13.5 Phase 8 batch — translated text produced by the
    /// post-dispatch MT pass when [`ExtractOptions::translate_to`]
    /// was set AND [`Self::language`] is known.  `None` when
    /// translation wasn't run (no `translate_to`), when no source
    /// language could be determined (no LID model), or when source
    /// equals target (identity short-circuit).
    pub translated_text: Option<String>,
    /// ISO 639-1 target language of [`Self::translated_text`].
    /// Echoes [`ExtractOptions::translate_to`] on success so
    /// downstream consumers (the LanceDB write path in Phase 8b)
    /// know which column the text belongs to.
    pub translated_to_lang: Option<String>,
    /// P13.6 Step 3c — L2 audio metadata.  Populated only by the
    /// audio extractor via [`crate::audio::probe::probe_metadata`]
    /// (symphonia format-reader probe, no decode pass).  All
    /// fields optional per the underlying probe — see
    /// [`crate::audio::probe::AudioMetadata`] for per-field
    /// semantics.  Plumbed into LanceDB columns `audio_*` by the
    /// `AddAudioMetadataColumns` migration (v101).  `None` for
    /// non-audio extractors.
    pub audio: Option<crate::audio::probe::AudioMetadata>,
    /// P13.6 Step 9 — L2 image (EXIF) metadata.  Populated only by
    /// the OCR extractor path via [`crate::images::exif::read_exif`]
    /// (kamadak-exif under the hood).  Full struct is held here for
    /// frontend display + future migrations; bg_ingest copies the
    /// curated subset (camera_make / camera_model / lens_model /
    /// taken_at_unix / iso) into RawDocument's flat image_* fields.
    /// `None` for non-image extractors and for images whose EXIF
    /// block is unparseable or absent (common after re-saves
    /// through phone galleries / Telegram).
    pub image_exif: Option<crate::images::exif::ExifSummary>,

    /// v106 — Original source URL the document came from.  Populated
    /// by the markdown extractor's YAML-frontmatter pass (`url:` key)
    /// and, in time, by other extractors that can recover provenance
    /// (PDF `/URL` in XMP, EPUB `<dc:source>`, browser-saved HTML).
    /// `None` for files with no source URL.  bg_ingest copies this
    /// into `RawDocument.url` which lands on `DocumentChunk.url`.
    pub source_url: Option<String>,

    /// v107 — Tags lifted from YAML frontmatter (`tags: [...]`),
    /// EPUB `<dc:subject>`, DOCX keywords, etc.  Empty `Vec` means
    /// "no tags".  bg_ingest folds these into `RawDocument.tags`
    /// (already used for the existing `collection:<id>` routing
    /// markers) so they survive into both `DocumentChunk.tags` and
    /// the cb-api wire's `ManifestRow.tags`.
    pub tags: Vec<String>,
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
///   Tier 4 (CrispEmbed) if compiled in → Tier 3 (PaddleOCR) →
///   Tier 2 (ocrs) → Tier 1 (Tesseract) → nothing.
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
    /// Tier 4 — CrispEmbed GGUF OCR (Surya/Qwen2.5-VL/DBNet+TrOCR,
    /// requires `crispembed` feature).
    Tier4,
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

/// Configuration for the C++ OCR pipeline orchestrator (CrispEmbed
/// `CrispOcrPipeline`): source-type routing + per-stage image cleanup
/// (classical + NAFNet) + accept-gate escalation. When
/// [`Self::enabled`] is false the extractor uses the legacy Rust tier
/// ladder unchanged. Mirrors the flat `crispembed_ocr_pipeline_params`
/// C struct so it threads straight through.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrPipelineConfig {
    /// Master switch. `false` (default) → legacy Rust tier ladder.
    #[serde(default)]
    pub enabled: bool,
    /// Classify the image (screenshot / scanned-doc / photo) and route to
    /// the matching cleanup+engine recipe.
    #[serde(default = "default_true")]
    pub router: bool,
    /// Run per-stage scan cleanup (deskew/crop/whiten/binarize) before OCR.
    #[serde(default = "default_true")]
    pub cleanup_enabled: bool,
    /// Enable the learned NAFNet tier-2 denoise (downloads ~30 MB on first use).
    #[serde(default)]
    pub denoise: bool,
    /// Accept-gate: minimum recognized characters before escalating.
    #[serde(default = "default_ocr_min_chars")]
    pub min_chars: i32,
    /// Accept-gate: minimum mean region confidence (0 = ignore).
    #[serde(default = "default_ocr_min_confidence")]
    pub min_confidence: f32,
    /// Detection model registry name (`None` → `surya-det`).
    #[serde(default)]
    pub det_model: Option<String>,
    /// Recognition model registry name (`None` → `qwen2vl-ocr`).
    #[serde(default)]
    pub rec_model: Option<String>,
    /// NAFNet denoise GGUF registry name (`None` → `nafnet-denoise`).
    #[serde(default)]
    pub nafnet_model: Option<String>,
    /// Optional post-OCR punctuation/spacing/truecasing restorer (FireRedPunc /
    /// PCS / fullstop-punc) applied to the joined text. `None`/empty = off.
    #[serde(default)]
    pub punct_model: Option<String>,
    /// Full per-stage builder. When non-empty, the pipeline is built from these
    /// explicit stages (full tweakability) instead of the flat fields above —
    /// each stage picks an engine + models + cleanup recipe + engine params +
    /// accept-gate, grouped into per-source-type chains in order.
    #[serde(default)]
    pub stages: Vec<OcrStageSpec>,
}

/// Per-stage cleanup recipe (mirrors `crispembed::OcrCleanupSpec`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrCleanupSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub deskew: bool,
    #[serde(default = "default_true")]
    pub crop_borders: bool,
    #[serde(default = "default_true")]
    pub whiten_background: bool,
    #[serde(default)]
    pub binarize: bool,
    #[serde(default)]
    pub binarize_method: i32, // 0=Otsu 1=Sauvola
    #[serde(default = "default_sauvola_k")]
    pub sauvola_k: f32,
    #[serde(default = "default_sauvola_window")]
    pub sauvola_window: i32,
    #[serde(default = "default_morph_kernel")]
    pub morph_kernel: i32,
    #[serde(default = "default_border_threshold")]
    pub border_threshold: f32,
    #[serde(default = "default_deskew_max_angle")]
    pub deskew_max_angle: f32,
    #[serde(default)]
    pub denoise: bool, // NAFNet tier-2
}

fn default_sauvola_k() -> f32 { 0.2 }
fn default_sauvola_window() -> i32 { 25 }
fn default_morph_kernel() -> i32 { 51 }
fn default_border_threshold() -> f32 { 0.15 }
fn default_deskew_max_angle() -> f32 { 15.0 }

impl Default for OcrCleanupSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            deskew: true,
            crop_borders: true,
            whiten_background: true,
            binarize: false,
            binarize_method: 0,
            sauvola_k: 0.2,
            sauvola_window: 25,
            morph_kernel: 51,
            border_threshold: 0.15,
            deskew_max_angle: 15.0,
            denoise: false,
        }
    }
}

/// One fully-specified pipeline stage (full per-stage builder).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrStageSpec {
    /// `auto` | `screenshot` | `scanned_doc` | `photo`.
    #[serde(default = "default_source_type")]
    pub source_type: String,
    /// `dbnet_trocr` | `surya` | `got` | `glm` | `qwen2vl` | `internvl2`.
    #[serde(default = "default_engine")]
    pub engine: String,
    /// Detection / single model registry name.
    #[serde(default)]
    pub det_model: Option<String>,
    /// Recognition model (dbnet_trocr / surya).
    #[serde(default)]
    pub rec_model: Option<String>,
    #[serde(default)]
    pub cleanup: OcrCleanupSpec,
    #[serde(default = "default_det_prob")]
    pub det_prob_threshold: f32,
    #[serde(default = "default_det_box")]
    pub det_box_threshold: f32,
    #[serde(default = "default_det_short")]
    pub det_target_short: i32,
    #[serde(default)]
    pub vlm_max_tokens: i32,
    #[serde(default)]
    pub vlm_prompt: String,
    #[serde(default = "default_ocr_min_chars")]
    pub min_chars: i32,
    #[serde(default = "default_ocr_min_confidence")]
    pub min_confidence: f32,
}

fn default_source_type() -> String { "auto".to_string() }
fn default_engine() -> String { "dbnet_trocr".to_string() }
fn default_det_prob() -> f32 { 0.3 }
fn default_det_box() -> f32 { 0.5 }
fn default_det_short() -> i32 { 736 }

fn default_true() -> bool {
    true
}
fn default_ocr_min_chars() -> i32 {
    8
}
fn default_ocr_min_confidence() -> f32 {
    0.5
}

impl Default for OcrPipelineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            router: true,
            cleanup_enabled: true,
            denoise: false,
            min_chars: default_ocr_min_chars(),
            min_confidence: default_ocr_min_confidence(),
            det_model: None,
            rec_model: None,
            nafnet_model: None,
            punct_model: None,
            stages: Vec::new(),
        }
    }
}

/// Map an engine string to the C builder's engine id.
fn engine_id(name: &str) -> i32 {
    match name {
        "surya" => 1,
        "got" => 2,
        "glm" => 3,
        "qwen2vl" => 4,
        "internvl2" => 5,
        "tesseract" => 6,
        _ => 0, // dbnet_trocr
    }
}

/// Map a source-type string to the C builder's source-type id.
fn source_type_id(name: &str) -> i32 {
    match name {
        "screenshot" => 1,
        "scanned_doc" => 2,
        "photo" => 3,
        _ => 0, // auto
    }
}

/// PLAN P7.8 + P13.5 Phase 7 options for the extractor dispatcher.
///
/// `Copy` was dropped when [`Self::text_lid_model`] (a `PathBuf`)
/// landed; the existing call sites pass `ExtractOptions` by value
/// into `spawn_blocking` (which moves anyway) or take it by reference
/// in tests, so the loss of `Copy` is a no-op.
#[derive(Debug, Clone)]
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
    /// P13.5 Phase 8 batch: ISO 639-1 target language for an
    /// index-time translation pass.  When set, the dispatcher runs
    /// MT after LID (LID has to land first to know the source
    /// language) and stashes the translation into
    /// [`ExtractedDocument::translated_text`].  Requires
    /// `text_lid_model` to be set too (otherwise no source lang is
    /// available); skipped silently otherwise.  `None` (default)
    /// skips translation — zero overhead on the no-translate path.
    pub translate_to: Option<String>,
    /// P13.5 Phase 8 batch: MT backend name.  Defaults to `m2m100`
    /// (100 langs, any-to-any) when [`Self::translate_to`] is set
    /// but this is `None`.  Ignored when `translate_to` is `None`.
    pub translate_backend: Option<String>,
    /// P13.5 Phase 8 batch: explicit MT model file path.  `None`
    /// uses CrispASR's registry auto-download.  Ignored when
    /// `translate_to` is `None`.
    pub translate_model: Option<std::path::PathBuf>,
    /// P13.6 Step 5 — when `false`, audio + video extensions fall
    /// through to L1 metadata-only (the dispatcher returns an
    /// "extraction skipped by user setting" error so the bg_ingest
    /// classifier downgrades to L1 with a clear reason).  Default
    /// `true` so legacy callers (CLI, the BatchReview JS path) keep
    /// their previous behaviour without explicit opt-in.  bg_ingest
    /// reads this from `IndexConfig.audio_extraction_enabled`.
    pub audio_extraction_enabled: bool,
    /// P13.6 Step 7c — how deeply to ingest audio/video.  `L1`
    /// short-circuits the extractor entirely; `L2` runs the cheap
    /// symphonia probe but skips ASR; `L3` (default) runs the full
    /// decode → transcribe pipeline.  bg_ingest reads this from
    /// `IndexConfig.ingest_audio_level`.  Plumbed as a String here
    /// so the existing serde Deserialize on IndexConfig doesn't
    /// need to import the enum across module boundaries; the
    /// dispatcher matches on the canonical "l1" / "l2" / "l3"
    /// kebab-case values.
    pub ingest_audio_level: String,
    /// P13.7 Step 3 — image-extraction master switch.  When
    /// `false`, the OCR dispatch arm short-circuits with an
    /// explicit "skipped by Settings" error so bg_ingest writes
    /// the file as L1 metadata-only.  Default `true` preserves
    /// pre-P13.7 behaviour for legacy callers.  bg_ingest reads
    /// this from `IndexConfig.image_extraction_enabled`.
    pub image_extraction_enabled: bool,
    /// OCR pipeline orchestrator config (C++ cleanup + routing + accept-gate).
    /// When `enabled` is false the legacy Rust tier ladder runs unchanged.
    /// bg_ingest fills this from the persisted OCR-pipeline settings.
    pub ocr_pipeline: OcrPipelineConfig,
    /// P13.7 Step 1 — how deeply to ingest images.  `"l1"`
    /// short-circuits the extractor entirely; `"l2"` runs the
    /// kamadak-exif probe but skips OCR; `"l3"` (default) runs
    /// EXIF + OCR.  Plumbed as a String matching the audio enum
    /// pattern (canonical kebab-case `"l1"` / `"l2"` / `"l3"`).
    pub ingest_image_level: String,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            try_ocr: false,
            ocr_pdf_min_chars: 0,
            ocr_tier: OcrTier::default(),
            ocr_rec_lang: OcrRecLang::default(),
            text_lid_model: None,
            translate_to: None,
            translate_backend: None,
            translate_model: None,
            // P13.6 Step 5 default — audio extraction ON unless the
            // caller (bg_ingest reading IndexConfig) explicitly
            // disables it.
            audio_extraction_enabled: true,
            // P13.6 Step 7c default — full pipeline.  bg_ingest
            // overrides per IndexConfig.ingest_audio_level.
            ingest_audio_level: "l3".to_string(),
            // P13.7 default — image extraction ON, L3 full pipeline.
            image_extraction_enabled: true,
            ingest_image_level: "l3".to_string(),
            // OCR pipeline orchestrator off by default → legacy ladder.
            ocr_pipeline: OcrPipelineConfig::default(),
        }
    }
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
            translate_to: None,
            translate_backend: None,
            translate_model: None,
            // P13.6 Step 5 — keep audio extraction ON for the
            // no-opts legacy API; only bg_ingest reading
            // IndexConfig flips it off.
            audio_extraction_enabled: true,
            ingest_audio_level: "l3".to_string(),
            // P13.7 — image side parallel defaults.
            image_extraction_enabled: true,
            ingest_image_level: "l3".to_string(),
            ocr_pipeline: OcrPipelineConfig::default(),
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
        e if OCR_IMAGE_EXTS.contains(&e) => {
            // PLAN P7.8 — tiered OCR for images.
            // P13.7 Step 1+3 — image-side L1/L2/L3 + master-switch
            // gate, parallel to the audio dispatch arm.
            //
            //   L1 OR image_extraction_enabled=false → skip
            //                                          (filesystem
            //                                          metadata only).
            //   L2 → EXIF probe only; full_text stays "".  Still
            //        populates image_* columns.
            //   L3 (default) → EXIF + OCR (when opts.try_ocr).
            //                  Without try_ocr, L3 behaves like L2 —
            //                  the OCR-tier ladder doesn't fire.
            let level = opts.ingest_image_level.as_str();
            if level == "l1" || !opts.image_extraction_enabled {
                return Err(anyhow::anyhow!(
                    "image extraction skipped at L1 by Settings; skipped {}",
                    path.display()
                ));
            }
            // EXIF runs unconditionally for L2 + L3.  Cheap; failed
            // parses come back as None.
            let image_exif = crate::images::exif::read_exif(path).ok();

            if level == "l2" || !opts.try_ocr {
                // L2 or "L3 but OCR off" — return an empty-text doc
                // with image_exif populated.  bg_ingest writes the
                // image_* columns either way.
                return Ok(ExtractedDocument {
                    full_text: String::new(),
                    headings: Vec::new(),
                    ext: ext.clone(),
                    language: None,
                    translated_text: None,
                    translated_to_lang: None,
                    audio: None,
                    image_exif,
                    source_url: None,
                    tags: vec![],
                });
            }

            // L3 + OCR enabled.
            //
            // Smart pipeline (C++ orchestrator): when enabled, route to the
            // CrispEmbed pipeline (source-type cleanup + NAFNet denoise +
            // accept-gate). On failure (e.g. models unavailable) fall through
            // to the legacy Rust tier ladder below.
            if opts.ocr_pipeline.enabled && ocr_crispembed::is_crispembed_ocr_available() {
                match ocr_crispembed::ocr_via_pipeline(path, &opts.ocr_pipeline) {
                    Ok(mut doc) => {
                        doc.ext = ext.clone();
                        doc.image_exif = image_exif;
                        return Ok(doc);
                    }
                    Err(e) => {
                        eprintln!(
                            "[ocr] smart pipeline failed ({e:#}); falling back to tier ladder"
                        );
                    }
                }
            }

            // Legacy tier ladder.
            // P17.2 — CrispEmbed Tier 4 at the top when compiled in.
            let want_tier4 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier4);
            let want_tier3 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier3);
            let want_tier2 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier2);
            let mut doc = if want_tier4 && ocr_crispembed::is_crispembed_ocr_available() {
                match ocr_crispembed::ocr_via_crispembed(path) {
                    Ok(d) => d,
                    Err(_) => {
                        // Fall through to lower tiers.
                        if want_tier3 && ocr_paddle::is_paddle_ocr_available() {
                            ocr_paddle::ocr_via_paddle(path, opts.ocr_rec_lang)
                                .or_else(|_| {
                                    if want_tier2 && ocr_ocrs::is_ocrs_available() {
                                        ocr_ocrs::ocr_via_ocrs(path)
                                    } else {
                                        ocr::ocr_via_tesseract(path)
                                    }
                                })?
                        } else if want_tier2 && ocr_ocrs::is_ocrs_available() {
                            ocr_ocrs::ocr_via_ocrs(path)
                                .or_else(|_| ocr::ocr_via_tesseract(path))?
                        } else {
                            ocr::ocr_via_tesseract(path)?
                        }
                    }
                }
            } else if want_tier3 && ocr_paddle::is_paddle_ocr_available() {
                match ocr_paddle::ocr_via_paddle(path, opts.ocr_rec_lang) {
                    Ok(d) => d,
                    Err(_) => {
                        if want_tier2 && ocr_ocrs::is_ocrs_available() {
                            ocr_ocrs::ocr_via_ocrs(path)
                                .or_else(|_| ocr::ocr_via_tesseract(path))?
                        } else {
                            ocr::ocr_via_tesseract(path)?
                        }
                    }
                }
            } else if want_tier2 && ocr_ocrs::is_ocrs_available() {
                ocr_ocrs::ocr_via_ocrs(path).or_else(|_| ocr::ocr_via_tesseract(path))?
            } else {
                ocr::ocr_via_tesseract(path)?
            };
            doc.ext = ext.clone();
            doc.image_exif = image_exif;
            Ok(doc)
        }
        e if audio::AUDIO_EXTS.contains(&e) => {
            // P13.5 slice B / P13.6 Step 7c — audio / video.
            //
            // Three Settings-driven gates before the full decode +
            // transcribe pipeline fires:
            //   1. ingest_audio_level == "l1": user wants filesystem
            //      metadata only.  Skip ALL extraction.
            //   2. audio_extraction_enabled == false: master switch
            //      from P13.6 Step 5.  Same skip behaviour.
            //   3. Feature flag check: `crispasr` not compiled in.
            //      Same skip path with a different "rebuild with
            //      --features" message.
            //
            // ingest_audio_level == "l2" — run the symphonia probe
            // (cheap, no decode) and produce an ExtractedDocument
            // with the L2 fields populated but an empty transcript.
            // bg_ingest still writes the audio_* columns; the
            // full_text stays empty so search-by-content doesn't
            // match the audio file body.
            //
            // ingest_audio_level == "l3" (default) — full pipeline.
            let level = opts.ingest_audio_level.as_str();
            if level == "l1" || !opts.audio_extraction_enabled {
                Err(anyhow::anyhow!(
                    "audio extraction skipped at L1 by Settings; skipped {}",
                    path.display()
                ))
            } else if !audio::is_audio_extraction_available() {
                Err(anyhow::anyhow!(
                    "audio extraction needs the `crispasr` cargo feature \
                     (rebuild with --features crispasr-metal / -cuda / -vulkan); \
                     skipped {}",
                    path.display()
                ))
            } else if level == "l2" {
                // L2 — probe only, no decode/transcribe.  Builds an
                // ExtractedDocument with audio = Some(metadata) and
                // full_text = "" so the bg_ingest writer still
                // populates the audio_* columns and the search
                // doesn't have to recognise an L2 audio row as
                // anything special.
                #[cfg(feature = "crispasr")]
                {
                    let audio_meta = crate::audio::probe::probe_metadata(path).ok();
                    Ok(ExtractedDocument {
                        full_text: String::new(),
                        headings: Vec::new(),
                        ext: ext.clone(),
                        language: None,
                        translated_text: None,
                        translated_to_lang: None,
                        audio: audio_meta,
                        image_exif: None,
                        source_url: None,
                        tags: vec![],
                    })
                }
                #[cfg(not(feature = "crispasr"))]
                {
                    Err(anyhow::anyhow!(
                        "L2 audio probe needs the `crispasr` cargo feature; skipped {}",
                        path.display()
                    ))
                }
            } else {
                // L3 — full pipeline (decode + transcribe + probe).
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

        // ── P13.5 Phase 8 batch: post-LID translation hook ─────────
        //
        // When the caller supplies `translate_to`, AND a source
        // language is now known (either set explicitly by some
        // future caller or produced by the LID hook above), AND it
        // differs from the target, run the MT pass.  Same non-fatal
        // policy: extraction-level success is what we report; a
        // failed translation just leaves `translated_text = None`
        // and downstream code falls back to the original.
        if let Some(target) = opts.translate_to.as_deref() {
            let target = target.trim().to_ascii_lowercase();
            if let Some(source) = doc.language.as_deref() {
                if !source.is_empty() && !target.is_empty() && source != target {
                    let backend = opts
                        .translate_backend
                        .as_deref()
                        .unwrap_or("m2m100")
                        .to_string();
                    let model_path = opts.translate_model.clone();
                    match run_batch_translation(
                        &doc.full_text,
                        source,
                        &target,
                        &backend,
                        model_path,
                    ) {
                        Ok(translated) => {
                            doc.translated_text = Some(translated);
                            doc.translated_to_lang = Some(target);
                        }
                        Err(e) => {
                            eprintln!(
                                "[extractor] batch translation failed for {} (non-fatal): {e:#}",
                                path.display()
                            );
                        }
                    }
                }
            }
        }

        doc
    });

    result.with_context(|| format!("extracting {}", path.display()))
}

/// Run an index-time translation pass over `text` via a process-
/// level shared MT handle.  Bridges the sync extractor surface into
/// the async [`crate::asr::AsrHandle`] surface by spinning up a
/// current-thread tokio runtime (same shape `cmd_chat_transcribe`
/// uses) — bg_ingest calls extractors via `tokio::task::spawn_blocking`
/// so we're already on a dedicated blocking thread.
fn run_batch_translation(
    text: &str,
    source: &str,
    target: &str,
    backend: &str,
    model_path: Option<std::path::PathBuf>,
) -> Result<String> {
    use std::sync::OnceLock;
    static MT_HANDLE: OnceLock<crate::asr::AsrHandle> = OnceLock::new();

    let handle = MT_HANDLE.get_or_init(|| {
        let config = match &model_path {
            Some(p) => crate::asr::AsrConfig::with_model_path(backend, p.to_string_lossy()),
            None => crate::asr::AsrConfig::new(backend),
        };
        // Cache dir mirrors the audio extractor's pattern — the
        // same models dir under <data-dir>.  Errors creating the
        // dir are swallowed; AsrHandle::load will surface them on
        // first transcribe call.
        let cache_dir = batch_translate_cache_dir();
        crate::asr::AsrHandle::new(config, cache_dir)
    });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| "constructing tokio runtime for batch translation")?;
    let translated = rt
        .block_on(handle.translate_text(
            text.to_string(),
            source.to_string(),
            target.to_string(),
            0, // upstream default (200 tokens for m2m100)
        ))
        .with_context(|| format!("translate {source}→{target} via {backend}"))?;
    Ok(translated)
}

/// Default cache dir for the batch-translation MT model — mirrors
/// `extractors::audio`'s resolution so the same downloaded GGUFs
/// are shared across ASR + MT + text-LID surfaces.
#[cfg(target_os = "macos")]
fn batch_translate_cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|h| h.join("Library/Application Support/com.crispstrobe.crispsorter"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/crispsorter"));
    let dir = base.join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
#[cfg(target_os = "windows")]
fn batch_translate_cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .map(|a| a.join("com.crispstrobe.crispsorter"))
        .unwrap_or_else(|| std::path::PathBuf::from("C:\\Temp\\crispsorter"));
    let dir = base.join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn batch_translate_cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|h| h.join(".local/share"))
        })
        .map(|d| d.join("com.crispstrobe.crispsorter"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/crispsorter"));
    let dir = base.join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
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
    fn extract_options_default_skips_translate() {
        // Phase 8 batch contract: defaults preserve zero-overhead
        // behaviour.  Same shape as the Phase 7 LID-default test —
        // protects against a future "let's translate everything by
        // default" PR slipping by without flagging the perf cost
        // (MT models are large + slow).
        let opts = ExtractOptions::default();
        assert!(opts.translate_to.is_none());
        assert!(opts.translate_backend.is_none());
        assert!(opts.translate_model.is_none());
    }

    #[test]
    fn extract_without_translate_to_leaves_translation_none() {
        // The post-dispatch translate hook is the only writer of
        // `ExtractedDocument.translated_text` + `translated_to_lang`.
        // With no `translate_to` configured, both must stay None so
        // downstream (Phase 8b's LanceDB write path) can use them as
        // a "should we write the new column?" sentinel.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("hello.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let doc = extract_text_from_path(&p).unwrap();
        assert!(doc.translated_text.is_none());
        assert!(doc.translated_to_lang.is_none());
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
    fn extract_text_with_opts_no_ocr_returns_l2_exif_only() {
        // P13.7 Step 1+3 — semantic change.  Pre-P13.7, `try_ocr=false`
        // + an image extension returned an error ("no extractor").
        // The new dispatcher treats this as the L2-EXIF-only path:
        // returns Ok(doc) with full_text = "" so bg_ingest still
        // writes the image_* LanceDB columns.  Skip-entirely now
        // requires explicit opt-out via `image_extraction_enabled=
        // false` or `ingest_image_level="l1"` — see the next test.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("scan.png");
        std::fs::write(&p, b"\x89PNG").unwrap(); // fake bytes; ext drives dispatch
        let opts = ExtractOptions { try_ocr: false, ..Default::default() };
        let doc = extract_text_from_path_with_opts(&p, opts)
            .expect("L2 EXIF-only path returns Ok with empty text");
        assert!(doc.full_text.is_empty(), "no OCR → no text");
        // image_exif will be None for these fake bytes (no real EXIF
        // header), but the doc is still valid — the L2 contract only
        // requires Ok(_), not necessarily populated EXIF.
    }

    #[test]
    fn extract_text_with_opts_image_l1_skips_entirely() {
        // P13.7 Step 1+3 — explicit opt-out path: ingest_image_level
        // = "l1" short-circuits the dispatcher with an explicit
        // "skipped at L1 by Settings" error.  bg_ingest reads this
        // error and downgrades the row to L1 metadata-only.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("scan.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        let opts = ExtractOptions {
            ingest_image_level: "l1".to_string(),
            ..Default::default()
        };
        let err = extract_text_from_path_with_opts(&p, opts)
            .expect_err("L1 must skip image extraction with an error");
        assert!(
            err.to_string().contains("skipped at L1"),
            "error must reference L1: {err}"
        );
    }

    #[test]
    fn extract_text_with_opts_image_master_switch_off_skips_entirely() {
        // P13.7 Step 3 — master switch.  `image_extraction_enabled=
        // false` is the user-facing toggle in Settings → Multimodal;
        // it takes the same "skipped at L1 by Settings" exit as the
        // L1 path above.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("scan.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        let opts = ExtractOptions {
            image_extraction_enabled: false,
            ..Default::default()
        };
        let err = extract_text_from_path_with_opts(&p, opts)
            .expect_err("master switch off must skip image extraction");
        assert!(
            err.to_string().contains("skipped at L1"),
            "error must reference L1: {err}"
        );
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
