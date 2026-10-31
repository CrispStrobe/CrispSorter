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
//! Permissive models (MIT/Apache/…) never trigger the gate.  Enforcement lives
//! at the shared download choke points (`Embedder::new`, `Reranker::load`, the
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
}

impl ModelLicense {
    pub fn requires_consent(self) -> bool {
        !matches!(self, ModelLicense::Permissive)
    }
    pub fn label(self) -> &'static str {
        match self {
            ModelLicense::Permissive => "permissive",
            ModelLicense::NonCommercial(s) | ModelLicense::Restricted(s) => s,
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

/// License lookup for a bare registry / override name — used by the
/// load-by-name and GUI registry-download paths where the `EmbedderModel`
/// enum variant isn't available. Keys mirror `gguf_registry_name()` and the
/// GUI's `indexEmbedderToRust` mapping.
pub fn license_for_registry_name(name: &str) -> ModelLicense {
    match name {
        "jina-v3" | "jina-v5-small" | "jina-v5-nano" | "jina-reranker-v2-base-multilingual" => {
            ModelLicense::NonCommercial("CC-BY-NC-4.0")
        }
        "embedding-gemma300-m" | "embeddinggemma-300m" => {
            ModelLicense::Restricted("Gemma Terms of Use")
        }
        // GLiNER NER (index::ner) — the German-tuned LFM model ships under the
        // LFM Open License v1.0 (restricted); the DeBERTa GGUF is Apache-2.0.
        // LFM2.5 embeddings/ColBERT from CrispEmbed also use LFM Open License.
        "sauerkraut-gliner-lfm" | "gliner-lfm" | "gliner-lfm-q4k"
        | "lfm2-embed" | "lfm2-embed-q4k" | "lfm2-colbert" | "lfm2-colbert-q4k" => {
            ModelLicense::Restricted("LFM Open License v1.0")
        }
        // OCR models whose weights inherit non-commercial terms (see
        // `RESTRICTED_MODELS` below).
        other => match restricted_model(other) {
            Some(m) => ModelLicense::NonCommercial(m.terms.name),
            None => ModelLicense::Permissive,
        },
    }
}

pub fn ensure_for_registry_name(name: &str) -> anyhow::Result<()> {
    if let Some(m) = restricted_model(name) {
        return ensure_restricted_model(name, m);
    }
    ensure(name, name, license_for_registry_name(name))
}

// ── Inherited non-commercial terms (CrispEmbed OCR registry audit) ──────────
//
// CrispEmbed's license audit (CrispStrobe/CrispEmbed#92, evidence
// `tests/results/licenses/registry-qwen-audit.json`) found registry models that
// were labelled Apache-2.0 / CC-BY-4.0 but whose weights inherit non-commercial
// terms. The CrispEmbed runtime CrispSorter pins still carries the old labels
// and has no consent gate of its own, so CrispSorter enforces consent here,
// before any download or load. Matching is by registry alias, by registry
// filename, and by filename stem for local paths (case-insensitive).

/// Full terms of one restrictive license, as shown to the user.
#[derive(Debug, PartialEq, Eq)]
pub struct LicenseTerms {
    /// Short machine tag (matches CrispEmbed's registry `license` field).
    pub tag: &'static str,
    /// The license's own name.
    pub name: &'static str,
    /// Where the full text lives.
    pub url: &'static str,
    /// Plain-language summary of the obligations the user acknowledges.
    pub notice: &'static str,
    /// CrispEmbed runtime gate to set once the user has accepted (newer
    /// runtimes refuse to load the model without it; older ones ignore it).
    pub runtime_env: Option<&'static str>,
}

pub const QWEN_RESEARCH: LicenseTerms = LicenseTerms {
    tag: "qwen-research",
    name: "Qwen RESEARCH LICENSE AGREEMENT",
    url: "https://huggingface.co/Qwen/Qwen2.5-VL-3B-Instruct/blob/main/LICENSE",
    notice: "Non-commercial use only (research / evaluation). Commercial use needs a \
             separate license from Alibaba Cloud.",
    runtime_env: Some("CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE"),
};

pub const TONGYI_QIANWEN_RESEARCH: LicenseTerms = LicenseTerms {
    tag: "tongyi-qianwen-research",
    name: "Tongyi Qianwen RESEARCH LICENSE AGREEMENT",
    url: "https://huggingface.co/Qwen/Qwen1.5-0.5B/blob/main/LICENSE",
    notice: "Non-commercial use only. In addition, neither the model nor any output \
             from it may be used to improve any other large language model.",
    runtime_env: Some("CRISPEMBED_ACCEPT_TONGYI_QIANWEN_LICENSE"),
};

pub const CC_BY_NC_4_0: LicenseTerms = LicenseTerms {
    tag: "cc-by-nc-4.0",
    name: "Creative Commons Attribution-NonCommercial 4.0 (CC-BY-NC-4.0)",
    url: "https://creativecommons.org/licenses/by-nc/4.0/",
    notice: "Non-commercial use only; attribution required.",
    runtime_env: None,
};

/// One affected model family.
#[derive(Debug, PartialEq, Eq)]
pub struct RestrictedModel {
    /// Consent key (the canonical CrispEmbed registry alias).
    pub key: &'static str,
    /// Human-readable model name.
    pub display: &'static str,
    /// Why the restrictive terms apply.
    pub reason: &'static str,
    pub terms: &'static LicenseTerms,
    /// Registry aliases (exact match).
    aliases: &'static [&'static str],
    /// Lower-case filename stems matched as substrings of a path's basename.
    stems: &'static [&'static str],
}

