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

/// Split a TIFF into one PNG per frame. Every frame must decode; a partially
/// decoded document is an error rather than an apparently successful import.
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
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 100_000_000 {
            anyhow::bail!("TIFF page {} exceeds the 100 megapixel limit", idx + 1);
        }
        if idx >= 10_000 { anyhow::bail!("TIFF exceeds the 10000 page limit"); }
        let color = dec.colortype().context("TIFF colortype")?;
        let data = dec.read_image().context("TIFF read_image")?;

        // Convert common 8/16-bit scan types. Reject the entire import for
        // unsupported frames: skipping one would silently change page numbers
        // and could misassociate review decisions with the wrong source page.
        let dynimg: Option<DynamicImage> = match (color, data) {
            (ColorType::Gray(8), DecodingResult::U8(buf)) =>
                GrayImage::from_raw(w, h, buf).map(DynamicImage::ImageLuma8),
            (ColorType::RGB(8), DecodingResult::U8(buf)) =>
                RgbImage::from_raw(w, h, buf).map(DynamicImage::ImageRgb8),
            (ColorType::RGBA(8), DecodingResult::U8(buf)) =>
                RgbaImage::from_raw(w, h, buf).map(DynamicImage::ImageRgba8),
            (ColorType::Gray(16), DecodingResult::U16(buf)) =>
                GrayImage::from_raw(w, h, buf.into_iter().map(|v| (v >> 8) as u8).collect()).map(DynamicImage::ImageLuma8),
            (ColorType::RGB(16), DecodingResult::U16(buf)) =>
                RgbImage::from_raw(w, h, buf.into_iter().map(|v| (v >> 8) as u8).collect()).map(DynamicImage::ImageRgb8),
            (ColorType::RGBA(16), DecodingResult::U16(buf)) =>
                RgbaImage::from_raw(w, h, buf.into_iter().map(|v| (v >> 8) as u8).collect()).map(DynamicImage::ImageRgba8),
            _ => None,
        };
        let img = dynimg.ok_or_else(|| anyhow::anyhow!(
            "TIFF {} page {} has unsupported color data ({color:?}); no pages were imported",
            path.display(), idx + 1,
        ))?;
        let out = tmp.path().join(format!("page_{idx}.png"));
        img.save(&out).with_context(|| format!("saving TIFF page {}", idx + 1))?;
        paths.push(out);
        idx += 1;

        if dec.more_images() {
            dec.next_image().context("TIFF next_image")?;
        } else {
            break;
        }
    }

    if paths.is_empty() { anyhow::bail!("TIFF contains no readable pages"); }
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

    let tmp = tempfile::tempdir().context("temp dir for PDF pages")?;
    let mut paths: Vec<PathBuf> = Vec::new();
    let path_str = path.to_string_lossy();

    for (i, page) in doc.pages().iter().enumerate() {
        // Pick an OCR-optimal render DPI per page: profile the page's embedded
        // raster resolution (crispembed) and clamp to the OCR-useful range,
        // falling back to ~200 DPI. Render pixel-width = page width (points)
        // × DPI / 72. This avoids upsampling a low-res scan into a huge image
        // and avoids losing detail on a high-res one (vs the old fixed 1654 px).
        let dpi = ocr_render_dpi(&path_str, i as i32);
        let w_points = page.width().value; // PdfPoints, 1/72"
        let target_w = ((w_points * dpi / 72.0).round() as i32).clamp(800, 6000);
        let cfg = PdfRenderConfig::new().set_target_width(target_w);
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

/// Choose an OCR render DPI for a PDF page. With `crispembed`, profile the
/// page's mean embedded-image DPI (`crispembed::pdf_page_dpi`) and clamp to the
/// OCR-useful range [150, 400] — rendering above the source DPI just
/// interpolates (no real gain), below loses detail. Falls back to ~200 DPI when
/// the page has no raster images (vector/text PDF) or profiling fails.
#[cfg(all(feature = "pdf-render", feature = "crispembed"))]
fn ocr_render_dpi(path_str: &str, page: i32) -> f32 {
    match crispembed::pdf_page_dpi(path_str, page) {
        Ok((dpi, n)) if n > 0 && dpi >= 72.0 => dpi.clamp(150.0, 400.0),
        _ => 200.0,
    }
}

#[cfg(all(feature = "pdf-render", not(feature = "crispembed")))]
fn ocr_render_dpi(_path_str: &str, _page: i32) -> f32 {
    200.0
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

    #[test]
    fn mixed_bit_depth_tiff_preserves_every_page_in_order() {
        use tiff::encoder::{colortype, TiffEncoder};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mixed.tiff");
        {
            let mut file = std::fs::File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(&mut file).unwrap();
            encoder.write_image::<colortype::Gray8>(4, 4, &[64u8; 16]).unwrap();
            encoder.write_image::<colortype::Gray16>(4, 4, &[32_768u16; 16]).unwrap();
            encoder.write_image::<colortype::RGB16>(4, 4, &[65_535u16; 48]).unwrap();
        }
        let pages = rasterize_pages(&path, "tiff").unwrap();
        assert_eq!(pages.len(), 3);
        let intensities: Vec<_> = pages.paths().iter().map(|p| image::open(p).unwrap().to_luma8().get_pixel(0, 0)[0]).collect();
        assert_eq!(intensities, vec![64, 128, 255]);
    }

    #[test]
    fn unsupported_tiff_frame_rejects_complete_import() {
        use tiff::encoder::{colortype, TiffEncoder};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("float.tiff");
        {
            let mut file = std::fs::File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(&mut file).unwrap();
            encoder.write_image::<colortype::Gray8>(4, 4, &[64u8; 16]).unwrap();
            encoder.write_image::<colortype::Gray32Float>(4, 4, &[0.5f32; 16]).unwrap();
        }
        let error = rasterize_pages(&path, "tiff").err().expect("unsupported second page must not disappear");
        assert!(error.to_string().contains("page 2"), "{error}");
    }
}
