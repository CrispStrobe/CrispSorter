//! Model license-consent gate.
//!
//! A handful of downloadable models are **non-commercial** (CC-BY-NC-4.0) or
//! **use-restricted** (Gemma Terms of Use).  This module refuses to download or
//! load them unless the operator has explicitly accepted the license, via one of:
//!
//!   * env `CRISPSORTER_ACCEPT_MODEL_LICENSE` = `1` / `true` / `all` (blanket),
//!     or a comma-separated list of consent keys (e.g. `jina-v5-nano,…`);
//!   * the CLI `--accept-license` flag (calls [`accept_all`]);
//!   * the GUI confirmation dialog (calls [`accept`] per model key, persisted +
//!     replayed on startup).
//!
//! Permissive models (MIT/Apache/…) never trigger the gate.  Registry names
//! that are not on the reviewed list are [`ModelLicense::Unknown`] and are
//! refused unconditionally — blanket or per-model consent cannot authorize a
//! name whose license nobody has audited.  Enforcement lives at the shared
//! download choke points (`Embedder::new`, `Reranker::load`, the
//! `embedder_download_registry_model` Tauri command) so it covers every backend
//! (GGUF/CrispEmbed, ONNX/fastembed, direct-HF) and both front-ends.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

const ENV: &str = "CRISPSORTER_ACCEPT_MODEL_LICENSE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelLicense {
    /// MIT / Apache-2.0 / BSD / … — no consent required.
    Permissive,
    /// CC-BY-NC and friends — commercial use prohibited without a separate grant.
    NonCommercial(&'static str),
    /// Custom use-restricted terms (e.g. Gemma) — commercial OK but policy-bound.
    Restricted(&'static str),
    /// No reviewed license is available; consent cannot authorize this model.
    Unknown,
}

impl ModelLicense {
    pub fn requires_consent(self) -> bool {
        !matches!(self, ModelLicense::Permissive)
    }
    pub fn label(self) -> &'static str {
        match self {
            ModelLicense::Permissive => "permissive",
            ModelLicense::NonCommercial(s) | ModelLicense::Restricted(s) => s,
            ModelLicense::Unknown => "unknown license",
        }
    }
}

fn accepted() -> &'static Mutex<HashSet<String>> {
    static A: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashSet::new()))
}

fn blanket() -> &'static Mutex<bool> {
    static B: OnceLock<Mutex<bool>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(false))
}

/// Accept every restrictive license for the lifetime of the process
/// (CLI `--accept-license`).
pub fn accept_all() {
    *blanket().lock().unwrap() = true;
}

/// Accept a single model's license by its consent key (GUI confirmation).
pub fn accept(key: &str) {
    if !key.is_empty() {
        accepted().lock().unwrap().insert(key.to_string());
    }
}

fn env_accepts(key: &str) -> bool {
    match std::env::var(ENV) {
        Ok(v) => {
            let v = v.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("all")
                || v.split(',').any(|x| x.trim().eq_ignore_ascii_case(key))
        }
        Err(_) => false,
    }
}

pub fn is_accepted(key: &str) -> bool {
    *blanket().lock().unwrap() || env_accepts(key) || accepted().lock().unwrap().contains(key)
}

/// The gate. `Ok(())` when the license is permissive or consent is on record;
/// otherwise an actionable error naming the model + how to accept.
pub fn ensure(display: &str, key: &str, lic: ModelLicense) -> anyhow::Result<()> {
    if lic == ModelLicense::Unknown {
        anyhow::bail!("Model \"{display}\" has an unknown or unreviewed license; it cannot be downloaded or loaded until the registry is reviewed.");
    }
    if !lic.requires_consent() || is_accepted(key) {
        return Ok(());
    }
    anyhow::bail!(
        "Model \"{display}\" carries a {} license that must be accepted before \
         download/use. Accept it by setting {ENV}=1 (or {ENV}=\"{key}\"), passing \
         --accept-license on the CLI, or confirming the prompt in the GUI.",
        lic.label()
    )
}

