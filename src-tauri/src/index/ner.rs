//! Zero-shot named-entity recognition (GLiNER) via CrispEmbed.
//!
//! Mirrors `index::reranker`: a lazy-loaded GGUF model held behind an
//! `Arc<Mutex<Option<_>>>`, feature-gated on `crispembed`, soft-failing,
//! and routed through the [`index::license_consent`] gate at load.
//!
//! Wired into the ingest pipeline (`index::ingest`): when
//! `IndexConfig.ner_enabled` is on, every document's `full_text` (truncated
//! to `ner_max_chars`) runs through [`NerHandle::extract_tags`] once, and the
//! resulting `"<label>:<text>"` tags are merged into `RawDocument.tags` so
//! they land in the existing `tags` column.  This reuses the whole tag stack
//! — tag-cloud sidebar, `array_has(tags,…)` filter, `index search --tag`, and
//! the federated `--tag` path — with zero schema migration.
//!
//! Entity tags are namespaced with a compact, lowercased label prefix and the
//! original-case entity text, e.g. `person:Barack Obama`, `org:United
//! Nations`, `loc:Hawaii`, `date:2009`.
//!
//! Without the `crispembed` cargo feature this module compiles to stubs:
//! [`NerHandle::extract_tags`] returns an empty `Vec` (no-op), so ingest
//! behaviour is byte-identical to a build without NER.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "crispembed")]
use anyhow::Context;
#[cfg(feature = "crispembed")]
use hf_hub::api::tokio::ApiBuilder;
#[cfg(feature = "crispembed")]
use std::path::Path;

/// The GLiNER NER models offered in the Settings UI. GGUF-only — these are
/// CrispEmbed models loaded through `crispembed::CrispNER`. Mirrors the NER
/// slice of `CrispEmbed/examples/cli/model_mgr.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NerModel {
    /// cstr/sauerkraut-gliner-lfm-GGUF — LFM2.5-350M bidirectional, German-
    /// tuned, 5 languages. Default (fits the DE/EN corpus). ~419 MB Q8_0.
    /// Carries the LFM Open License v1.0 (restricted) → license-gated.
    #[default]
    SauerkrautGlinerLfm,
    /// cstr/gliner-deberta-GGUF — DeBERTa-v3-base 209M, English/multilingual,
    /// Apache-2.0 (permissive). ~198 MB Q8_0.
    GlinerDeberta,
}

impl NerModel {
    pub fn display_name(&self) -> &'static str {
        match self {
            NerModel::SauerkrautGlinerLfm => "Sauerkraut-GLiNER LFM (German-tuned, default)",
            NerModel::GlinerDeberta => "GLiNER DeBERTa-v3 (English/multilingual, Apache-2.0)",
        }
    }

    /// License class for the consent gate (`index::license_consent`).
    pub fn license(&self) -> crate::index::license_consent::ModelLicense {
        use crate::index::license_consent::ModelLicense::*;
        match self {
            // LFM Open License v1.0 — commercial use conditioned on revenue,
            // policy-bound; treat as restricted (consent required).
            NerModel::SauerkrautGlinerLfm => Restricted("LFM Open License v1.0"),
            // GLiNER-DeBERTa GGUF is Apache-2.0.
            NerModel::GlinerDeberta => Permissive,
        }
    }

    /// Consent key (matches the GUI's NER model mapping); empty when permissive.
    pub fn consent_key(&self) -> &'static str {
        match self {
            NerModel::SauerkrautGlinerLfm => "sauerkraut-gliner-lfm",
            NerModel::GlinerDeberta => "",
        }
    }

    /// Gate: errors unless permissive or consent is on record.
    pub fn ensure_license_consent(&self) -> Result<()> {
        crate::index::license_consent::ensure(
            self.display_name(),
            self.consent_key(),
            self.license(),
        )
    }

    /// HuggingFace repo id + filename for this model's GGUF. Unlike the
    /// reranker registry, the repo prefix and file prefix differ for the
    /// sauerkraut model, so both are spelled out per variant.
    pub fn gguf_spec(&self) -> NerGgufSpec {
        match self {
            NerModel::SauerkrautGlinerLfm => NerGgufSpec {
                repo: "cstr/sauerkraut-gliner-lfm-GGUF".to_owned(),
                file: "gliner-lfm-q8_0.gguf".to_owned(),
            },
            NerModel::GlinerDeberta => NerGgufSpec {
                repo: "cstr/gliner-deberta-GGUF".to_owned(),
                file: "gliner-deberta-q8_0.gguf".to_owned(),
            },
        }
    }
}

