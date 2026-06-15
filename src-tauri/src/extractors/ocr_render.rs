//! P20 — OCR result rendering (structured / searchable output).
//!
//! Maps the OCR orchestrator's per-region results into the page→line→word
//! layout that CrispEmbed's `ocr_render` C module consumes, then renders to
//! plain text / hOCR / ALTO 3.1 / searchable PDF.
//!
//! ## Status (prepared 2026-06-15)
//!
//! Plain `Text` is produced here in Rust. **hOCR / ALTO** route to CrispEmbed's
//! C++ `ocr_render` renderer via `crispembed::ocr_render` (we keep rendering in
//! C++ per the "keep it all in cpp" directive). **Searchable PDF** is gated: the
//! current `crispembed::ocr_render` returns `Option<String>`, which truncates a
//! binary PDF at the first NUL byte — PDF needs a size-aware (`Vec<u8>`) binding
//! over `ocr_render.h`'s `output_size` API (tracked follow-up). Without the
//! `crispembed` feature, all structured formats return a clear error.
//!
//! Everything else (region extraction via
//! [`super::ocr_crispembed::ocr_regions_via_pipeline`], the page-mapping, the
//! format surface, the text renderer, and the CLI `--format`/`--out` flags) is
//! complete and tested, so wiring the renderer in is a localized follow-up.

use anyhow::Result;

/// Form-feed page separator (matches `page_source::PAGE_SEPARATOR`).
const PAGE_SEPARATOR: &str = "\u{000C}";

/// One recognized region (bounding box + text + confidence) from the OCR
/// orchestrator. Mirrors `crispembed::OcrPipelineRegion`, but kept local so the
/// renderer + non-crispembed builds don't depend on the CrispEmbed crate type.
#[derive(Debug, Clone)]
pub struct OcrRegion {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub confidence: f32,
}

/// Output format for OCR results. Mirrors `ocr_render_format` in
/// `CrispEmbed/src/ocr_render.h` (Text=0, Hocr=1, Alto=2, Pdf=3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrOutputFormat {
    Text,
    Hocr,
    Alto,
    Pdf,
}

impl OcrOutputFormat {
    /// Parse a case-insensitive format name (`text`/`txt`, `hocr`, `alto`, `pdf`).
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "text" | "txt" | "plain" => Some(Self::Text),
            "hocr" => Some(Self::Hocr),
            "alto" | "xml" => Some(Self::Alto),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }

    /// The CrispEmbed `ocr_render_format` enum value this maps to.
    pub fn c_format(self) -> i32 {
        match self {
            Self::Text => 0,
            Self::Hocr => 1,
            Self::Alto => 2,
            Self::Pdf => 3,
        }
    }

    /// File extension for the rendered artifact.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Hocr => "hocr",
            Self::Alto => "xml",
            Self::Pdf => "pdf",
        }
    }

    /// Structured formats need CrispEmbed's renderer; `Text` is produced here.
    pub fn needs_render_binding(self) -> bool {
        !matches!(self, Self::Text)
    }

    /// Whether the output is binary (PDF) and must be written to `--out`
    /// rather than printed to a terminal.
    pub fn is_binary(self) -> bool {
        matches!(self, Self::Pdf)
    }
}

/// A page of OCR regions for rendering. Mirrors `ocr_render_page` — we use
/// region-level granularity (each region becomes one line with one word, which
/// is faithful for the orchestrator's region-level output).
#[derive(Debug, Clone)]
pub struct RenderPage {
    pub regions: Vec<OcrRegion>,
    pub page_width: i32,
    pub page_height: i32,
    /// Original image path (used by the searchable-PDF renderer for the image
    /// layer); empty when unavailable.
    pub image_path: String,
}

impl RenderPage {
    /// Build a render page from orchestrator regions + the page image's
    /// dimensions. `image_path` may be empty for non-PDF formats.
    pub fn from_regions(
        regions: Vec<OcrRegion>,
        page_width: i32,
        page_height: i32,
        image_path: impl Into<String>,
    ) -> Self {
        Self { regions, page_width, page_height, image_path: image_path.into() }
    }
}

