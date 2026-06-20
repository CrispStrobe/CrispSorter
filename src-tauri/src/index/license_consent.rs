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
        _ => ModelLicense::Permissive,
    }
}

pub fn ensure_for_registry_name(name: &str) -> anyhow::Result<()> {
    ensure(name, name, license_for_registry_name(name))
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