/// Reviewed exact names from the linked CrispEmbed `model_mgr.cpp` registry,
/// plus the GUI and typed-model keys used by this application. Keep this list
/// in sync when the linked registry changes; unreviewed names must fail closed.
/// An upstream `other*` license is deliberately not treated as permissive.
pub fn license_for_registry_name(name: &str) -> ModelLicense {
    match name {
        "jina-reranker-v2-base-multilingual"
        | "jina-reranker-v2-base-multilingual-q4k"
        | "jina-v3"
        | "jina-v5-nano"
        | "jina-v5-nano-iq4xs"
        | "jina-v5-nano-q4k"
        | "jina-v5-nano-q8"
        | "jina-v5-small"
        | "jina-v5-small-iq4xs"
        | "jina-v5-small-q4k"
        | "jina-v5-small-q8" => ModelLicense::NonCommercial("CC-BY-NC-4.0"),
        "splade-v3" | "splade-v3-q8" => ModelLicense::NonCommercial("CC-BY-NC-SA-4.0"),
        "posformer-crohme" => ModelLicense::NonCommercial("CC-BY-NC-SA-3.0"),
        "embedding-gemma300-m"
        | "embeddinggemma-300m"
        | "embeddinggemma-300m-iq4xs"
        | "embeddinggemma-300m-q4k"
        | "embeddinggemma-300m-q8"
        | "embeddinggemma-300m-qat"
        | "gemma3-embed-2b" => ModelLicense::Restricted("Gemma Terms of Use"),
        "gliner-lfm"
        | "gliner-lfm-q4k"
        | "lfm2-colbert"
        | "lfm2-colbert-q4k"
        | "lfm2-embed"
        | "lfm2-embed-iq4xs"
        | "lfm2-embed-q4k"
        | "lfm2-embed-q8"
        | "sauerkraut-gliner-lfm" => ModelLicense::Restricted("LFM Open License v1.0"),
        "texo-distill" => ModelLicense::Restricted("AGPL-3.0"),
        "surya-det" => ModelLicense::Restricted("OpenRAIL-M"),
        // CrispEmbed's licence audit (README "VLM OCR engines' upstream
        // licences", verified against the base chain): Qwen2.5-VL-3B
        // derivatives are Qwen Research and GOT-OCR2's decoder is Tongyi
        // Qianwen Research, both non-commercial. v0.17.12's registry still
        // labels them permissive. `qwen2vl-ocr` is this app's alias for
        // `qwen2vl-3b`. The Apache-2.0 Qwen2-VL-2B family (`german-ocr-3.1`,
        // `nanonets-ocr2-1.5b`) is unaffected.
        "qwen2vl-3b"
        | "qwen2vl-ocr"
        | "nanonets-ocr-s"
        | "uni-mumer-qwen2.5-vl-3b"
        | "teleocr-f16"
        | "teleocr-q4_k-m"
        | "teleocr-q4_k-mix"
        | "teleocr-q8_0" => ModelLicense::NonCommercial("Qwen Research License"),
        "got-ocr2"
        | "got-ocr2-f16"
        | "got-ocr2-q4_k-experimental"
        | "got-ocr2-q8-q4k-mix-experimental"
        | "got-ocr2-q8_0-experimental" => ModelLicense::NonCommercial("Tongyi Qianwen Research License"),
        "adair-5d"
        | "all-MiniLM-L12-v2"
        | "all-MiniLM-L12-v2-iq4xs"
        | "all-MiniLM-L12-v2-q4k"
        | "all-MiniLM-L12-v2-q8"
        | "all-MiniLM-L6-v2"
        | "all-MiniLM-L6-v2-iq4xs"
        | "all-MiniLM-L6-v2-q4k"
        | "all-MiniLM-L6-v2-q8"
        | "all-mini-lm-l6-v2"
        | "all-mpnet-base-v2"
        | "all-mpnet-base-v2-iq4xs"
        | "all-mpnet-base-v2-q4k"
        | "all-mpnet-base-v2-q8"
        | "arctic-embed-l-v2"
        | "arctic-embed-l-v2-iq4xs"
        | "arctic-embed-l-v2-q4k"
        | "arctic-embed-l-v2-q8"
        | "arctic-embed-xs"
        | "arctic-embed-xs-iq4xs"
        | "arctic-embed-xs-q4k"
        | "arctic-embed-xs-q8"
        | "auraface-v1"
        | "bert-base-ner"
        | "bge-base-en-v1.5"
        | "bge-base-en-v1.5-iq4xs"
        | "bge-base-en-v1.5-q4k"
        | "bge-base-en-v1.5-q8"
        | "bge-base-en-v15"
        | "bge-large-en-v1.5"
        | "bge-large-en-v1.5-iq4xs"
        | "bge-large-en-v1.5-q4k"
        | "bge-large-en-v1.5-q8"
        | "bge-large-en-v15"
        | "bge-m3"
        | "bge-m3-iq4xs"
        | "bge-m3-q4k"
        | "bge-m3-q8"
        | "bge-reranker-base"
        | "bge-reranker-v2-m3"
        | "bge-small-en-v1.5"
        | "bge-small-en-v1.5-iq4xs"
        | "bge-small-en-v1.5-q4k"
        | "bge-small-en-v1.5-q8"
        | "bge-small-en-v15"
        | "bidirlm-omni-2.5b"
        | "bidirlm-omni-2.5b-mm"
        | "bidirlm-omni-2.5b-textonly"
        | "bidirlm-omni-2.5b-textonly-q4k"
        | "bttr-hw"
        | "cld3"
        | "clip-text-base"
        | "clip-text-base-f32"
        | "clip-text-base-iq4xs"
        | "clip-text-base-q8"
        | "clip-text-large"
        | "clip-text-large-f32"
        | "clip-text-large-iq4xs"
        | "clip-text-large-q8"
        | "clip-vit-base-patch16"
        | "clip-vit-large-patch14"
        | "clip-vit-large-patch14-336"
        | "dat-sr-x2"
        | "dbnet-det"
        | "deberta-v2-xlarge"
        | "deepseek-ocr2"
        | "esrgan-x4"
        | "f2llm-v2-0.6b"
        | "f2llm-v2-0.6b-iq4xs"
        | "f2llm-v2-0.6b-q4k"
        | "f2llm-v2-0.6b-q8"
        | "firered-ocr"
        | "firered-ocr-q4k"
        | "fireredpunc"
        | "fireredpunc-iq4xs"
        | "fireredpunc-q4k"
        | "fireredpunc-q8"
        | "flova"
        | "flova-q4k"
        | "flova-q8"
        | "fullstop-punc"
        | "fullstop-punc-q8"
        | "german-ocr-3.1"
        | "gliner-deberta"
        | "gliner-deberta-q4k"
        | "glm-ocr"
        | "glotlid"
        | "granite-embedding-107m"
        | "granite-embedding-278m"
        | "granite-vision"
        | "granite-vision-q4k"
        | "gte-base-en-v1.5"
        | "gte-base-en-v1.5-iq4xs"
        | "gte-base-en-v1.5-q4k"
        | "gte-base-en-v1.5-q8"
        | "gte-base-en-v15"
        | "gte-large-en-v1.5"
        | "gte-large-en-v1.5-iq4xs"
        | "gte-large-en-v1.5-q4k"
        | "gte-large-en-v1.5-q8"
        | "gte-large-en-v15"
        | "gte-modernbert-base"
        | "gte-small"
        | "gte-small-iq4xs"
        | "gte-small-q4k"
        | "gte-small-q8"
        | "h2ovl-mississippi-2b"
        | "h2ovl-mississippi-800m"
        | "harrier-0.6b"
        | "harrier-0.6b-iq4xs"
        | "harrier-0.6b-q4k"
        | "harrier-0.6b-q8"
        | "harrier-270m"
        | "harrier-270m-iq4xs"
        | "harrier-270m-q4k"
        | "harrier-270m-q8"
        | "hat-sr-x4"
        | "hmer-hw"
        | "instructir"
        | "internvl2-1b"
        | "internvl2-2b"
        | "layout-heron"
        | "lightonocr"
        | "lilt-base"
        | "lilt-funsd"
        | "lilt-funsd-f32"
        | "lilt-funsd-iq4xs"
        | "lilt-funsd-q8"
        | "mixtex-zhen"
        | "modernbert-base"
        | "modernbert-large"
        | "ms-marco-MiniLM-L-12-v2"
        | "ms-marco-MiniLM-L-12-v2-iq4xs"
        | "ms-marco-MiniLM-L-6-v2"
        | "ms-marco-MiniLM-L-6-v2-iq4xs"
        | "multilingual-e5-base"
        | "multilingual-e5-base-iq4xs"
        | "multilingual-e5-base-q4k"
        | "multilingual-e5-base-q8"
        | "multilingual-e5-large"
        | "multilingual-e5-large-f32"
        | "multilingual-e5-large-iq4xs"
        | "multilingual-e5-large-q4k"
        | "multilingual-e5-large-q8"
        | "multilingual-e5-small"
        | "multilingual-e5-small-iq4xs"
        | "multilingual-e5-small-q4k"
        | "multilingual-e5-small-q8"
        | "multilingual-mini-lm"
        | "mxbai-embed-large-v1"
        | "mxbai-embed-large-v1-iq4xs"
        | "mxbai-embed-large-v1-q4k"
        | "mxbai-embed-large-v1-q8"
        | "mxbai-rerank-base-v1"
        | "mxbai-rerank-xsmall-v1"
        | "nafnet-denoise"
        | "nanonets-ocr2-1.5b"
        | "nomic-bert-moe"
        | "nomic-embed-text-v1.5"
        | "nomic-embed-text-v1.5-iq4xs"
        | "nomic-embed-text-v1.5-q4k"
        | "nomic-embed-text-v1.5-q8"
        | "nomic-embed-text-v15"
        | "nomic-embed-text-v2-moe"
        | "nomic-embed-text-v2-moe-iq4xs"
        | "nomic-embed-text-v2-moe-q4k"
        | "nomic-embed-text-v2-moe-q8"
        | "octen-0.6b"
        | "octen-0.6b-iq4xs"
        | "octen-0.6b-q4k"
        | "octen-0.6b-q8"
        | "octen-06b-int8-local"
        | "octen-4b"
        | "octen-4b-iq4xs"
        | "octen-4b-q4k"
        | "octen-4b-q8"
        | "octen-8b"
        | "octen-8b-iq4xs"
        | "octen-8b-q4k"
        | "octen-8b-q8"
        | "paddleocr-vl"
        | "paddleocr-vl-1.6"
        | "paddleocr-vl-1.6-q4k"
        | "paddleocr-vl-q4k"
        | "pan-x4"
        | "paraphrase-multilingual-MiniLM-L12-v2"
        | "paraphrase-multilingual-MiniLM-L12-v2-iq4xs"
        | "paraphrase-multilingual-MiniLM-L12-v2-q4k"
        | "paraphrase-multilingual-MiniLM-L12-v2-q8"
        | "parseq"
        | "parseq-tiny"
        | "pcs"
        | "pcs-iq4xs"
        | "pcs-q4k"
        | "pcs-q8"
        | "pix2struct-base"
        | "pix2tex-mfr"
        | "pixie-rune-v1"
        | "pixie-rune-v1-int4"
        | "pixie-rune-v1-int4-full"
        | "pixie-rune-v1-iq4xs"
        | "pixie-rune-v1-q"
        | "pixie-rune-v1-q4k"
        | "pixie-rune-v1-q8"
        | "ppformulanet-l"
        // PP-OCRv6 det/rec and EasyOCR English Gen2 are apache-2.0 in the pinned
        // registry (`model_mgr.cpp`), and both engines are offered in Settings.
        | "easyocr-english-g2"
        | "ppocrv6-tiny-det"
        | "ppocrv6-tiny-rec"
        | "qari-ocr"
        | "qwen3-embed-0.6b"
        | "qwen3-embed-0.6b-iq4xs"
        | "qwen3-embed-0.6b-q4k"
        | "qwen3-embed-0.6b-q8"
        | "qwen3-embed-4b"
        | "qwen3-embed-4b-iq4xs"
        | "qwen3-embed-4b-q4k"
        | "qwen3-embed-4b-q8"
        | "qwen3-embed-8b"
        | "qwen3-embed-8b-iq4xs"
        | "qwen3-embed-8b-q4k"
        | "qwen3-embed-8b-q8"
        | "qwen3vl-2b"
        | "restormer-denoise"
        | "safmn-x4"
        | "scrfd-det-10g"
        | "scunet-color"
        | "sface"
        | "siglip-base"
        | "siglip-large-256"
        | "siglip-so400m-patch14-384"
        | "siglip-text-base"
        | "siglip-text-base-f32"
        | "siglip-text-base-iq4xs"
        | "siglip-text-base-q8"
        | "smt-fp"
        | "smt-grandstaff"
        | "snowflake-arctic-embed-l"
        | "snowflake-arctic-embed-l-iq4xs"
        | "snowflake-arctic-embed-l-q4k"
        | "snowflake-arctic-embed-l-q8"
        | "snowflake-arctic-embed-m"
        | "snowflake-arctic-embed-m-iq4xs"
        | "snowflake-arctic-embed-m-q4k"
        | "snowflake-arctic-embed-m-q8"
        | "snowflake-arctic-lv2"
        | "snowflake-arctic-lv2-fp16"
        | "snowflake-arctic-lv2-fp32"
        | "snowflake-arctic-lv2-int8"
        | "snowflake-arctic-lv2-o4"
        | "snowflake-arctic-lv2-q4"
        | "snowflake-arctic-lv2-q4-f16"
        | "splade-pp-en-v1"
        | "swinir-sr-x4"
        | "tbsrn-telescope"
        | "tesseract-ara"
        | "tesseract-chi-sim"
        | "tesseract-deu"
        | "tesseract-eng"
        | "tesseract-fra"
        | "tesseract-ita"
        | "tesseract-jpn"
        | "tesseract-kor"
        | "tesseract-nld"
        | "tesseract-por"
        | "tesseract-rus"
        | "tesseract-spa"
        | "text-sr"
        | "texteller-3"
        | "tps-loc"
        | "transcoda"
        | "trocr-printed"
        | "tromr"
        | "uni-mumer-qwen3-vl-2b"
        | "unlimited-ocr"
        | "yunet" => ModelLicense::Permissive,
        _ => ModelLicense::Unknown,
    }
}