/// Render OCR pages to the chosen format.
///
/// `Text` is produced in Rust now. `Hocr`/`Alto`/`Pdf` route to CrispEmbed's
/// `ocr_render` C module — pending its Rust binding (see module docs), so they
/// return a clear, actionable error until that binding lands.
pub fn render(pages: &[RenderPage], fmt: OcrOutputFormat) -> Result<Vec<u8>> {
    match fmt {
        OcrOutputFormat::Text => Ok(render_text(pages).into_bytes()),
        _ => render_structured(pages, fmt),
    }
}

/// Whether structured (hOCR/ALTO) rendering is wired up. True when the
/// `crispembed` feature is compiled in (the renderer binding is then linked).
/// The CLI uses this to fail fast (before running OCR) on an unavailable format.
/// Note: searchable PDF is still gated inside [`render`] pending a size-aware
/// binding — see the module docs.
pub fn structured_render_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Plain-text rendering: regions joined by newlines, pages by form-feed.
fn render_text(pages: &[RenderPage]) -> String {
    let mut out = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 {
            out.push_str(PAGE_SEPARATOR);
        }
        for (j, r) in page.regions.iter().enumerate() {
            if j > 0 {
                out.push('\n');
            }
            out.push_str(r.text.trim_end_matches('\n'));
        }
    }
    out
}

/// Structured rendering (hOCR / ALTO) via CrispEmbed's `ocr_render`.
///
/// Each [`RenderPage`]'s regions map to `crispembed::OcrRegion` (same
/// box+text+confidence shape) and render via the one-shot `crispembed::ocr_render`.
/// Multi-page docs render one document per page and concatenate (single-document
/// multi-page output would need the lower-level `add_page` API). Searchable PDF
/// is gated — see module docs.
#[cfg(feature = "crispembed")]
fn render_structured(pages: &[RenderPage], fmt: OcrOutputFormat) -> Result<Vec<u8>> {
    if matches!(fmt, OcrOutputFormat::Pdf) {
        anyhow::bail!(
            "searchable-PDF output isn't available via the current Rust binding: \
             `crispembed::ocr_render` returns Option<String>, which truncates the \
             binary PDF at the first NUL. It needs a size-aware (Vec<u8>) binding \
             over ocr_render.h's output_size API. hOCR / ALTO / text work today."
        );
    }
    let fmt_str = match fmt {
        OcrOutputFormat::Hocr => "hocr",
        OcrOutputFormat::Alto => "alto",
        _ => unreachable!("Text handled by render(); Pdf handled above"),
    };
    let mut out = String::new();
    let mut rendered_pages = 0usize;
    for (i, page) in pages.iter().enumerate() {
        if page.regions.is_empty() {
            continue;
        }
        let regions: Vec<crispembed::OcrRegion> = page
            .regions
            .iter()
            .map(|r| crispembed::OcrRegion {
                text: r.text.clone(),
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                confidence: r.confidence,
            })
            .collect();
        let doc = crispembed::ocr_render(&regions, page.page_width, page.page_height, fmt_str)
            .ok_or_else(|| anyhow::anyhow!("crispembed::ocr_render returned null (page {})", i + 1))?;
        if rendered_pages > 0 {
            out.push('\n');
        }
        out.push_str(&doc);
        rendered_pages += 1;
    }
    if rendered_pages > 1 {
        eprintln!(
            "[ocr_render] {rendered_pages} pages rendered as concatenated {fmt_str} \
             documents; single-document multi-page output needs the lower-level \
             ocr_render add_page binding (follow-up)."
        );
    }
    Ok(out.into_bytes())
}

