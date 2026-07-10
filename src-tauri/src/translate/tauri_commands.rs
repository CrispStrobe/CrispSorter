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
        "openrouter" => Ok(ProviderKind::OpenRouter),
        "together" => Ok(ProviderKind::Together),
        "cerebras" => Ok(ProviderKind::Cerebras),
        "mistral" => Ok(ProviderKind::Mistral),
        "nebius" => Ok(ProviderKind::Nebius),
        "scaleway" => Ok(ProviderKind::Scaleway),
        "poe" => Ok(ProviderKind::Poe),
        "google" => Ok(ProviderKind::Google),
        "nmt" => Ok(ProviderKind::Nmt),
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
///
/// When `preserve_formatting` is `true`, intra-paragraph runs (bold,
/// italic, rStyle) are mapped from source to target via word
/// alignment using the multilingual encoder at `align_model_path`.
/// This requires the binary to be built with `--features translate-align`;
/// otherwise the flag is rejected with an error.
#[tauri::command]
pub async fn translate_docx(
    app: tauri::AppHandle,
    input: String,
    output: String,
    source_lang: String,
    target_lang: String,
    providers: Vec<ProviderSpec>,
    concurrency: Option<usize>,
    preserve_formatting: Option<bool>,
    align_model_path: Option<String>,
) -> Result<TranslateResult, String> {
    use tauri::Emitter;

    let translator = build_translator(providers)?;
    let in_path = PathBuf::from(&input);
    let out_path = PathBuf::from(&output);
    let conc = concurrency.unwrap_or(4).max(1);
    let preserve_formatting = preserve_formatting.unwrap_or(false);

    if preserve_formatting {
        #[cfg(not(feature = "translate-align"))]
        return Err(
            "preserve_formatting requires the binary to be built with --features translate-align"
                .into(),
        );
    }

    let mut pkg = crisp_docx_core::open(&in_path).map_err(|e| e.to_string())?;

    // P30.1 — Pre-processing: strip revision IDs (cures Word
    // "unreadable content" dialog) and normalize quotes.
    let _ = crisp_docx_core::strip_rsids(&mut pkg);
    let _ = crisp_docx_core::normalize_quotes_in_package(
        &mut pkg,
        crisp_docx_core::QuoteStyle::English,
        crisp_docx_core::QuoteOptions::default(),
    );

    // P30.1 — Pre-flight validation: warn but don't block.
    let check = crisp_docx_core::check_package(&pkg);
    if let Ok(ref report) = check {
        if !report.issues.is_empty() {
            let warning = format!(
                "DOCX pre-flight: {} issue(s): {}",
                report.issues.len(),
                report.issues.join("; ")
            );
            log::warn!("{warning}");
            let _ = app.emit("translate://warning", warning);
        }
    }

    let paragraphs = crisp_docx_core::extract_paragraph_texts(&pkg).map_err(|e| e.to_string())?;
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

    #[cfg(feature = "translate-align")]
    if preserve_formatting {
        let model_path = align_model_path
            .as_deref()
            .ok_or_else(|| "preserve_formatting requires align_model_path".to_string())?;
        write_back_with_alignment(&mut pkg, &paragraphs, &new_texts, model_path)
            .map_err(|e| format!("format-preserving write-back: {e}"))?;
    } else {
        crisp_docx_core::replace_paragraph_texts(&mut pkg, &new_texts)
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(feature = "translate-align"))]
    {
        let _ = align_model_path; // unused without the feature
        crisp_docx_core::replace_paragraph_texts(&mut pkg, &new_texts)
            .map_err(|e| e.to_string())?;
    }

    crisp_docx_core::save(&pkg, &out_path).map_err(|e| e.to_string())?;

    Ok(TranslateResult {
        total,
        succeeded,
        failed,
    })
}

/// Re-runs the alignment-driven format transfer on top of the
/// already-translated paragraph texts and writes them back at run
/// granularity, carrying each source run's rPr through to the target.
#[cfg(feature = "translate-align")]
fn write_back_with_alignment(
    pkg: &mut crisp_docx_core::Package,
    src_texts: &[String],
    translations: &[String],
    model_path: &str,
) -> Result<(), String> {
    use crisp_docx_align::{align_texts, transfer_format_via_words, SourceRun, Strategy};
    use crisp_docx_core::{ParagraphInfo, Run as CoreRun};
    use crispembed::CrispEmbed;

    let mut model = CrispEmbed::new(model_path, 4)
        .map_err(|e| format!("loading align model {model_path}: {e}"))?;

    let src_paragraphs = crisp_docx_core::extract_paragraph_runs(pkg).map_err(|e| e.to_string())?;
    if src_paragraphs.len() != src_texts.len() {
        return Err(format!(
            "paragraph-count mismatch (text={}, runs={})",
            src_texts.len(),
            src_paragraphs.len()
        ));
    }

    let mut new_paragraphs: Vec<ParagraphInfo> = Vec::with_capacity(src_paragraphs.len());
    for (i, info) in src_paragraphs.iter().enumerate() {
        let translation = translations.get(i).cloned().unwrap_or_default();
        let src_text = info.full_text();
        if src_text.trim().is_empty() || translation.trim().is_empty() {
            new_paragraphs.push(info.clone());
            continue;
        }

        let source_runs: Vec<SourceRun<Option<Vec<u8>>>> = info
            .runs
            .iter()
            .map(|r| SourceRun {
                text: r.text.clone(),
                format_id: r.rpr_xml.clone(),
            })
            .collect();

        let alignment = align_texts(
            &mut model,
            &src_text,
            &translation,
            Strategy::Itermax { min_sim: 0.3 },
        )
        .map_err(|e| format!("aligning paragraph {i}: {e}"))?;
        let target_runs =
            transfer_format_via_words(&source_runs, &translation, &alignment.word_edges, None);

        // Carry every source-paragraph footnote ref through to the last
        // target run (deterministic; finer placement is a future-task).
        let mut footnote_refs_all: Vec<Vec<u8>> = info
            .runs
            .iter()
            .flat_map(|r| r.footnote_refs.clone())
            .collect();

        let mut runs: Vec<CoreRun> = target_runs
            .into_iter()
            .map(|tr| CoreRun {
                text: tr.text,
                rpr_xml: tr.format_id,
                footnote_refs: Vec::new(),
            })
            .collect();
        if !footnote_refs_all.is_empty() {
            if let Some(last) = runs.last_mut() {
                last.footnote_refs.append(&mut footnote_refs_all);
            } else {
                runs.push(CoreRun {
                    text: String::new(),
                    rpr_xml: None,
                    footnote_refs: footnote_refs_all,
                });
            }
        }

        new_paragraphs.push(ParagraphInfo {
            ppr_xml: info.ppr_xml.clone(),
            runs,
            leading_bookmark_starts: info.leading_bookmark_starts.clone(),
            trailing_bookmark_ends: info.trailing_bookmark_ends.clone(),
        });
    }

    crisp_docx_core::replace_paragraph_runs(pkg, &new_paragraphs).map_err(|e| e.to_string())
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
