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
///
/// Every measurement is optional because a DOCX section may simply not
/// state it — `w:sectPr` can omit `w:pgSz` or `w:pgMar` entirely, and the
/// reader reports that as absent rather than guessing.  Substituting a
/// default here would make "the document says nothing about page width"
/// indistinguishable from "the document says A4", which is exactly the
/// distinction a blueprint view exists to show.  Serialises as `null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocxSection {
    pub page_width_pt: Option<f64>,
    pub page_height_pt: Option<f64>,
    pub left_margin_pt: Option<f64>,
    pub right_margin_pt: Option<f64>,
    pub top_margin_pt: Option<f64>,
    pub bottom_margin_pt: Option<f64>,
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
            // Passed through as Option, not defaulted: main's
            // `unwrap_or(0.0)` would make "the document states nothing about
            // page width" indistinguishable from "the page is 0 pt wide",
            // which is the distinction `DocxSection`'s doc comment and
            // `analyze_reports_unstated_geometry_as_none_not_a_default` exist
            // to protect. (Merge resolution, 2026-07-30.)
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

// ── Test fixtures ────────────────────────────────────────────────────────

/// Minimal `.docx` packages built in memory.
///
/// The P30 tests were deferred for want of "test .docx fixtures in the
/// repo" — but a `.docx` is a zip of a few XML parts, so the fixtures can
/// be *authored* instead of committed. That keeps them readable (the shape
/// each test depends on is right there in the string) and keeps binaries
/// out of git. `crisp-docx-core`'s own integration tests use the same
/// approach.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::{Cursor, Write};

    const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    const EMPTY_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

    fn document(body_inner: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body>{body_inner}</w:body></w:document>"#
        )
        .into_bytes()
    }

    fn zip_package(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    fn simple(body_inner: &str) -> Vec<u8> {
        zip_package(&[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("word/document.xml", &document(body_inner)),
            ("word/_rels/document.xml.rels", EMPTY_RELS.as_bytes()),
        ])
    }

    /// One paragraph, nothing unusual.
    pub fn plain() -> Vec<u8> {
        simple(r#"<w:p><w:r><w:t>a perfectly ordinary paragraph</w:t></w:r></w:p>"#)
    }

    /// Two bold headings at different sizes over three body paragraphs, so
    /// size clustering has something to cluster.
    pub fn with_headings() -> Vec<u8> {
        let mut body = String::new();
        body.push_str(
            r#"<w:p><w:pPr><w:rPr><w:b/><w:sz w:val="48"/></w:rPr></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="48"/></w:rPr><w:t>Chapter One</w:t></w:r></w:p>"#,
        );
        for i in 0..3 {
            body.push_str(&format!(
                r#"<w:p><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t>body sentence {i} of the chapter</w:t></w:r></w:p>"#
            ));
        }
        body.push_str(
            r#"<w:p><w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="32"/></w:rPr><w:t>Section A</w:t></w:r></w:p>"#,
        );
        for i in 0..3 {
            body.push_str(&format!(
                r#"<w:p><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t>more body prose number {i}</w:t></w:r></w:p>"#
            ));
        }
        simple(&body)
    }

    /// Every rsid / paraId attribute variant `strip_rsids` removes.
    pub fn with_rsids() -> Vec<u8> {
        simple(
            r#"<w:p w14:paraId="A1B2" w14:textId="C3D4" w:rsidR="00112233" w:rsidRDefault="44556677" w:rsidRPr="DEADBEEF" w:rsidP="55667788"><w:r w:rsidR="11223344" w:rsidRPr="99887766"><w:t>tracked</w:t></w:r></w:p>"#,
        )
    }

    /// Straight quotes and apostrophes for the normaliser to curl.
    pub fn with_straight_quotes() -> Vec<u8> {
        simple(r#"<w:p><w:r><w:t>He said "hello" to them.</w:t></w:r></w:p>"#)
    }

    /// Inline `[1]` / `[2]` markers and no notes part at all.
    pub fn with_inline_markers() -> Vec<u8> {
        simple(
            r#"<w:p><w:r><w:t xml:space="preserve">opener.[1] middle.[2] end.</w:t></w:r></w:p>"#,
        )
    }

    /// Letter-size page geometry in a trailing `sectPr`.
    pub fn blueprint_letter() -> Vec<u8> {
        simple(
            r#"<w:p><w:r><w:t>blueprint-only paragraph</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>"#,
        )
    }

    /// A `sectPr` that states nothing — the "unstated vs stated" case the
    /// optional geometry fields exist for.
    pub fn blueprint_without_geometry() -> Vec<u8> {
        simple(r#"<w:p><w:r><w:t>no geometry here</w:t></w:r></w:p><w:sectPr/>"#)
    }

    /// Two distinctive paragraphs plus a different page size, for transplant.
    pub fn source_two_paragraphs() -> Vec<u8> {
        simple(
            r#"<w:p><w:r><w:t>source paragraph one</w:t></w:r></w:p><w:p><w:r><w:t>source paragraph two</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="8000" w:h="10000"/></w:sectPr>"#,
        )
    }

    /// A working footnotes part referenced from the body.
    pub fn with_footnotes() -> Vec<u8> {
        let body = r#"<w:p><w:r><w:t xml:space="preserve">Body. </w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="1"/></w:r><w:r><w:t xml:space="preserve"> after.</w:t></w:r></w:p>"#;
        let footnotes = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="-1" w:type="separator"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="0" w:type="continuationSeparator"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> the note body</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        let rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
        zip_package(&[
            ("[Content_Types].xml", content_types),
            ("word/document.xml", &document(body)),
            ("word/footnotes.xml", footnotes),
            ("word/_rels/document.xml.rels", rels),
        ])
    }

    /// A relationship pointing at a part that is not in the package.
    pub fn with_dangling_relationship() -> Vec<u8> {
        let rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
        zip_package(&[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("word/document.xml", &document(r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>"#)),
            ("word/_rels/document.xml.rels", rels),
        ])
    }

    /// Write a fixture to `dir/name` and return the path as a String, which
    /// is what every `docx_*` command takes.
    pub fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
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
                page_width_pt: Some(595.0),
                page_height_pt: Some(842.0),
                left_margin_pt: Some(72.0),
                right_margin_pt: Some(72.0),
                top_margin_pt: Some(72.0),
                bottom_margin_pt: Some(72.0),
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
        assert_eq!(back.sections[0].page_width_pt, Some(595.0));
    }

    #[test]
    fn docx_section_with_unstated_geometry_serialises_as_null() {
        // A `w:sectPr` that omits `w:pgSz`/`w:pgMar` must stay
        // distinguishable from one that states a size.
        let b = DocxBlueprint {
            sections: vec![DocxSection {
                page_width_pt: None,
                page_height_pt: None,
                left_margin_pt: None,
                right_margin_pt: None,
                top_margin_pt: None,
                bottom_margin_pt: None,
                orientation: None,
            }],
            default_font: "Calibri".into(),
            default_font_size_pt: 11.0,
            style_count: 1,
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("\"page_width_pt\":null"));
        let back: DocxBlueprint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sections[0].page_width_pt, None);
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

    // ── Behaviour against authored fixtures (P30.1–P30.8) ─────────────
    //
    // Everything above this line is serde round-tripping, which would pass
    // just as well if every command returned an empty struct. These call
    // the commands on real packages and check what came out.

    use super::fixtures;
    use tempfile::TempDir;

    #[tokio::test]
    async fn check_passes_a_sound_package_and_names_what_it_checked() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "plain.docx", &fixtures::plain());
        let r = docx_check(path).await.unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert!(r.issues.is_empty());
        // A report with no `ok` lines is indistinguishable from a check that
        // never ran, so the caller gets the list of what passed.
        assert!(!r.ok.is_empty(), "no checks reported");
    }

    #[tokio::test]
    async fn check_reports_a_dangling_relationship_rather_than_erroring() {
        // A package that *parses* but points at a missing part is exactly
        // the case validation exists for: it must come back as an issue,
        // not as an Err (which the UI would show as "could not check").
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(
            dir.path(),
            "dangling.docx",
            &fixtures::with_dangling_relationship(),
        );
        let r = docx_check(path).await.expect("check should not error");
        assert!(!r.valid, "a missing rel target should not be valid");
        assert!(
            r.issues.iter().any(|i| i.contains("footnotes.xml")),
            "issues did not name the missing target: {:?}",
            r.issues
        );
    }

    #[tokio::test]
    async fn analyze_reads_page_geometry_in_points() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "bp.docx", &fixtures::blueprint_letter());
        let b = docx_analyze(path).await.unwrap();
        assert_eq!(b.sections.len(), 1, "{b:?}");
        // 12240 twips = 612 pt, 15840 = 792 pt (US Letter); 1440 twips = 72 pt.
        assert_eq!(b.sections[0].page_width_pt, Some(612.0));
        assert_eq!(b.sections[0].page_height_pt, Some(792.0));
        assert_eq!(b.sections[0].left_margin_pt, Some(72.0));
    }

    #[tokio::test]
    async fn analyze_reports_unstated_geometry_as_none_not_a_default() {
        // The whole reason these fields are Option: "the document says
        // nothing" must stay distinguishable from "the document says A4".
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(
            dir.path(),
            "nogeo.docx",
            &fixtures::blueprint_without_geometry(),
        );
        let b = docx_analyze(path).await.unwrap();
        let s = b.sections.first().expect("a sectPr is still a section");
        assert_eq!(s.page_width_pt, None, "invented a page width");
        assert_eq!(s.page_height_pt, None, "invented a page height");
        assert_eq!(s.top_margin_pt, None, "invented a margin");
    }

    #[tokio::test]
    async fn headings_are_inferred_from_bold_and_size_with_no_heading_styles() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "headed.docx", &fixtures::with_headings());
        let hs = docx_infer_headings(path).await.unwrap();
        assert_eq!(hs.len(), 2, "{hs:?}");
        assert_eq!(hs[0].text, "Chapter One");
        assert_eq!(hs[0].level, 1);
        assert_eq!(hs[1].text, "Section A");
        // The larger of the two bold sizes must outrank the smaller.
        assert!(hs[1].level > hs[0].level, "{hs:?}");
    }

    #[tokio::test]
    async fn a_document_without_headings_infers_none() {
        // The inverse assertion: inference that fires on ordinary prose
        // would poison the index with junk structure.
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "flat.docx", &fixtures::plain());
        assert!(docx_infer_headings(path).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn transplant_puts_source_text_into_the_blueprint_geometry() {
        let dir = TempDir::new().unwrap();
        let source = fixtures::write(dir.path(), "src.docx", &fixtures::source_two_paragraphs());
        let blueprint = fixtures::write(dir.path(), "bp.docx", &fixtures::blueprint_letter());
        let out = dir.path().join("out.docx").to_string_lossy().into_owned();

        let r = docx_transplant(source, blueprint, out.clone()).await.unwrap();
        assert_eq!(r.source_paragraphs, 2, "{r:?}");

        // Both halves of the claim: the body came from the source…
        let pkg = crisp_docx_core::open(std::path::Path::new(&out)).unwrap();
        let paras = crisp_docx_core::extract_paragraph_texts(&pkg).unwrap();
        let joined = paras.join("\n");
        assert!(joined.contains("source paragraph one"), "{joined:?}");
        assert!(
            !joined.contains("blueprint-only paragraph"),
            "blueprint body survived the transplant: {joined:?}"
        );
        // …and the page geometry came from the blueprint, not the source.
        let schema = crisp_docx_core::analyze_blueprint(&pkg).unwrap();
        assert_eq!(
            schema.sections.first().and_then(|s| s.page_width_pt),
            Some(612.0),
            "source page size leaked through"
        );
    }

    #[tokio::test]
    async fn notes_convert_to_endnotes_and_an_unknown_kind_is_refused() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "fn.docx", &fixtures::with_footnotes());
        let out = dir.path().join("en.docx").to_string_lossy().into_owned();

        docx_convert_notes(path.clone(), "endnotes".into(), out.clone())
            .await
            .unwrap();
        let pkg = crisp_docx_core::open(std::path::Path::new(&out)).unwrap();
        let names: Vec<&str> = pkg.parts().map(|(n, _)| n).collect();
        assert!(
            names.iter().any(|n| n.contains("endnotes.xml")),
            "no endnotes part: {names:?}"
        );
        let doc = String::from_utf8_lossy(
            pkg.get_part("word/document.xml").expect("document part"),
        )
        .into_owned();
        assert!(doc.contains("endnoteReference"), "body still references a footnote");

        // A typo must not silently pick a direction.
        assert!(docx_convert_notes(path, "sidenotes".into(), out).await.is_err());
    }

    #[tokio::test]
    async fn inline_markers_become_real_footnotes() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "markers.docx", &fixtures::with_inline_markers());
        let out = dir.path().join("noted.docx").to_string_lossy().into_owned();

        let mut notes = BTreeMap::new();
        notes.insert(1u32, "first note".to_string());
        notes.insert(2u32, "second note".to_string());
        let inserted = docx_inject_footnotes(path, notes, out.clone()).await.unwrap();
        assert_eq!(inserted, 2);

        let pkg = crisp_docx_core::open(std::path::Path::new(&out)).unwrap();
        let footnotes = String::from_utf8_lossy(
            pkg.get_part("word/footnotes.xml")
                .expect("footnotes part was not created"),
        )
        .into_owned();
        assert!(footnotes.contains("first note"), "{footnotes:?}");
        // The literal marker text must be gone from the body — a document
        // showing both "[1]" and a footnote mark is worse than either.
        let doc = String::from_utf8_lossy(pkg.get_part("word/document.xml").unwrap()).into_owned();
        assert!(!doc.contains("[1]"), "marker text left behind: {doc:?}");
        assert!(doc.contains("footnoteReference"), "no reference inserted");
    }

    #[tokio::test]
    async fn stripping_rsids_removes_them_and_keeps_the_text() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "rsid.docx", &fixtures::with_rsids());
        let out = dir.path().join("clean.docx").to_string_lossy().into_owned();

        let n = docx_strip_rsids(path, out.clone()).await.unwrap();
        assert!(n > 0, "reported stripping nothing");
        let pkg = crisp_docx_core::open(std::path::Path::new(&out)).unwrap();
        let doc = String::from_utf8_lossy(pkg.get_part("word/document.xml").unwrap()).into_owned();
        for attr in ["w:rsidR", "w:rsidRPr", "w:rsidP", "w14:paraId"] {
            assert!(!doc.contains(attr), "{attr} survived: {doc:?}");
        }
        assert!(doc.contains("tracked"), "content was lost with the rsids");
    }

    #[tokio::test]
    async fn quotes_are_curled_in_the_requested_style() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "q.docx", &fixtures::with_straight_quotes());
        let de = dir.path().join("de.docx").to_string_lossy().into_owned();
        let en = dir.path().join("en.docx").to_string_lossy().into_owned();

        docx_normalize_quotes(path.clone(), "german".into(), de.clone())
            .await
            .unwrap();
        docx_normalize_quotes(path, "english".into(), en.clone())
            .await
            .unwrap();

        let body = |p: &str| {
            let pkg = crisp_docx_core::open(std::path::Path::new(p)).unwrap();
            String::from_utf8_lossy(pkg.get_part("word/document.xml").unwrap()).into_owned()
        };
        let german = body(&de);
        let english = body(&en);
        assert!(german.contains('„'), "no German opener: {german:?}");
        assert!(english.contains('“'), "no English opener: {english:?}");
        assert_ne!(german, english, "both styles produced the same bytes");
        assert!(!german.contains("\"hello\""), "straight quotes survived");
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_docx_fails_with_a_message() {
        let dir = TempDir::new().unwrap();
        let path = fixtures::write(dir.path(), "not.docx", b"this is not a zip");
        let err = docx_check(path).await.expect_err("should not succeed");
        assert!(!err.is_empty(), "empty error message");
    }
}