#[cfg(not(feature = "crispembed"))]
fn render_structured(_pages: &[RenderPage], fmt: OcrOutputFormat) -> Result<Vec<u8>> {
    anyhow::bail!(
        "{:?} output needs CrispEmbed's `ocr_render` renderer — rebuild with the \
         `crispembed` feature (e.g. --features crispembed-metal). `--render text` \
         works without it.",
        fmt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(text: &str, x: f32, y: f32) -> OcrRegion {
        OcrRegion { text: text.into(), x, y, w: 50.0, h: 12.0, confidence: 0.9 }
    }

    #[test]
    fn format_parsing_is_case_insensitive() {
        assert_eq!(OcrOutputFormat::from_name("TEXT"), Some(OcrOutputFormat::Text));
        assert_eq!(OcrOutputFormat::from_name("txt"), Some(OcrOutputFormat::Text));
        assert_eq!(OcrOutputFormat::from_name("hOCR"), Some(OcrOutputFormat::Hocr));
        assert_eq!(OcrOutputFormat::from_name("alto"), Some(OcrOutputFormat::Alto));
        assert_eq!(OcrOutputFormat::from_name("xml"), Some(OcrOutputFormat::Alto));
        assert_eq!(OcrOutputFormat::from_name("pdf"), Some(OcrOutputFormat::Pdf));
        assert_eq!(OcrOutputFormat::from_name("docx"), None);
    }

    #[test]
    fn format_metadata() {
        assert_eq!(OcrOutputFormat::Text.c_format(), 0);
        assert_eq!(OcrOutputFormat::Pdf.c_format(), 3);
        assert_eq!(OcrOutputFormat::Hocr.ext(), "hocr");
        assert_eq!(OcrOutputFormat::Alto.ext(), "xml");
        assert!(OcrOutputFormat::Pdf.is_binary());
        assert!(!OcrOutputFormat::Hocr.is_binary());
        assert!(!OcrOutputFormat::Text.needs_render_binding());
        assert!(OcrOutputFormat::Alto.needs_render_binding());
    }

    #[test]
    fn text_render_joins_regions_and_pages() {
        let page1 = RenderPage::from_regions(
            vec![region("Hello", 0.0, 0.0), region("world", 0.0, 20.0)],
            600,
            800,
            "",
        );
        let page2 = RenderPage::from_regions(vec![region("Second page", 0.0, 0.0)], 600, 800, "");
        let out = String::from_utf8(render(&[page1, page2], OcrOutputFormat::Text).unwrap()).unwrap();
        assert_eq!(out, "Hello\nworld\u{000C}Second page");
    }

    #[test]
    fn structured_availability_tracks_crispembed_feature() {
        assert_eq!(structured_render_available(), cfg!(feature = "crispembed"));
    }

    // Without the crispembed feature, structured formats bail with an actionable
    // message naming the renderer + the feature to enable.
    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn structured_formats_need_crispembed_feature() {
        assert!(!structured_render_available());
        let page = RenderPage::from_regions(vec![region("x", 0.0, 0.0)], 100, 100, "/tmp/x.png");
        for fmt in [OcrOutputFormat::Hocr, OcrOutputFormat::Alto, OcrOutputFormat::Pdf] {
            let err = render(&[page.clone()], fmt).unwrap_err().to_string();
            assert!(err.contains("ocr_render"), "names the renderer: {err}");
            assert!(err.contains("crispembed"), "names the feature to enable: {err}");
        }
    }

    // With crispembed, PDF stays gated (binary truncation) with a clear reason.
    #[cfg(feature = "crispembed")]
    #[test]
    fn pdf_gated_with_crispembed() {
        let page = RenderPage::from_regions(vec![region("x", 0.0, 0.0)], 100, 100, "/tmp/x.png");
        let err = render(&[page], OcrOutputFormat::Pdf).unwrap_err().to_string();
        assert!(err.contains("PDF") || err.contains("Pdf"), "explains PDF gating: {err}");
    }

    // Live: drive the real CrispEmbed renderer (no OCR models needed — just the
    // `ocr_render` binding + a linked libcrispembed). Validates hOCR structure.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal hocr_render_live -- --ignored
    fn hocr_render_live() {
        let page = RenderPage::from_regions(
            vec![OcrRegion {
                text: "Hello".into(),
                x: 10.0, y: 20.0, w: 80.0, h: 30.0,
                confidence: 0.95,
            }],
            600,
            800,
            "",
        );
        let s = String::from_utf8(render(&[page], OcrOutputFormat::Hocr).expect("hocr render"))
            .expect("utf8");
        assert!(s.contains("ocr_page") || s.contains("ocrx_word"), "hOCR markup: {s}");
        assert!(s.contains("Hello"), "recognized text present: {s}");
    }
}
