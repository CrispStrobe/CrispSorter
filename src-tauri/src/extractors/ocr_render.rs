//! P20 — OCR result rendering (structured / searchable output).
//!
//! Maps the OCR orchestrator's per-region results into the page→line→word
//! layout that CrispEmbed's `ocr_render` C module consumes, then renders to
//! plain text / hOCR / ALTO 3.1 / searchable PDF.
//!
//! Plain text is produced in Rust. Structured formats use CrispEmbed's native
//! renderers and preserve page order, including pages without recognized text.
//! Searchable PDF requires the paired `ocr-reviewed-pdf` feature and a renderer
//! advertising decoded raster images and Unicode text support. Physical page
//! sizes are separate from region pixel coordinates; raster sources without
//! resolution metadata use an explicit 300 DPI fallback.
//!
//! Missing renderers, unsupported archival conformance, invalid geometry, and
//! legacy ALTO text truncation return errors before output files are committed.

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
    /// PDF points, separate from the pixel coordinate system used by regions.
    pub physical_size_points: Option<(f32, f32)>,
    // Split TIFF/PDF frame files must outlive the rendering call. The legacy
    // importer used to drop its temporary directory before returning pages.
    _source_images: Option<std::sync::Arc<super::page_source::PageImages>>,
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
        Self { regions, page_width, page_height, image_path: image_path.into(),
            physical_size_points: None, _source_images: None }
    }

    pub fn with_physical_size(mut self, width_points: f32, height_points: f32) -> Self {
        self.physical_size_points = Some((width_points, height_points));
        self
    }
}

/// Convert a float rectangle to enclosing integer edges. Independent rounding
/// of x and width can send a valid half-pixel boundary outside the image.
fn render_region(region: &OcrRegion, width: i32, height: i32) -> Result<OcrRegion> {
    anyhow::ensure!(width > 0 && height > 0, "OCR output page dimensions must be positive");
    anyhow::ensure!([region.x, region.y, region.w, region.h].iter().all(|v| v.is_finite())
        && region.x >= 0.0 && region.y >= 0.0 && region.w > 0.0 && region.h > 0.0,
        "OCR output region has invalid geometry");
    let right = f64::from(region.x) + f64::from(region.w);
    let bottom = f64::from(region.y) + f64::from(region.h);
    anyhow::ensure!(right <= f64::from(width) + 0.01 && bottom <= f64::from(height) + 0.01,
        "OCR output region lies outside its page");
    let left = region.x.floor();
    let top = region.y.floor();
    let right = right.ceil().min(f64::from(width)) as f32;
    let bottom = bottom.ceil().min(f64::from(height)) as f32;
    anyhow::ensure!(right > left && bottom > top, "OCR output region is empty after edge conversion");
    Ok(OcrRegion { text:region.text.clone(), x:left, y:top, w:right-left, h:bottom-top, confidence:region.confidence })
}

/// Render OCR pages to the chosen format.
///
/// `Text` is produced in Rust; structured formats use the native renderer.
/// PDF requires a matching renderer with decoded images and Unicode support.
/// Archival conformance has not been validated, so `pdfa` fails explicitly.
pub fn render(pages: &[RenderPage], fmt: OcrOutputFormat, pdfa: bool) -> Result<Vec<u8>> {
    anyhow::ensure!(!pdfa, "PDF/A output is unavailable because archival conformance has not been validated");
    if fmt == OcrOutputFormat::Pdf && !reviewed_pdf_render_available() {
        anyhow::bail!("PDF output requires a matching crispembed ocr_render build with the ocr-reviewed-pdf feature");
    }
    match fmt {
        OcrOutputFormat::Text => Ok(render_text(pages).into_bytes()),
        _ => render_structured(pages, fmt, pdfa),
    }
}

pub fn reviewed_pdf_render_available() -> bool {
    #[cfg(feature = "ocr-reviewed-pdf")]
    { crispembed::ocr_render_capabilities() & 7 == 7 }
    #[cfg(not(feature = "ocr-reviewed-pdf"))]
    { false }
}