static RESTRICTED_MODELS: &[RestrictedModel] = &[
    RestrictedModel {
        key: "qwen2vl-3b",
        display: "Qwen2.5-VL-3B-Instruct",
        reason: "is Qwen2.5-VL-3B-Instruct",
        terms: &QWEN_RESEARCH,
        // `qwen2vl-ocr` is the legacy CrispSorter/CrispEmbed default name; it
        // never resolved in the pinned registry, but it names this model.
        aliases: &["qwen2vl-3b", "qwen2vl-ocr", "qwen2.5-vl-3b-q4_k.gguf"],
        stems: &["qwen2.5-vl-3b", "qwen2_5-vl-3b", "qwen2.5vl-3b"],
    },
    RestrictedModel {
        key: "nanonets-ocr-s",
        display: "Nanonets-OCR-s",
        reason: "is a fine-tune of Qwen2.5-VL-3B-Instruct",
        terms: &QWEN_RESEARCH,
        aliases: &["nanonets-ocr-s", "nanonets-ocr-s-q4_k.gguf"],
        stems: &["nanonets-ocr-s"],
    },
    RestrictedModel {
        key: "uni-mumer-qwen2.5-vl-3b",
        display: "Uni-MuMER (Qwen2.5-VL-3B)",
        reason: "is a fine-tune of Qwen2.5-VL-3B-Instruct",
        terms: &QWEN_RESEARCH,
        aliases: &["uni-mumer-qwen2.5-vl-3b", "uni-mumer-qwen2.5-vl-3b-q4_k.gguf"],
        stems: &["uni-mumer-qwen2.5-vl-3b"],
    },
    RestrictedModel {
        key: "teleocr",
        display: "TeleOCR",
        reason: "uses the Qwen2.5-VL-3B vision tower",
        terms: &QWEN_RESEARCH,
        aliases: &["teleocr", "teleocr-f16", "teleocr-q8_0", "teleocr-q4_k-m"],
        stems: &["teleocr"],
    },
    RestrictedModel {
        key: "got-ocr2",
        display: "GOT-OCR2",
        reason: "its decoder is Qwen1.5-0.5B",
        terms: &TONGYI_QIANWEN_RESEARCH,
        aliases: &["got-ocr2", "got-ocr2-q4_k.gguf"],
        stems: &["got-ocr2", "got_ocr2", "got-ocr-2"],
    },
    RestrictedModel {
        key: "transcoda",
        display: "Transcoda-59M (OMR)",
        reason: "its encoder is fine-tuned from the CC-BY-NC ConvNeXt V2 ImageNet weights",
        terms: &CC_BY_NC_4_0,
        aliases: &["transcoda", "transcoda-q8_0.gguf"],
        stems: &["transcoda"],
    },
];

