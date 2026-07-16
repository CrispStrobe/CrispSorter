//! P30 — crisp-docx deep integration: DOCX surgery Tauri commands.
//!
//! Exposes crisp-docx-core's OOXML surgery as Tauri commands and CLI
//! verbs.  These go beyond the existing translate pipeline (P16) to
//! cover validation, blueprint analysis, body transplant, heading
//! inference, and footnote/endnote operations.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ── P30.4 — DOCX validation ─────────────────────────────────────────────

/// DOCX package validation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocxCheckResult {
    /// Checks that passed.
    pub ok: Vec<String>,
    /// Issues found (empty = valid).
    pub issues: Vec<String>,
    /// Overall verdict.
    pub valid: bool,
}

/// Validate a DOCX file's internal structure.
///
/// Runs 7 checks: XML parse, rsids declared, paraIds unique, rel targets
/// exist, body shape valid, bookmark IDs unique, inline rIds resolve.
#[tauri::command]
pub async fn docx_check(path: String) -> Result<DocxCheckResult, String> {
    let p = PathBuf::from(&path);
    let pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;
    let report = crisp_docx_core::check_package(&pkg).map_err(|e| e.to_string())?;
    Ok(DocxCheckResult {
        valid: report.issues.is_empty(),
        ok: report.ok,
        issues: report.issues,
    })
}

// ── P30.3 — Blueprint analysis ──────────────────────────────────────────

/// Document blueprint (page geometry, default font, styles).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocxBlueprint {
    pub sections: Vec<DocxSection>,
    pub default_font: String,
    pub default_font_size_pt: f64,
    pub style_count: usize,
}

/// One section's page geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocxSection {
    pub page_width_pt: f64,
    pub page_height_pt: f64,
    pub left_margin_pt: f64,
    pub right_margin_pt: f64,
    pub top_margin_pt: f64,
    pub bottom_margin_pt: f64,
    pub orientation: Option<String>,
}

/// Analyze a DOCX file's blueprint (page geometry, fonts, styles).
#[tauri::command]
pub async fn docx_analyze(path: String) -> Result<DocxBlueprint, String> {
    let p = PathBuf::from(&path);
    let pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;
    let schema = crisp_docx_core::analyze_blueprint(&pkg).map_err(|e| e.to_string())?;

    let sections = schema
        .sections
        .iter()
        .map(|s| DocxSection {
            page_width_pt: s.page_width_pt,
            page_height_pt: s.page_height_pt,
            left_margin_pt: s.left_margin_pt,
            right_margin_pt: s.right_margin_pt,
            top_margin_pt: s.top_margin_pt,
            bottom_margin_pt: s.bottom_margin_pt,
            orientation: s.orientation.clone(),
        })
        .collect();

    Ok(DocxBlueprint {
        sections,
        default_font: schema.default_font,
        default_font_size_pt: schema.default_font_size_pt,
        style_count: schema.styles.styles.len(),
    })
}

// ── P30.2 — Heading inference ───────────────────────────────────────────

/// An inferred heading from direct formatting analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredHeading {
    /// 1-based heading level (1 = H1, 2 = H2, 3 = H3).
    pub level: u8,
    /// The heading text.
    pub text: String,
}

/// Infer heading levels from a DOCX that lacks explicit heading styles.
///
/// Uses bold + font size clustering to detect H1/H2/H3 structure.
#[tauri::command]
pub async fn docx_infer_headings(path: String) -> Result<Vec<InferredHeading>, String> {
    let p = PathBuf::from(&path);
    let pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;
    let inferences =
        crisp_docx_core::infer_heading_levels(&pkg, None).map_err(|e| e.to_string())?;

    Ok(inferences
        .iter()
        .map(|h| InferredHeading {
            level: h.heading_level,
            text: h.preview.clone(),
        })
        .collect())
}

// ── P30.5 — Body transplant ("restyle to template") ─────────────────────

/// Transplant result summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransplantResult {
    pub output_path: String,
    pub source_paragraphs: usize,
    pub blueprint_styles: usize,
    /// Number of paragraph styles remapped by the StyleMapper.
    pub styles_remapped: usize,
}