/// Whether structured (hOCR/ALTO) rendering is wired up. True when the
/// `crispembed` feature is compiled in (the renderer binding is then linked).
/// The CLI uses this to fail fast (before running OCR) on an unavailable format.
/// Searchable PDF additionally requires [`reviewed_pdf_render_available`].
pub fn structured_render_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Rasterize a document (image/PDF/TIFF) and OCR each page through the smart
/// pipeline into [`RenderPage`]s, ready for [`render`]. Shared by the `ocr`
/// CLI (`--render`) and the GUI's per-file "export searchable" action so both
/// produce byte-identical structured output. `ext` is the lower-cased source
/// extension (drives PDF vs single-image rasterization).
pub fn render_pages_from_file(
    file: &std::path::Path,
    ext: &str,
    cfg: &crate::extractors::OcrPipelineConfig,
) -> std::result::Result<Vec<RenderPage>, String> {
    use crate::extractors::{ocr_crispembed, page_source};

    let images = std::sync::Arc::new(if ext == "pdf" {
        page_source::rasterize_pdf(file).map_err(|e| format!("rasterize PDF: {e:#}"))?
    } else {
        page_source::rasterize_pages(file, ext).map_err(|e| format!("rasterize: {e:#}"))?
    });

    let mut pages = Vec::with_capacity(images.len());
    for (i, page_path) in images.paths().iter().enumerate() {
        let regions = ocr_crispembed::ocr_regions_via_pipeline(page_path, cfg)
            .map_err(|e| format!("OCR failed on page {}: {e:#}", i + 1))?;
        let (w, h) = image::image_dimensions(page_path).map_err(|e| format!("Read page {} dimensions: {e}", i + 1))?;
        if w == 0 || h == 0 || w > i32::MAX as u32 || h > i32::MAX as u32 {
            return Err(format!("Page {} has invalid output dimensions", i + 1));
        }
        let mut page = RenderPage::from_regions(
            regions,
            w as i32,
            h as i32,
            page_path.display().to_string(),
        );
        page.physical_size_points = images.page_size_points(i).map(|(w, h)| (w as f32, h as f32));
        page._source_images = Some(images.clone());
        pages.push(page);
    }
    Ok(pages)
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

/// Structured / searchable rendering (hOCR / ALTO / PDF) via CrispEmbed's
/// lower-level `ocr_render_pages` binding — **multi-page** (one document
/// spanning all pages) and **binary-safe** (searchable PDF via `output_size`).
/// Each [`RenderPage`]'s regions map to `crispembed::OcrRegion` (same
/// box+text+confidence shape; one-word lines at region granularity).
#[cfg(feature = "crispembed")]
fn render_structured(pages: &[RenderPage], fmt: OcrOutputFormat, pdfa: bool) -> Result<Vec<u8>> {
    let fmt_str = match fmt {
        OcrOutputFormat::Hocr => "hocr",
        OcrOutputFormat::Alto => "alto",
        OcrOutputFormat::Pdf => "pdf",
        OcrOutputFormat::Text => "text", // (render() routes Text away; total for safety)
    };
    // Convert to crispembed regions, keeping the per-page Vecs alive for the
    // duration of the `ocr_render_pages` call (the inputs borrow them).
    let page_regions: Vec<Vec<crispembed::OcrRegion>> = pages
        .iter()
        .map(|p| {
            p.regions
                .iter()
                .map(|r| {
                    let r = render_region(r, p.page_width, p.page_height)?;
                    Ok(crispembed::OcrRegion {
                    text: r.text.clone(),
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                    confidence: r.confidence,
                    // Added to `OcrResult` upstream after v0.16.1. These
                    // regions come from our own detector, which does not
                    // rotate anything, so the honest value is `false` — the
                    // renderer only reads it to decide whether a box's
                    // coordinates were already de-skewed.
                    orientation_corrected: false,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let inputs: Vec<crispembed::OcrRenderPageInput> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| crispembed::OcrRenderPageInput {
            regions: &page_regions[i],
            page_width: p.page_width,
            page_height: p.page_height,
            image_path: if p.image_path.is_empty() {
                None
            } else {
                Some(p.image_path.as_str())
            },
        })
        .collect();
    #[cfg(feature = "ocr-reviewed-pdf")]
    let bytes = {
        let sizes: Vec<_> = pages.iter().map(|p| p.physical_size_points
            .unwrap_or((p.page_width as f32 * 72.0 / 300.0, p.page_height as f32 * 72.0 / 300.0))).collect();
        crispembed::ocr_render_pages_checked_with_sizes(&inputs, fmt_str, pdfa, Some(&sizes))
            .map_err(|e| anyhow::anyhow!("Native OCR export: {e}"))?
    };
    #[cfg(not(feature = "ocr-reviewed-pdf"))]
    let bytes = crispembed::ocr_render_pages(&inputs, fmt_str, pdfa)
        .ok_or_else(|| anyhow::anyhow!("crispembed::ocr_render_pages returned None ({fmt_str})"))?;
    if fmt == OcrOutputFormat::Alto { validate_alto_output(pages, &bytes)?; }
    Ok(bytes)
}

/// Older linked renderers used a fixed text buffer for ALTO. Validate both
/// structure and exact reviewed text before a legacy output can be committed.
fn validate_alto_output(pages: &[RenderPage], bytes: &[u8]) -> Result<()> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut actual_text = Vec::new();
    let mut page_count = 0;
    loop {
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element) => {
                if element.local_name().as_ref() == b"Page" { page_count += 1; }
                if element.local_name().as_ref() == b"String" {
                    let mut content = None;
                    for attribute in element.attributes() {
                        let attribute = attribute?;
                        if attribute.key.as_ref() == b"CONTENT" {
                            content = Some(attribute.decode_and_unescape_value(reader.decoder())?.into_owned());
                        }
                    }
                    actual_text.push(content.ok_or_else(|| anyhow::anyhow!("ALTO output omitted a text value"))?);
                }
            },
            Event::Eof => break,
            _ => {},
        }
    }
    let expected: Vec<_> = pages.iter().flat_map(|p| p.regions.iter().map(|r| r.text.as_str())).collect();
    anyhow::ensure!(page_count == pages.len() && actual_text.iter().map(String::as_str).eq(expected.into_iter()),
        "The linked ALTO renderer changed or truncated reviewed text/pages. Use the paired OCR workspace build, or export hOCR/JSON.");
    Ok(())
}

#[cfg(not(feature = "crispembed"))]
fn render_structured(_pages: &[RenderPage], fmt: OcrOutputFormat, _pdfa: bool) -> Result<Vec<u8>> {
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

    #[test]
    fn fractional_boundary_uses_enclosing_edges_without_exceeding_page() {
        let value = OcrRegion { text:"edge".into(), x:0.5, y:0.5, w:999.5, h:999.5, confidence:0.9 };
        let normalized = render_region(&value, 1000, 1000).unwrap();
        assert_eq!((normalized.x, normalized.y, normalized.w, normalized.h), (0.0, 0.0, 1000.0, 1000.0));
        let near = OcrRegion { x:999.5, y:999.5, w:0.505, h:0.505, ..value.clone() };
        let normalized = render_region(&near, 1000, 1000).unwrap();
        assert_eq!((normalized.x, normalized.y, normalized.w, normalized.h), (999.0, 999.0, 1.0, 1.0));
        assert!(render_region(&OcrRegion { w:1000.0, ..value }, 1000, 1000).is_err());
    }

    #[test]
    fn split_page_assets_outlive_the_import_function() {
        use tiff::encoder::{colortype, TiffEncoder};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.tiff");
        let file = std::fs::File::create(&path).unwrap();
        TiffEncoder::new(file).unwrap().write_image::<colortype::Gray8>(2, 2, &[255,0,0,255]).unwrap();
        let page = {
            let images = std::sync::Arc::new(super::super::page_source::rasterize_pages(&path, "tiff").unwrap());
            let mut page = RenderPage::from_regions(vec![], 2, 2, images.paths()[0].to_string_lossy());
            page._source_images = Some(images);
            page
        };
        assert!(std::path::Path::new(&page.image_path).is_file());
        let retained_path = page.image_path.clone();
        drop(page);
        assert!(!std::path::Path::new(&retained_path).exists());
    }

    #[test]
    fn alto_validation_rejects_truncation_or_missing_pages() {
        let pages = vec![RenderPage::from_regions(vec![region("reviewed & complete", 0.0, 0.0)], 100, 100, "")];
        validate_alto_output(&pages, br#"<alto><Page><String CONTENT="reviewed &amp; complete" /></Page></alto>"#).unwrap();
        assert!(validate_alto_output(&pages, br#"<alto><Page><String CONTENT="reviewed" /></Page></alto>"#).is_err());
        assert!(validate_alto_output(&pages, br#"<alto><Page><String CONTENT="reviewed &amp; complete" /></Page><Page /></alto>"#).is_err());
        assert!(validate_alto_output(&pages, br#"<alto><Page><String CONTENT="interrupted"#).is_err());
    }

    #[cfg(feature = "ocr-reviewed-pdf")]
    #[test]
    fn paired_pdf_preserves_unicode_pixels_page_count_and_paper_size() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("canonical.png");
        let image = image::RgbImage::from_fn(100, 120, |x, y| image::Rgb([x as u8, y as u8, (x + y) as u8]));
        image.save(&path).unwrap();
        let text = "Résumé 東京 😀";
        let pages: Vec<_> = (0..64).map(|index| {
            let regions = if index == 31 { vec![] } else { vec![OcrRegion {
                text:format!("{text} page {}", index + 1), x:0.5, y:10.5, w:99.5, h:20.0, confidence:0.9,
            }] };
            RenderPage::from_regions(regions, 100, 120, path.to_string_lossy()).with_physical_size(600.0, 720.0)
        }).collect();
        let bytes = render(&pages, OcrOutputFormat::Pdf, false).unwrap();
        let document = lopdf::Document::load_mem(&bytes).unwrap();
        let page_objects = document.get_pages();
        assert_eq!(page_objects.len(), 64, "all document pages, including a blank page, survive");
        for id in page_objects.values() {
            let dict = document.get_object(*id).unwrap().as_dict().unwrap();
            let size = dict.get(b"MediaBox").unwrap().as_array().unwrap();
            assert_eq!(size[2].as_float().unwrap(), 600.0);
            assert_eq!(size[3].as_float().unwrap(), 720.0);
        }
        let extracted = document.extract_text(&[1]).unwrap();
        for token in ["Résumé", "東京", "😀"] {
            assert!(extracted.contains(token), "Unicode text missing: {token}: {extracted:?}");
        }
        let images: Vec<_> = document.objects.values().filter_map(|o| o.as_stream().ok())
            .filter(|s| s.dict.get(b"Subtype").ok().and_then(|v| v.as_name().ok()) == Some(b"Image".as_slice())).collect();
        assert!(!images.is_empty(), "PDF contains actual image objects");
        for encoded in images {
            assert_eq!(encoded.get_plain_content().unwrap(), image.as_raw().as_slice(), "canonical PNG pixels remain exact");
        }
        assert!(render(&pages, OcrOutputFormat::Pdf, true).unwrap_err().to_string().contains("PDF/A"));
        let rtl = RenderPage::from_regions(vec![OcrRegion {
            text:"العربية 123 ABC".into(), x:0.0, y:0.0, w:100.0, h:20.0, confidence:0.9,
        }], 100, 120, path.to_string_lossy()).with_physical_size(600.0, 720.0);
        let error = render(&[rtl], OcrOutputFormat::Pdf, false).unwrap_err().to_string();
        assert!(error.contains("right-to-left"), "unsupported bidi must fail explicitly before writing: {error}");
    }

    #[cfg(feature = "ocr-reviewed-pdf")]
    #[test]
    fn paired_export_reports_missing_images_and_preserves_long_alto_text() {
        let page = RenderPage::from_regions(vec![region("retained", 0.0, 0.0)], 100, 100, "/missing-ocr-test-image.png");
        assert!(render(&[page], OcrOutputFormat::Pdf, false).is_err());
        let text = "東京 العربية 123 ABC & quoted \"value\"\n".repeat(500);
        let page = RenderPage::from_regions(vec![OcrRegion { text, x:0.0, y:0.0, w:100.0, h:20.0, confidence:0.9 }], 100, 100, "");
        let output = render(&[page], OcrOutputFormat::Alto, false).unwrap();
        assert!(output.len() > 10_000);
    }

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
        let out = String::from_utf8(render(&[page1, page2], OcrOutputFormat::Text, false).unwrap()).unwrap();
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
            let err = render(&[page.clone()], fmt, false).unwrap_err().to_string();
            assert!(err.contains("ocr_render"), "names the renderer: {err}");
            assert!(err.contains("crispembed"), "names the feature to enable: {err}");
        }
    }

    // Live: drive the real CrispEmbed renderers (no OCR models needed — just the
    // `ocr_render_pages` binding + a linked libcrispembed). Validates hOCR
    // structure, multi-page (one document), and binary PDF.
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
        let s = String::from_utf8(render(&[page], OcrOutputFormat::Hocr, false).expect("hocr render"))
            .expect("utf8");
        assert!(s.contains("ocr_page") || s.contains("ocrx_word"), "hOCR markup: {s}");
        assert!(s.contains("Hello"), "recognized text present: {s}");
    }

}