/// The affected model family a registry alias, registry filename or local
/// GGUF path refers to, if any.
pub fn restricted_model(name_or_path: &str) -> Option<&'static RestrictedModel> {
    let raw = name_or_path.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if let Some(m) = RESTRICTED_MODELS
        .iter()
        .find(|m| m.aliases.iter().any(|a| *a == lower))
    {
        return Some(m);
    }
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    RESTRICTED_MODELS
        .iter()
        .find(|m| m.stems.iter().any(|s| base.contains(s)))
}

/// Every affected model family (for listings / notices).
pub fn restricted_models() -> &'static [RestrictedModel] {
    RESTRICTED_MODELS
}

/// The license tag to show for a registry entry: our corrected tag for an
/// affected model, else whatever the registry declares.
pub fn display_license_tag(name: &str, filename: &str, registry_tag: &str) -> String {
    restricted_model(name)
        .or_else(|| restricted_model(filename))
        .map(|m| m.terms.tag.to_string())
        .unwrap_or_else(|| registry_tag.to_string())
}

/// The acknowledgement text shown before consent (GUI dialog, CLI error).
pub fn acknowledgement_text(m: &RestrictedModel) -> String {
    format!(
        "{} {} and is therefore governed by the {} ({}). {} Full license: {}",
        m.display, m.reason, m.terms.name, m.terms.tag, m.terms.notice, m.terms.url
    )
}

/// Consent for an affected model is recorded under its family key, but the
/// GUI and env may name any alias — accept either.
pub fn restricted_accepted(m: &RestrictedModel, requested: &str) -> bool {
    is_accepted(m.key)
        || is_accepted(requested)
        || m.aliases.iter().any(|a| is_accepted(a))
}

/// Gate for one affected model. On acceptance it also sets the matching
/// CrispEmbed runtime env gate so the load keeps working on runtimes that
/// enforce it themselves.
pub fn ensure_restricted_model(requested: &str, m: &RestrictedModel) -> anyhow::Result<()> {
    if !restricted_accepted(m, requested) {
        anyhow::bail!(
            "Model \"{requested}\" requires license acknowledgement before download/use. {} \
             Accept it by confirming the prompt in the GUI, setting {ENV}=\"{}\" (or {ENV}=1), \
             or passing --accept-license on the CLI.",
            acknowledgement_text(m),
            m.key
        );
    }
    if let Some(var) = m.terms.runtime_env {
        // Edition 2021: `set_var` is safe. Set before the CrispEmbed context
        // for this model is created, which is always after this gate.
        std::env::set_var(var, "1");
    }
    Ok(())
}

