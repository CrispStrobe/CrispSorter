//! Multi-page page sourcing for OCR.
//!
//! Decodes a document file into **one image per page** so the OCR dispatch can
//! run the pipeline per page and concatenate. Multi-page **TIFF** scans are
//! split via the pure-Rust `tiff` crate (each frame → a temp PNG). Single-page
//! image formats return the original path unchanged (zero-copy). Multi-page
//! **PDF** rasterization is a follow-up (see PLAN — `pdfium-render`); for now a
//! PDF returns no pages here and the caller keeps its text-layer/legacy path.
//!
//! `image` decodes only the first TIFF frame, so multi-frame handling uses the
//! `tiff` decoder directly and re-encodes each frame to PNG via `image`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Per-page rendered images. Holds the `TempDir` so split pages outlive use.
pub struct PageImages {
    paths: Vec<PathBuf>,
    _tmp: Option<tempfile::TempDir>,
}

impl PageImages {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    pub fn len(&self) -> usize {
        self.paths.len()
    }
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
    /// The single original path (no decode) — the safe fallback.
    pub fn single(path: &Path) -> Self {
        PageImages { paths: vec![path.to_path_buf()], _tmp: None }
    }
}

/// Decode `path` into per-page image files. Multi-page TIFF is split into temp
/// PNGs; every other format returns the original path as a single page.
pub fn rasterize_pages(path: &Path, ext: &str) -> Result<PageImages> {
    match ext {
        "tif" | "tiff" => tiff_pages(path),
        _ => Ok(PageImages::single(path)),
    }
}

/// Split a (possibly multi-page) TIFF into per-frame temp PNGs. Returns the
/// single original path when the TIFF has only one frame (avoids a needless
/// decode→encode round-trip).
fn tiff_pages(path: &Path) -> Result<PageImages> {
    use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
    use tiff::decoder::{Decoder, DecodingResult};
    use tiff::ColorType;

    let file = std::fs::File::open(path)
        .with_context(|| format!("opening TIFF {}", path.display()))?;
    let mut dec = Decoder::new(std::io::BufReader::new(file)).context("TIFF decoder")?;

    let tmp = tempfile::tempdir().context("temp dir for TIFF pages")?;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut idx = 0usize;

    loop {
        let (w, h) = dec.dimensions().context("TIFF dimensions")?;
        let color = dec.colortype().context("TIFF colortype")?;
        let data = dec.read_image().context("TIFF read_image")?;

        // Convert the frame to an 8-bit DynamicImage for PNG encoding. Handle
        // the common scan color types; bail on exotic ones (caller falls back).
        let dynimg: Option<DynamicImage> = match (color, data) {
            (ColorType::Gray(8), DecodingResult::U8(buf)) => {
                GrayImage::from_raw(w, h, buf).map(DynamicImage::ImageLuma8)
            }
            (ColorType::RGB(8), DecodingResult::U8(buf)) => {
                RgbImage::from_raw(w, h, buf).map(DynamicImage::ImageRgb8)
            }
            (ColorType::RGBA(8), DecodingResult::U8(buf)) => {
                RgbaImage::from_raw(w, h, buf).map(DynamicImage::ImageRgba8)
            }
            _ => None, // 16-bit / palette / CMYK etc. — skip this frame
        };

        if let Some(img) = dynimg {
            let out = tmp.path().join(format!("page_{idx}.png"));
            img.save(&out)
                .with_context(|| format!("saving TIFF page {idx}"))?;
            paths.push(out);
            idx += 1;
        } else {
            eprintln!(
                "[page_source] TIFF {} frame {idx}: unsupported color type {color:?}; skipped",
                path.display()
            );
        }

        if dec.more_images() {
            dec.next_image().context("TIFF next_image")?;
        } else {
            break;
        }
    }

    // Single frame (the common case) → just use the original file; no temp.
    if paths.len() <= 1 {
        return Ok(PageImages::single(path));
    }
    Ok(PageImages { paths, _tmp: Some(tmp) })
}