/// Graft the body of `source` into the styles/headers/footers of
/// `blueprint` and write the result to `output`.
///
/// After transplanting the body, runs `StyleMapper` to remap source
/// paragraph styles to their blueprint equivalents (heading → heading,
/// body → body, etc.) using the semantic fallback chain.
#[tauri::command]
pub async fn docx_transplant(
    source: String,
    blueprint: String,
    output: String,
) -> Result<TransplantResult, String> {
    let source_path = PathBuf::from(&source);
    let blueprint_path = PathBuf::from(&blueprint);
    let output_path = PathBuf::from(&output);

    let source_pkg = crisp_docx_core::open(&source_path).map_err(|e| e.to_string())?;
    let mut blueprint_pkg = crisp_docx_core::open(&blueprint_path).map_err(|e| e.to_string())?;

    let source_paras = crisp_docx_core::extract_paragraph_texts(&source_pkg)
        .map(|p| p.len())
        .unwrap_or(0);

    // Build style indexes for both documents.
    let source_index = crisp_docx_core::StyleIndex::from_package(&source_pkg)
        .map_err(|e| format!("source style index: {e}"))?;
    let blueprint_index = crisp_docx_core::StyleIndex::from_package(&blueprint_pkg)
        .map_err(|e| format!("blueprint style index: {e}"))?;
    let blueprint_style_count = blueprint_index.styles.len();

    // Transplant body (source content → blueprint shell).
    crisp_docx_core::transplant_body(&mut blueprint_pkg, &source_pkg)
        .map_err(|e| format!("transplant failed: {e}"))?;

    // Remap source styles to blueprint equivalents.
    let mapper = crisp_docx_core::StyleMapper::new(
        &blueprint_index,
        std::collections::HashMap::new(), // no user overrides
    );
    let remapped = crisp_docx_core::apply_style_mapping(
        &mut blueprint_pkg,
        &mapper,
        &source_index,
        &blueprint_index,
    )
    .map_err(|e| format!("style mapping: {e}"))?;

    crisp_docx_core::save(&blueprint_pkg, &output_path).map_err(|e| e.to_string())?;

    Ok(TransplantResult {
        output_path: output,
        source_paragraphs: source_paras,
        blueprint_styles: blueprint_style_count,
        styles_remapped: remapped,
    })
}

// ── P30.6 — Footnote/endnote conversion ─────────────────────────────────

/// Convert footnotes ↔ endnotes in a DOCX file.
///
/// `target_kind` must be `"footnotes"` or `"endnotes"`.
#[tauri::command]
pub async fn docx_convert_notes(
    path: String,
    target_kind: String,
    output: String,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let out = PathBuf::from(&output);

    let target = match target_kind.to_ascii_lowercase().as_str() {
        "footnotes" | "footnote" => crisp_docx_core::NotesKind::Footnotes,
        "endnotes" | "endnote" => crisp_docx_core::NotesKind::Endnotes,
        other => {
            return Err(format!(
                "unknown target_kind '{other}', expected 'footnotes' or 'endnotes'"
            ))
        }
    };

    let mut pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;
    crisp_docx_core::convert_notes_kind(&mut pkg, target).map_err(|e| e.to_string())?;
    crisp_docx_core::save(&pkg, &out).map_err(|e| e.to_string())?;

    Ok(output)
}

// ── P30.7 — Footnote injection ──────────────────────────────────────────

/// Inject footnotes into a DOCX from inline [N] markers.
///
/// `notes` maps marker numbers to note text (e.g. `{"1": "Note text"}`).
#[tauri::command]
pub async fn docx_inject_footnotes(
    path: String,
    notes: BTreeMap<u32, String>,
    output: String,
) -> Result<usize, String> {
    let p = PathBuf::from(&path);
    let out = PathBuf::from(&output);

    let mut pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;

    // Convert owned strings to borrowed for the crisp-docx API.
    let notes_ref: BTreeMap<u32, &str> = notes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let report =
        crisp_docx_core::inject_footnotes(&mut pkg, &notes_ref).map_err(|e| e.to_string())?;

    crisp_docx_core::save(&pkg, &out).map_err(|e| e.to_string())?;
    Ok(report.inserted)
}

// ── P30.1 supplement — strip_rsids standalone ───────────────────────────

/// Strip revision tracking attributes from a DOCX (standalone, non-translation).
#[tauri::command]
pub async fn docx_strip_rsids(path: String, output: String) -> Result<usize, String> {
    let p = PathBuf::from(&path);
    let out = PathBuf::from(&output);

    let mut pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;
    let count = crisp_docx_core::strip_rsids(&mut pkg).map_err(|e| e.to_string())?;
    crisp_docx_core::save(&pkg, &out).map_err(|e| e.to_string())?;
    Ok(count)
}

