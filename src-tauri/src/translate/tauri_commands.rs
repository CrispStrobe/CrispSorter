//! Tauri commands for document translation.
//!
//! Frontend invokes these to drive a docx-to-docx LLM translation flow.
//! The implementation is delegated to the `crisp-docx-core` +
//! `crisp-docx-llm` workspace crates so the heavy lifting (OOXML
//! surgery, provider HTTP, fallback chain) lives in one place and is
//! exercised by the standalone `crisp-translate` CLI too.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crisp_docx_llm::{LlmTranslator, ProviderConfig, ProviderKind};

/// One provider config, as passed from the frontend.
///
/// `kind` is a lowercase string (`"openai"` / `"anthropic"` /
/// `"ollama"` / `"groq"`). `api_key` is None for Ollama.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// Provider name: `"openai"`, `"anthropic"`, `"ollama"`, `"groq"`.
    pub kind: String,
    /// Per-provider model id (e.g. `"gpt-4o-mini"`).
    pub model: String,
    /// API key (None / empty for Ollama).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional base URL override (proxy / custom host).
    #[serde(default)]
    pub base_url: Option<String>,
}

fn provider_kind(s: &str) -> Result<ProviderKind, String> {
    match s.to_ascii_lowercase().as_str() {
        "openai" => Ok(ProviderKind::OpenAi),
        "anthropic" => Ok(ProviderKind::Anthropic),
        "ollama" => Ok(ProviderKind::Ollama),
        "groq" => Ok(ProviderKind::Groq),
        other => Err(format!("unknown provider kind: {other}")),
    }
}

fn build_translator(providers: Vec<ProviderSpec>) -> Result<LlmTranslator, String> {
    if providers.is_empty() {
        return Err("no providers — pass at least one ProviderSpec".into());
    }
    let mut t = LlmTranslator::new();
    for p in providers {
        let kind = provider_kind(&p.kind)?;
        let api_key = p.api_key.filter(|s| !s.is_empty());
        let cfg = ProviderConfig {
            kind,
            api_key,
            model: p.model,
            base_url: p.base_url,
        };
        t = t.add_provider(cfg).map_err(|e| e.to_string())?;
    }
    Ok(t)
}

/// Dry-run extraction: read the input .docx and return its paragraph
/// texts. No LLM calls. Lets the UI preview what would be translated.
#[tauri::command]
pub async fn translate_dry_run(input: String) -> Result<Vec<String>, String> {
    let path = PathBuf::from(&input);
    let pkg = crisp_docx_core::open(&path).map_err(|e| e.to_string())?;
    crisp_docx_core::extract_paragraph_texts(&pkg).map_err(|e| e.to_string())
}

/// Progress event emitted as paragraphs land.
#[derive(Debug, Clone, Serialize)]
pub struct TranslateProgress {
    /// 1-based index of the paragraph just translated.
    pub paragraph_index: usize,
    /// Total paragraphs in the input.
    pub total: usize,
}

/// Translate every paragraph of `input` from `source_lang` to
/// `target_lang` and write the result to `output`.
///
/// Returns a summary `{ total, succeeded, failed }`. Paragraphs that
/// failed (e.g. all providers returned errors) are left as their
/// original text in the output document — never a half-translated
/// docx.
///
/// `concurrency` defaults to 4 if 0.
#[tauri::command]
pub async fn translate_docx(
    app: tauri::AppHandle,
    input: String,
    output: String,
    source_lang: String,
    target_lang: String,
    providers: Vec<ProviderSpec>,
    concurrency: Option<usize>,
) -> Result<TranslateResult, String> {
    use tauri::Emitter;

    let translator = build_translator(providers)?;
    let in_path = PathBuf::from(&input);
    let out_path = PathBuf::from(&output);
    let conc = concurrency.unwrap_or(4).max(1);

    let mut pkg = crisp_docx_core::open(&in_path).map_err(|e| e.to_string())?;
    let paragraphs =
        crisp_docx_core::extract_paragraph_texts(&pkg).map_err(|e| e.to_string())?;
    let total = paragraphs.len();

    // Wrap the translator so each spawned future can clone an Arc.
    let translator = std::sync::Arc::new(translator);

    use futures::stream::{self, StreamExt};
    // Materialise (index, owned-string) tuples up front so the async
    // closure isn't borrowing from a non-`'static` slice — Tauri's
    // command-handler macro requires the future to be `'static`.
    let owned_pairs: Vec<(usize, String)> = paragraphs.iter().cloned().enumerate().collect();
    let outs = stream::iter(owned_pairs.into_iter())
        .map(|(i, text)| {
            let translator = translator.clone();
            let app = app.clone();
            let src = source_lang.clone();
            let tgt = target_lang.clone();
            async move {
                let r = translator.translate_text(&text, &src, &tgt).await;
                let _ = app.emit(
                    "translate://progress",
                    TranslateProgress {
                        paragraph_index: i + 1,
                        total,
                    },
                );
                (i, r)
            }
        })
        .buffer_unordered(conc)
        .collect::<Vec<_>>()
        .await;

    // Reassemble in input order; on failure keep the original.
    let mut new_texts: Vec<String> = paragraphs.clone();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for (i, r) in outs {
        match r {
            Ok(v) => {
                new_texts[i] = v;
                succeeded += 1;
            }
            Err(_) => {
                failed += 1;
            }
        }
    }

    crisp_docx_core::replace_paragraph_texts(&mut pkg, &new_texts)
        .map_err(|e| e.to_string())?;
    crisp_docx_core::save(&pkg, &out_path).map_err(|e| e.to_string())?;

    Ok(TranslateResult {
        total,
        succeeded,
        failed,
    })
}

/// Summary returned from `translate_docx`.
#[derive(Debug, Clone, Serialize)]
pub struct TranslateResult {
    /// Total paragraphs in the input document.
    pub total: usize,
    /// Paragraphs that the LLM translated successfully.
    pub succeeded: usize,
    /// Paragraphs that the LLM failed on (kept as original text).
    pub failed: usize,
}