/// Rasterize a PDF into one PNG per page (for scanned / image-only PDFs).
/// Requires the `pdf-render` feature (PDFium, bound at runtime). Returns an
/// error when the feature is off or no libpdfium is available — the caller
/// then keeps its text-layer / legacy fallback.
#[cfg(feature = "pdf-render")]
pub fn rasterize_pdf(path: &Path) -> Result<PageImages> {
    use pdfium_render::prelude::*;

    // Bind PDFium: prefer a libpdfium shipped with the app, else the
    // system-installed library. We stage libpdfium into `bin/` (a bundled
    // resource → `resources/bin/`, alongside the llama-server sidecar), so the
    // candidate dirs cover every platform's bundle layout relative to the exe:
    //   - the exe dir itself (Windows portable .zip lays DLLs next to the .exe)
    //   - `resources/bin` under the exe (Linux .deb / generic)
    //   - `../Resources/resources/bin` + `../Frameworks` (macOS .app)
    //   - `../lib` (FHS-style installs)
    let bindings = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
        .and_then(|dir| {
            let candidates = [
                dir.clone(),
                dir.join("resources").join("bin"),
                dir.join("..").join("Resources").join("resources").join("bin"),
                dir.join("..").join("Frameworks"),
                dir.join("..").join("Resources"),
                dir.join("..").join("lib"),
            ];
            candidates.iter().find_map(|d| {
                let name = Pdfium::pdfium_platform_library_name_at_path(d.as_path());
                Pdfium::bind_to_library(name).ok()
            })
        })
        .or_else(|| Pdfium::bind_to_system_library().ok())
        .context("no libpdfium found (install it or use the bundled release lib)")?;
    let pdfium = Pdfium::new(bindings);

    let doc = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("loading PDF {}", path.display()))?;

    let cfg = PdfRenderConfig::new().set_target_width(1654); // ~200 DPI on A4 width
    let tmp = tempfile::tempdir().context("temp dir for PDF pages")?;
    let mut paths: Vec<PathBuf> = Vec::new();

    for (i, page) in doc.pages().iter().enumerate() {
        let bitmap = page
            .render_with_config(&cfg)
            .with_context(|| format!("rendering PDF page {i}"))?;
        let w = bitmap.width() as u32;
        let h = bitmap.height() as u32;
        // RGBA bytes are version-independent of pdfium-render's `image` dep.
        let rgba = bitmap.as_rgba_bytes();
        let img = image::RgbaImage::from_raw(w, h, rgba)
            .ok_or_else(|| anyhow::anyhow!("PDF page {i}: bad bitmap buffer"))?;
        let out = tmp.path().join(format!("page_{i}.png"));
        image::DynamicImage::ImageRgba8(img)
            .save(&out)
            .with_context(|| format!("saving PDF page {i}"))?;
        paths.push(out);
    }
    if paths.is_empty() {
        anyhow::bail!("PDF has no renderable pages: {}", path.display());
    }
    Ok(PageImages { paths, _tmp: Some(tmp) })
}

#[cfg(not(feature = "pdf-render"))]
pub fn rasterize_pdf(_path: &Path) -> Result<PageImages> {
    anyhow::bail!("PDF rasterization requires the `pdf-render` cargo feature")
}

/// Page separator inserted between concatenated pages of a multi-page doc.
/// Form feed (U+000C) is the conventional page break and survives FTS/tantivy
/// tokenization as whitespace.
pub const PAGE_SEPARATOR: &str = "\u{000C}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_tiff_is_single_page() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        let pages = rasterize_pages(&p, "png").unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages.paths()[0], p);
    }

    #[test]
    fn single_helper_returns_self() {
        let p = Path::new("/tmp/x.jpg");
        let pages = PageImages::single(p);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages.paths()[0], p);
    }

    /// Live: rasterize a real PDF (path via `$CS_TEST_PDF`) and assert every
    /// page renders to a decodable PNG. Skips cleanly when the env var is unset
    /// (matches the cb-api live-test convention). Needs the `pdf-render` feature
    /// + a libpdfium on the system / next to the test binary.
    #[cfg(feature = "pdf-render")]
    #[test]
    #[ignore] // cargo test --features pdf-render pdf_rasterize_live -- --ignored
    fn pdf_rasterize_live() {
        let Ok(pdf) = std::env::var("CS_TEST_PDF") else {
            eprintln!("CS_TEST_PDF unset; skipping live PDF rasterize test");
            return;
        };
        let pages = rasterize_pdf(Path::new(&pdf)).expect("rasterize PDF");
        assert!(!pages.is_empty(), "PDF produced ≥1 page");
        for p in pages.paths() {
            assert!(p.exists(), "page image written: {}", p.display());
            let img = image::open(p).expect("page PNG decodes");
            assert!(img.width() > 0 && img.height() > 0, "non-empty page bitmap");
        }
        println!("rasterized {} page(s) from {pdf}", pages.len());
    }

    #[test]
    fn multipage_tiff_splits_into_frames() {
        // Build a 2-frame grayscale TIFF and assert it splits into 2 pages.
        use tiff::encoder::{colortype, TiffEncoder};
        let tmp = tempfile::tempdir().unwrap();
        let tpath = tmp.path().join("multi.tiff");
        {
            let mut f = std::fs::File::create(&tpath).unwrap();
            let mut enc = TiffEncoder::new(&mut f).unwrap();
            for _ in 0..2 {
                let px = vec![200u8; 16 * 16];
                enc.write_image::<colortype::Gray8>(16, 16, &px).unwrap();
            }
        }
        let pages = rasterize_pages(&tpath, "tiff").unwrap();
        assert_eq!(pages.len(), 2, "2-frame TIFF → 2 pages");
        for p in pages.paths() {
            assert!(p.exists(), "page image written: {}", p.display());
        }
    }
}