/// HF repo id + filename for a NER GGUF.
#[derive(Debug, Clone)]
pub struct NerGgufSpec {
    pub repo: String,
    pub file: String,
}

/// The default curated, configurable label set (Q3). Zero-shot, so labels are
/// plain strings. Multi-word labels (`"phone number"`) map to a compact prefix
/// via [`label_to_prefix`].
pub fn default_labels() -> Vec<String> {
    [
        "person",
        "organization",
        "location",
        "date",
        "product",
        "event",
        "title",
        "email",
        "phone number",
        "money",
        "law",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Map a GLiNER label to a compact, lowercased tag prefix. The model echoes
/// back the label string we passed in, so this mapping is deterministic for
/// the curated default set. Unknown / custom labels fall back to the
/// lowercased first whitespace token.
pub fn label_to_prefix(label: &str) -> String {
    match label.trim().to_lowercase().as_str() {
        "person" => "person".to_string(),
        "organization" | "org" => "org".to_string(),
        "location" | "loc" => "loc".to_string(),
        "date" => "date".to_string(),
        "product" => "product".to_string(),
        "event" => "event".to_string(),
        "title" => "title".to_string(),
        "email" => "email".to_string(),
        "phone number" | "phone" => "phone".to_string(),
        "money" => "money".to_string(),
        "law" => "law".to_string(),
        other => other
            .split_whitespace()
            .next()
            .unwrap_or("entity")
            .to_string(),
    }
}

/// One scored entity, backend-agnostic so the tag-building logic is testable
/// without the `crispembed` feature.
#[derive(Debug, Clone)]
pub(crate) struct ScoredEntity {
    pub label: String,
    pub text: String,
    pub score: f32,
}

/// Turn raw entities into deduped, capped, namespaced `label:text` tags (Q6).
///
/// - drop below `threshold` and empty-text entities,
/// - sort by score descending,
/// - dedup by `(prefix, lowercased text)` keeping the highest-scoring,
/// - cap to `max_entities` (0 = unlimited).
pub(crate) fn build_entity_tags(
    mut ents: Vec<ScoredEntity>,
    threshold: f32,
    max_entities: usize,
) -> Vec<String> {
    ents.retain(|e| e.score >= threshold && !e.text.trim().is_empty());
    // Highest score first; stable so ties keep input (positional) order.
    ents.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for e in ents {
        let prefix = label_to_prefix(&e.label);
        let text = e.text.trim();
        let key = (prefix.clone(), text.to_lowercase());
        if seen.insert(key) {
            out.push(format!("{prefix}:{text}"));
            if max_entities != 0 && out.len() >= max_entities {
                break;
            }
        }
    }
    out
}

// ── NER backend ──────────────────────────────────────────────────────────────

/// A loaded GLiNER model. Without `crispembed` this is a zero-field stub that
/// can never be constructed ([`Ner::load`] errors), keeping call sites
/// unconditional.
pub struct Ner {
    #[cfg(feature = "crispembed")]
    inner: crispembed::CrispNER,
}

impl Ner {
    /// Ensure the GGUF is on disk (downloading via hf-hub if absent), gate on
    /// the license, then open it through `crispembed::CrispNER`.
    #[cfg(feature = "crispembed")]
    pub async fn load(model: NerModel, cache_dir: &Path) -> Result<Self> {
        // License-consent gate before any download (e.g. LFM Open License).
        model.ensure_license_consent()?;
        let spec = model.gguf_spec();
        let path = ensure_ner_on_disk(&spec, cache_dir).await?;
        let p = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 NER GGUF path: {:?}", path))?;
        println!("[ner] Loading GGUF via CrispEmbed: {p}");
        let inner = crispembed::CrispNER::new(p, 0)
            .map_err(|e| anyhow::anyhow!("crispembed NER load failed: {e}"))?;
        Ok(Self { inner })
    }

    #[cfg(not(feature = "crispembed"))]
    pub async fn load(_model: NerModel, _cache_dir: &std::path::Path) -> Result<Self> {
        anyhow::bail!(
            "named-entity recognition requires the `crispembed` cargo feature \
             (build with --features crispembed-metal / -cuda / -vulkan)"
        );
    }

    /// Extract entities, normalising the backend type to [`ScoredEntity`].
    #[cfg(feature = "crispembed")]
    fn extract(&mut self, text: &str, labels: &[&str], threshold: f32) -> Vec<ScoredEntity> {
        self.inner
            .extract(text, labels, threshold)
            .into_iter()
            .map(|e| ScoredEntity {
                label: e.label,
                text: e.text,
                score: e.score,
            })
            .collect()
    }

    #[cfg(not(feature = "crispembed"))]
    fn extract(&mut self, _text: &str, _labels: &[&str], _threshold: f32) -> Vec<ScoredEntity> {
        vec![]
    }
}

// ── Lazy-load handle threaded through the ingest pipeline ────────────────────

/// Cheaply-clonable handle. The first [`extract_tags`](Self::extract_tags)
/// call loads the GGUF (downloading if absent) and caches the model behind the
/// inner mutex; subsequent calls reuse it.
///
/// A load failure is logged once and yields an empty tag list, so ingest
/// proceeds without entity tags rather than hard-erroring the whole document.
#[derive(Clone)]
pub struct NerHandle {
    model: NerModel,
    labels: Vec<String>,
    threshold: f32,
    max_entities: usize,
    /// Document text is truncated to this many bytes before extraction
    /// (latency cap; 0 = no truncation).
    max_chars: usize,
    cache_dir: PathBuf,
    slot: Arc<Mutex<Option<Ner>>>,
}

impl NerHandle {
    pub fn new(
        model: NerModel,
        labels: Vec<String>,
        threshold: f32,
        max_entities: usize,
        max_chars: usize,
        cache_dir: PathBuf,
    ) -> Self {
        Self {
            model,
            labels,
            threshold,
            max_entities,
            max_chars,
            cache_dir,
            slot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn model(&self) -> NerModel {
        self.model
    }

    /// Run NER over `text` and return ready-to-merge `label:text` tags.
    /// Truncates to `max_chars` on a char boundary first. Returns an empty
    /// `Vec` on empty input, load failure, or a build without `crispembed`.
    pub async fn extract_tags(&self, text: &str) -> Vec<String> {
        if text.trim().is_empty() || self.labels.is_empty() {
            return vec![];
        }
        let truncated = self.truncate(text);

        let mut guard = self.slot.lock().await;
        if guard.is_none() {
            match Ner::load(self.model, &self.cache_dir).await {
                Ok(n) => *guard = Some(n),
                Err(e) => {
                    eprintln!("[ner] load failed, skipping entity tags: {e:#}");
                    return vec![];
                }
            }
        }
        // Safe: just populated above (or pre-populated).
        let n = guard.as_mut().unwrap();
        let label_refs: Vec<&str> = self.labels.iter().map(|s| s.as_str()).collect();
        let ents = n.extract(truncated, &label_refs, self.threshold);
        build_entity_tags(ents, self.threshold, self.max_entities)
    }

    /// Run NER over `text` and return the structured fields `(label, value,
    /// score)` above the threshold, sorted by score — for ad-hoc key-info
    /// extraction (the `kie` CLI). Unlike [`Self::extract_tags`] (which returns
    /// merge-ready `label:text` strings) this keeps label/value/score separate.
    /// Empty on empty input, load failure, or a build without `crispembed`.
    pub async fn extract_fields(&self, text: &str) -> Vec<(String, String, f32)> {
        if text.trim().is_empty() || self.labels.is_empty() {
            return vec![];
        }
        let truncated = self.truncate(text);
        let mut guard = self.slot.lock().await;
        if guard.is_none() {
            match Ner::load(self.model, &self.cache_dir).await {
                Ok(n) => *guard = Some(n),
                Err(e) => {
                    eprintln!("[ner] load failed, no KIE fields: {e:#}");
                    return vec![];
                }
            }
        }
        let n = guard.as_mut().unwrap();
        let label_refs: Vec<&str> = self.labels.iter().map(|s| s.as_str()).collect();
        let mut ents = n.extract(truncated, &label_refs, self.threshold);
        ents.retain(|e| e.score >= self.threshold && !e.text.trim().is_empty());
        ents.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ents.into_iter().map(|e| (e.label, e.text, e.score)).collect()
    }

    /// Truncate `text` to at most `max_chars` bytes on a char boundary.
    fn truncate<'a>(&self, text: &'a str) -> &'a str {
        if self.max_chars == 0 || text.len() <= self.max_chars {
            return text;
        }
        let mut end = self.max_chars;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }
}

/// Build a [`NerHandle`] from the persisted [`IndexConfig`], or `None` when
/// NER is disabled. Shared by the GUI init path and the CLI ingest paths.
pub fn handle_from_config(
    config: &crate::index::IndexConfig,
    cache_dir: PathBuf,
) -> Option<NerHandle> {
    if !config.ner_enabled {
        return None;
    }
    let model = config.ner_model.unwrap_or_default();
    let labels = if config.ner_labels.is_empty() {
        default_labels()
    } else {
        config.ner_labels.clone()
    };
    Some(NerHandle::new(
        model,
        labels,
        config.ner_threshold,
        config.ner_max_entities,
        config.ner_max_chars,
        cache_dir,
    ))
}

#[cfg(feature = "crispembed")]
async fn ensure_ner_on_disk(spec: &NerGgufSpec, cache_dir: &Path) -> Result<PathBuf> {
    let api = ApiBuilder::new()
        .with_cache_dir(cache_dir.to_path_buf())
        .build()
        .context("Failed to build hf-hub Api for NER")?;
    let model_api = api.model(spec.repo.clone());
    println!("[ner] Fetching GGUF: {}/{} …", spec.repo, spec.file);
    model_api
        .get(&spec.file)
        .await
        .with_context(|| format!("failed to get {}/{}", spec.repo, spec.file))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(label: &str, text: &str, score: f32) -> ScoredEntity {
        ScoredEntity {
            label: label.to_string(),
            text: text.to_string(),
            score,
        }
    }

    /// Pin the serde kebab-case strings so the frontend mapper stays in lockstep.
    #[test]
    fn ner_model_serde_strings() {
        let cases: &[(NerModel, &str)] = &[
            (NerModel::SauerkrautGlinerLfm, "sauerkraut-gliner-lfm"),
            (NerModel::GlinerDeberta, "gliner-deberta"),
        ];
        for (variant, expected) in cases {
            let s = serde_json::to_string(variant).unwrap();
            assert_eq!(s.trim_matches('"'), *expected, "serde for {variant:?}");
        }
    }

    #[test]
    fn default_model_is_german_tuned() {
        assert_eq!(NerModel::default(), NerModel::SauerkrautGlinerLfm);
    }

    #[test]
    fn license_gating_split() {
        // Sauerkraut LFM is restricted (consent required); DeBERTa is permissive.
        assert!(NerModel::SauerkrautGlinerLfm.license().requires_consent());
        assert!(!NerModel::GlinerDeberta.license().requires_consent());
        assert_eq!(NerModel::GlinerDeberta.consent_key(), "");
        assert_eq!(
            NerModel::SauerkrautGlinerLfm.consent_key(),
            "sauerkraut-gliner-lfm"
        );
    }

    #[test]
    fn gguf_spec_matches_registry() {
        let s = NerModel::SauerkrautGlinerLfm.gguf_spec();
        assert_eq!(s.repo, "cstr/sauerkraut-gliner-lfm-GGUF");
        assert_eq!(s.file, "gliner-lfm-q8_0.gguf");
        let d = NerModel::GlinerDeberta.gguf_spec();
        assert_eq!(d.repo, "cstr/gliner-deberta-GGUF");
        assert_eq!(d.file, "gliner-deberta-q8_0.gguf");
    }

    #[test]
    fn label_prefix_mapping() {
        assert_eq!(label_to_prefix("person"), "person");
        assert_eq!(label_to_prefix("organization"), "org");
        assert_eq!(label_to_prefix("location"), "loc");
        assert_eq!(label_to_prefix("phone number"), "phone");
        // Case-insensitive.
        assert_eq!(label_to_prefix("PERSON"), "person");
        // Unknown multi-word label → lowercased first token.
        assert_eq!(label_to_prefix("Vehicle Identification"), "vehicle");
    }

    #[test]
    fn tags_are_namespaced_with_original_case_text() {
        let tags = build_entity_tags(
            vec![
                ent("person", "Barack Obama", 0.9),
                ent("organization", "United Nations", 0.8),
                ent("location", "Hawaii", 0.7),
            ],
            0.5,
            30,
        );
        assert!(tags.contains(&"person:Barack Obama".to_string()));
        assert!(tags.contains(&"org:United Nations".to_string()));
        assert!(tags.contains(&"loc:Hawaii".to_string()));
    }

    #[test]
    fn threshold_drops_low_scores() {
        let tags = build_entity_tags(
            vec![ent("person", "Alice", 0.9), ent("person", "Bob", 0.3)],
            0.5,
            30,
        );
        assert_eq!(tags, vec!["person:Alice".to_string()]);
    }

    #[test]
    fn dedup_case_insensitive_keeps_highest_score() {
        // Same (prefix, text) twice — only one tag, the higher score wins
        // ordering but the text/case of the first-after-sort survives.
        let tags = build_entity_tags(
            vec![
                ent("person", "obama", 0.6),
                ent("person", "Obama", 0.95),
            ],
            0.5,
            30,
        );
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0], "person:Obama");
    }

    #[test]
    fn cap_keeps_top_n_by_score() {
        let tags = build_entity_tags(
            vec![
                ent("person", "A", 0.6),
                ent("person", "B", 0.9),
                ent("person", "C", 0.7),
            ],
            0.5,
            2,
        );
        assert_eq!(tags, vec!["person:B".to_string(), "person:C".to_string()]);
    }

    #[test]
    fn max_entities_zero_is_unlimited() {
        let tags = build_entity_tags(
            vec![
                ent("person", "A", 0.6),
                ent("org", "B", 0.9),
                ent("loc", "C", 0.7),
            ],
            0.5,
            0,
        );
        assert_eq!(tags.len(), 3);
    }

    #[test]
    fn empty_and_whitespace_text_dropped() {
        let tags = build_entity_tags(
            vec![ent("person", "   ", 0.9), ent("org", "ACME", 0.9)],
            0.5,
            30,
        );
        assert_eq!(tags, vec!["org:ACME".to_string()]);
    }

    #[test]
    fn handle_truncates_on_char_boundary() {
        let h = NerHandle::new(
            NerModel::default(),
            default_labels(),
            0.5,
            30,
            5,
            PathBuf::from("/tmp"),
        );
        // "über" — 'ü' is two bytes; truncating at byte 5 must back off to a
        // boundary, never panic.
        let s = "über test";
        let t = h.truncate(s);
        assert!(s.is_char_boundary(t.len()));
        assert!(t.len() <= 5);
    }

    #[tokio::test]
    async fn extract_tags_empty_input_is_noop() {
        let h = NerHandle::new(
            NerModel::default(),
            default_labels(),
            0.5,
            30,
            8000,
            PathBuf::from("/tmp"),
        );
        assert!(h.extract_tags("").await.is_empty());
        assert!(h.extract_tags("   \n  ").await.is_empty());
    }

    #[cfg(not(feature = "crispembed"))]
    #[tokio::test]
    async fn extract_tags_is_noop_without_feature() {
        // Without the crispembed feature, extraction must be a silent no-op
        // (load errors → empty tags), so ingest stays byte-identical.
        let h = NerHandle::new(
            NerModel::default(),
            default_labels(),
            0.5,
            30,
            8000,
            PathBuf::from("/tmp"),
        );
        assert!(h.extract_tags("Barack Obama met the UN in Hawaii.").await.is_empty());
    }
}