// ── P30.8 — Quote normalization standalone ──────────────────────────────

/// Normalize quotes in a DOCX file.
///
/// `style` is one of: `"german"`, `"english"`, `"french"`, `"swiss"`,
/// `"german_guillemets"`.
#[tauri::command]
pub async fn docx_normalize_quotes(
    path: String,
    style: String,
    output: String,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let out = PathBuf::from(&output);

    let quote_style = parse_quote_style(&style)?;
    let mut pkg = crisp_docx_core::open(&p).map_err(|e| e.to_string())?;
    crisp_docx_core::normalize_quotes_in_package(
        &mut pkg,
        quote_style,
        crisp_docx_core::QuoteOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    crisp_docx_core::save(&pkg, &out).map_err(|e| e.to_string())?;
    Ok(output)
}

pub fn parse_quote_style(s: &str) -> Result<crisp_docx_core::QuoteStyle, String> {
    match s.to_ascii_lowercase().as_str() {
        "german" => Ok(crisp_docx_core::QuoteStyle::German),
        "english" => Ok(crisp_docx_core::QuoteStyle::English),
        "french" => Ok(crisp_docx_core::QuoteStyle::French),
        "swiss" => Ok(crisp_docx_core::QuoteStyle::Swiss),
        "german_guillemets" | "guillemets" => Ok(crisp_docx_core::QuoteStyle::GermanGuillemets),
        other => Err(format!(
            "unknown quote style '{other}', expected: german, english, french, swiss, german_guillemets"
        )),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quote_style_valid() {
        assert!(parse_quote_style("german").is_ok());
        assert!(parse_quote_style("English").is_ok());
        assert!(parse_quote_style("FRENCH").is_ok());
        assert!(parse_quote_style("swiss").is_ok());
        assert!(parse_quote_style("german_guillemets").is_ok());
        assert!(parse_quote_style("guillemets").is_ok());
    }

    #[test]
    fn parse_quote_style_invalid() {
        assert!(parse_quote_style("ascii").is_err());
        assert!(parse_quote_style("").is_err());
    }

    #[test]
    fn parse_notes_kind() {
        // Exercise the match arms in docx_convert_notes (sync version).
        for (input, _) in [
            ("footnotes", ()),
            ("endnotes", ()),
            ("footnote", ()),
            ("endnote", ()),
        ] {
            let kind = match input.to_ascii_lowercase().as_str() {
                "footnotes" | "footnote" => crisp_docx_core::NotesKind::Footnotes,
                "endnotes" | "endnote" => crisp_docx_core::NotesKind::Endnotes,
                _ => panic!("unexpected"),
            };
            // Just verify no panic.
            let _ = kind;
        }
    }

    #[test]
    fn docx_check_result_serde() {
        let r = DocxCheckResult {
            ok: vec!["XML valid".into()],
            issues: vec!["broken rel".into()],
            valid: false,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: DocxCheckResult = serde_json::from_str(&json).unwrap();
        assert!(!back.valid);
        assert_eq!(back.issues.len(), 1);
    }

    #[test]
    fn docx_blueprint_serde() {
        let b = DocxBlueprint {
            sections: vec![DocxSection {
                page_width_pt: 595.0,
                page_height_pt: 842.0,
                left_margin_pt: 72.0,
                right_margin_pt: 72.0,
                top_margin_pt: 72.0,
                bottom_margin_pt: 72.0,
                orientation: None,
            }],
            default_font: "Calibri".into(),
            default_font_size_pt: 11.0,
            style_count: 25,
        };
        let json = serde_json::to_string(&b).unwrap();
        let back: DocxBlueprint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.default_font, "Calibri");
        assert_eq!(back.sections.len(), 1);
    }

    #[test]
    fn transplant_result_serde() {
        let r = TransplantResult {
            output_path: "/tmp/out.docx".into(),
            source_paragraphs: 42,
            blueprint_styles: 15,
            styles_remapped: 8,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: TransplantResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_paragraphs, 42);
        assert_eq!(back.styles_remapped, 8);
    }

    #[test]
    fn inferred_heading_serde() {
        let h = InferredHeading {
            level: 1,
            text: "Introduction".into(),
        };
        let json = serde_json::to_string(&h).unwrap();
        let back: InferredHeading = serde_json::from_str(&json).unwrap();
        assert_eq!(back.level, 1);
        assert_eq!(back.text, "Introduction");
    }
}