/// Gate for any OCR/registry model name or path: `Ok` for unaffected models.
pub fn ensure_model_name(name_or_path: &str) -> anyhow::Result<()> {
    match restricted_model(name_or_path) {
        Some(m) => ensure_restricted_model(name_or_path, m),
        None => Ok(()),
    }
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
        let key = "test-nc-model-unique-key";
        let lic = ModelLicense::NonCommercial("CC-BY-NC-4.0");
        assert!(lic.requires_consent());
        assert!(ensure("Test NC", key, lic).is_err());
        accept(key);
        assert!(ensure("Test NC", key, lic).is_ok());
    }

    #[test]
    fn affected_ocr_models_are_restricted() {
        for (name, tag) in [
            ("qwen2vl-3b", "qwen-research"),
            ("qwen2vl-ocr", "qwen-research"),
            ("nanonets-ocr-s", "qwen-research"),
            ("uni-mumer-qwen2.5-vl-3b", "qwen-research"),
            ("teleocr-f16", "qwen-research"),
            ("teleocr-q8_0", "qwen-research"),
            ("teleocr-q4_k-m", "qwen-research"),
            ("got-ocr2", "tongyi-qianwen-research"),
            ("transcoda", "cc-by-nc-4.0"),
            // registry filenames and local paths
            ("qwen2.5-vl-3b-q4_k.gguf", "qwen-research"),
            ("/models/cache/Nanonets-OCR-s-Q4_K.gguf", "qwen-research"),
            ("C:\\models\\got-ocr2-q4_k.gguf", "tongyi-qianwen-research"),
            ("/x/transcoda-q8_0.gguf", "cc-by-nc-4.0"),
        ] {
            let m = restricted_model(name).unwrap_or_else(|| panic!("{name} not restricted"));
            assert_eq!(m.terms.tag, tag, "{name}");
            assert!(license_for_registry_name(name).requires_consent(), "{name}");
        }
    }

    #[test]
    fn apache_alternatives_are_not_restricted() {
        for name in [
            "qwen3vl-2b", "firered-ocr", "uni-mumer-qwen3-vl-2b", "paddleocr-vl",
            "olmocr-2-7b", "olmocr-2-7b-q4_k.gguf", "german-ocr-3.1", "nanonets-ocr2-1.5b",
            "trocr-printed", "dbnet-det", "glm-ocr", "qari-ocr", "deepseek-ocr2",
            "/m/qwen3-vl-2b-q4_k.gguf", "",
        ] {
            assert!(restricted_model(name).is_none(), "{name} wrongly restricted");
            assert!(ensure_model_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn acknowledgement_names_license_terms_and_link() {
        let got = restricted_model("got-ocr2").unwrap();
        let t = acknowledgement_text(got);
        assert!(t.contains("Tongyi Qianwen RESEARCH LICENSE AGREEMENT"));
        assert!(t.contains("Non-commercial"));
        assert!(t.contains("improve any other large language model"));
        assert!(t.contains("https://huggingface.co/Qwen/Qwen1.5-0.5B/blob/main/LICENSE"));
        let q = acknowledgement_text(restricted_model("nanonets-ocr-s").unwrap());
        assert!(q.contains("Qwen RESEARCH LICENSE AGREEMENT") && q.contains("Non-commercial"));
        assert!(q.contains("https://huggingface.co/Qwen/Qwen2.5-VL-3B-Instruct/blob/main/LICENSE"));
    }

    #[test]
    fn display_tag_corrects_registry_labels() {
        assert_eq!(display_license_tag("got-ocr2", "got-ocr2-q4_k.gguf", "apache-2.0"), "tongyi-qianwen-research");
        assert_eq!(display_license_tag("transcoda", "transcoda-q8_0.gguf", "cc-by-4.0"), "cc-by-nc-4.0");
        assert_eq!(display_license_tag("qwen3vl-2b", "qwen3-vl-2b-q4_k.gguf", "apache-2.0"), "apache-2.0");
    }

    // One test owns the process-global consent + env state for the restricted
    // OCR models, so parallel tests cannot race on it.
    #[test]
    fn restricted_ocr_gate_blocks_then_sets_runtime_env() {
        std::env::remove_var(ENV);
        std::env::remove_var("CRISPEMBED_ACCEPT_TONGYI_QIANWEN_LICENSE");
        std::env::remove_var("CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE");

        let err = ensure_model_name("got-ocr2").unwrap_err().to_string();
        assert!(err.contains("Tongyi Qianwen RESEARCH LICENSE AGREEMENT"), "{err}");
        assert!(err.contains("improve any other large language model"), "{err}");
        assert!(ensure_for_registry_name("got-ocr2").is_err());
        assert!(std::env::var("CRISPEMBED_ACCEPT_TONGYI_QIANWEN_LICENSE").is_err());

        // A path to the same weights is gated under the same key.
        assert!(ensure_model_name("/cache/got-ocr2-q4_k.gguf").is_err());
        accept("got-ocr2");
        assert!(ensure_model_name("/cache/got-ocr2-q4_k.gguf").is_ok());
        assert_eq!(std::env::var("CRISPEMBED_ACCEPT_TONGYI_QIANWEN_LICENSE").as_deref(), Ok("1"));
        // Accepting GOT does not accept the Qwen research models.
        assert!(ensure_model_name("qwen2vl-3b").is_err());
        assert!(std::env::var("CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE").is_err());

        // Accepting one alias (as the GUI registry download does) covers the family.
        accept("teleocr-q8_0");
        assert!(ensure_model_name("/m/teleocr-f16.gguf").is_ok());
        assert_eq!(std::env::var("CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE").as_deref(), Ok("1"));
        std::env::remove_var("CRISPEMBED_ACCEPT_TONGYI_QIANWEN_LICENSE");
        std::env::remove_var("CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE");
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
}