pub fn ensure_for_registry_name(name: &str) -> anyhow::Result<()> {
    ensure(name, name, license_for_registry_name(name))
}

/// True when `name` is a registry entry rather than a path that is already on
/// disk. A path implies the file was placed there deliberately; a registry name
/// implies a download, which is what the consent gate exists to authorise.
pub fn is_registry_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.to_ascii_lowercase().ends_with(".gguf")
}

/// Gate a name that is about to trigger an implicit download.
///
/// The GUI's download command is already gated; the lazy initialisers (OCR
/// stages, layout, formula OCR) call `resolve_model(name, Some(true))` straight
/// from Rust, which downloads on demand. Without this, selecting a restricted
/// registry entry there would fetch restricted weights with no consent.
pub fn ensure_registry_download_allowed(name: &str) -> anyhow::Result<()> {
    if !is_registry_name(name) {
        return Ok(());
    }
    ensure(name, name, license_for_registry_name(name))
}

/// `true` when resolving `name` may download. Paths always may; registry names
/// only with consent. Callers keep their existing fallback ("use the literal
/// string"), so a refused name makes the model load fail instead of fetching.
pub fn may_download(name: &str) -> bool {
    ensure_registry_download_allowed(name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_never_gated() {
        assert!(ensure("BGE-M3", "", ModelLicense::Permissive).is_ok());
        assert!(!ModelLicense::Permissive.requires_consent());
    }

    #[test]
    fn restrictive_blocked_until_accepted() {
        // The blanket env var is process-wide and checked by `ensure`; clear it so
        // this test asserts the same behavior whatever the caller's environment.
        std::env::remove_var(ENV);
        let key = "test-nc-model-unique-key";
        let lic = ModelLicense::NonCommercial("CC-BY-NC-4.0");
        assert!(lic.requires_consent());
        assert!(ensure("Test NC", key, lic).is_err());
        accept(key);
        assert!(ensure("Test NC", key, lic).is_ok());
    }

    #[test]
    fn registry_name_licenses() {
        assert!(license_for_registry_name("jina-v5-nano").requires_consent());
        assert!(license_for_registry_name("jina-reranker-v2-base-multilingual").requires_consent());
        assert!(license_for_registry_name("embedding-gemma300-m").requires_consent());
        assert!(license_for_registry_name("sauerkraut-gliner-lfm").requires_consent());
        assert!(!license_for_registry_name("gliner-deberta").requires_consent());
        assert!(!license_for_registry_name("bge-m3").requires_consent());
        assert!(!license_for_registry_name("pixie-rune-v1").requires_consent());
    }

    #[test]
    fn research_only_vlm_ocr_weights_require_consent() {
        for name in ["qwen2vl-3b", "qwen2vl-ocr", "nanonets-ocr-s", "uni-mumer-qwen2.5-vl-3b",
                     "teleocr-q8_0", "got-ocr2", "got-ocr2-f16"] {
            assert!(matches!(license_for_registry_name(name), ModelLicense::NonCommercial(_)), "{name}");
        }
        // The Apache-2.0 Qwen2-VL-2B fine-tunes stay usable without consent.
        assert!(!license_for_registry_name("german-ocr-3.1").requires_consent());
        assert!(!license_for_registry_name("nanonets-ocr2-1.5b").requires_consent());
    }

    #[test]
    fn ocr_and_enhancement_defaults_never_trip_the_download_gate() {
        // These are the literal names the lazy OCR / enhancement initialisers
        // resolve. If one of them becomes restricted or unreviewed, OCR stops
        // working for every user without a consent prompt, so pin the
        // expectation here rather than discovering it in the field.
        for name in [
            "dbnet-det",
            "german-ocr-3.1",
            "tesseract-eng",
            "parseq",
            "lilt-funsd",
            "tps-loc",
            "esrgan-x4",
            "safmn-x4",
            "hat-sr-x4",
            "tbsrn-telescope",
            "dat-sr-x2",
            "swinir-sr-x4",
            "pan-x4",
            "scunet-color",
            "instructir",
            "restormer-denoise",
            "ppocrv6-tiny-det",
            "ppocrv6-tiny-rec",
            "easyocr-english-g2",
            "nafnet-denoise",
            "yunet",
            "layout-heron",
            "siglip-base",
            "bidirlm-omni-2.5b",
            "ppformulanet-l",
        ] {
            assert!(
                may_download(name),
                "{name} is an OCR/enhancement default, so it must stay downloadable"
            );
        }
    }

    #[test]
    fn only_bare_names_are_treated_as_registry_entries() {
        assert!(is_registry_name("surya-det"));
        assert!(!is_registry_name("/models/ocr/tesseract-eng.gguf"));
        assert!(!is_registry_name("C:\\models\\ocr\\tesseract-eng.gguf"));
        assert!(!is_registry_name(""));
        // A file the user placed on disk is not a download.
        assert!(may_download("/models/ocr/surya-det.gguf"));
        // Restricted names still carry their class through the gate.
        assert!(license_for_registry_name("surya-det").requires_consent());
        assert!(license_for_registry_name("posformer-crohme").requires_consent());
    }
}
