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
        let fill_order = dec.find_tag_unsigned::<u16>(tiff::tags::Tag::FillOrder)
            .context("TIFF FillOrder tag")?.unwrap_or(1);
        if fill_order != 1 {
            anyhow::bail!("TIFF page {} uses unsupported bit fill order {fill_order}; no pages were imported", idx + 1);
        }
        let orientation = dec.find_tag_unsigned::<u16>(tiff::tags::Tag::Orientation)
            .context("TIFF Orientation tag")?.unwrap_or(1);
        if !(1..=8).contains(&orientation) {
            anyhow::bail!("TIFF page {} has invalid orientation {orientation}; no pages were imported", idx + 1);
        }
        let color = dec.colortype().context("TIFF colortype")?;
        let data = dec.read_image().context("TIFF read_image")?;

        // Convert common 8/16-bit scan types. Reject the entire import for
        // unsupported frames: skipping one would silently change page numbers
        // and could misassociate review decisions with the wrong source page.
        let dynimg: Option<DynamicImage> = match (color, data) {
            (ColorType::Gray(bits @ (1 | 2 | 4)), DecodingResult::U8(buf)) =>
                unpack_gray(w, h, bits, &buf).map(DynamicImage::ImageLuma8),
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
        // TIFF frame orientation is lost when writing the temporary PNG, so
        // normalize each frame now. The workspace must not apply it a second time.
        let img = match orientation {
            2 => img.fliph(), 3 => img.rotate180(), 4 => img.flipv(),
            5 => img.rotate90().fliph(), 6 => img.rotate90(),
            7 => img.rotate90().flipv(), 8 => img.rotate270(), _ => img,
        };
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

/// TIFF 0.11 returns packed MSB-first sub-byte grayscale samples, padded at
/// each row boundary. The decoder already normalizes WhiteIsZero photometry.
fn unpack_gray(width: u32, height: u32, bits: u8, bytes: &[u8]) -> Option<image::GrayImage> {
    if ![1, 2, 4].contains(&bits) { return None; }
    let stride = (u64::from(width) * u64::from(bits) + 7) / 8;
    if stride.checked_mul(u64::from(height))? != bytes.len() as u64 { return None; }
    let maximum = (1u8 << bits) - 1;
    Some(image::GrayImage::from_fn(width, height, |x, y| {
        let bit = u64::from(x) * u64::from(bits);
        let packed = bytes[(u64::from(y) * stride + bit / 8) as usize];
        let sample = (packed >> (8 - u64::from(bits) - bit % 8)) & maximum;
        image::Luma([(u16::from(sample) * 255 / u16::from(maximum)) as u8])
    }))
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
    let page_count = doc.pages().len() as usize;
    if page_count == 0 || page_count > 10_000 {
        anyhow::bail!("A PDF import must contain between 1 and 10000 pages");
    }

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
        let target_w = pdf_render_width(page.width().value, page.height().value, dpi)
            .with_context(|| format!("PDF page {}", i + 1))?;
        let cfg = PdfRenderConfig::new().set_target_width(target_w);
        let bitmap = page
            .render_with_config(&cfg)
            .with_context(|| format!("rendering PDF page {i}"))?;
        let (w, h) = (bitmap.width(), bitmap.height());
        if w <= 0 || h <= 0 || w as u64 * h as u64 > 100_000_000 {
            anyhow::bail!("PDF page {} rendered invalid or excessive dimensions: {w} × {h}", i + 1);
        }
        let (w, h) = (w as u32, h as u32);
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

#[cfg(any(feature = "pdf-render", test))]
fn pdf_render_width(width_points: f32, height_points: f32, dpi: f32) -> Result<i32> {
    if ![width_points, height_points, dpi].iter().all(|v| v.is_finite() && *v > 0.0) {
        anyhow::bail!("invalid PDF page dimensions or rendering resolution");
    }
    let target_width = ((f64::from(width_points) * f64::from(dpi) / 72.0).round() as i32).clamp(800, 6000);
    let target_height = (f64::from(height_points) * f64::from(target_width) / f64::from(width_points)).ceil();
    if !target_height.is_finite() || target_height < 1.0 || f64::from(target_width) * target_height > 100_000_000.0 {
        anyhow::bail!("PDF page would exceed the 100 megapixel limit at the selected rendering resolution");
    }
    Ok(target_width)
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

    // Procedural 13×3 CCITT Group4 scan, encoded with libtiff; no external
    // document content. Each row has five padding bits to catch row drift.
    const FAX4_SCAN: &[u8] = &[
        0x49,0x49,0x2a,0x00,0x1a,0x00,0x00,0x00,0x26,0xa8,0xbe,0x47,0x44,0x74,0x15,0x82,
        0xa4,0x92,0x08,0x15,0x62,0x38,0x8e,0x00,0x20,0x02,0x0a,0x00,0x00,0x01,0x03,0x00,
        0x01,0x00,0x00,0x00,0x0d,0x00,0x00,0x00,0x01,0x01,0x03,0x00,0x01,0x00,0x00,0x00,
        0x03,0x00,0x00,0x00,0x02,0x01,0x03,0x00,0x01,0x00,0x00,0x00,0x01,0x00,0x00,0x00,
        0x03,0x01,0x03,0x00,0x01,0x00,0x00,0x00,0x04,0x00,0x00,0x00,0x06,0x01,0x03,0x00,
        0x01,0x00,0x00,0x00,0x01,0x00,0x00,0x00,0x11,0x01,0x04,0x00,0x01,0x00,0x00,0x00,
        0x08,0x00,0x00,0x00,0x12,0x01,0x03,0x00,0x01,0x00,0x00,0x00,0x01,0x00,0x00,0x00,
        0x16,0x01,0x03,0x00,0x01,0x00,0x00,0x00,0x03,0x00,0x00,0x00,0x17,0x01,0x04,0x00,
        0x01,0x00,0x00,0x00,0x12,0x00,0x00,0x00,0x1c,0x01,0x03,0x00,0x01,0x00,0x00,0x00,
        0x01,0x00,0x00,0x00,0x00,0x00,0x00,0x00,
    ];

    fn fax4_with_tag(tag: u16, value: u16) -> Vec<u8> {
        let mut bytes = FAX4_SCAN.to_vec();
        let directory = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(bytes[directory..directory + 2].try_into().unwrap()) as usize;
        for index in 0..count {
            let entry = directory + 2 + index * 12;
            if u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap()) == tag {
                bytes[entry + 8..entry + 10].copy_from_slice(&value.to_le_bytes());
                return bytes;
            }
        }
        panic!("fixture tag {tag} missing");
    }

    #[test]
    fn bitonal_fax_preserves_every_pixel_and_row_padding() {
        let root = tempfile::tempdir().unwrap();
        let expected = vec![
            255,0,0,255,255,0,255,0,255,0,0,255,255,
            0,255,0,255,0,255,0,255,0,255,0,255,0,
            0,255,255,255,255,255,0,255,255,255,255,255,0,
        ];
        for (name, bytes, pixels, dimensions) in [
            ("normal", FAX4_SCAN.to_vec(), expected.clone(), (13, 3)),
            ("white-zero", fax4_with_tag(262, 0), expected.iter().map(|v| 255 - v).collect(), (13, 3)),
            ("rotated", fax4_with_tag(274, 6), image::DynamicImage::ImageLuma8(image::GrayImage::from_raw(13, 3, expected.clone()).unwrap()).rotate90().into_luma8().into_raw(), (3, 13)),
        ] {
            let source = root.path().join(format!("{name}.tiff"));
            std::fs::write(&source, bytes).unwrap();
            let pages = rasterize_pages(&source, "tiff").unwrap();
            assert_eq!(pages.len(), 1);
            let decoded = image::open(&pages.paths()[0]).unwrap().to_luma8();
            assert_eq!(decoded.dimensions(), dimensions, "{name} dimensions");
            assert_eq!(decoded.into_raw(), pixels, "{name} pixels");
        }
    }

    #[test]
    fn packed_grayscale_retains_row_boundaries_and_endpoints() {
        assert_eq!(unpack_gray(3, 2, 2, &[0b00011000, 0b11100100]).unwrap().into_raw(), vec![0,85,170,255,170,85]);
        assert_eq!(unpack_gray(3, 2, 4, &[0x0f,0x80,0xf0,0x70]).unwrap().into_raw(), vec![0,255,136,255,0,119]);
        assert!(unpack_gray(3, 2, 4, &[0x0f,0x80]).is_none());
    }

    #[test]
    fn pdf_raster_limits_are_checked_before_native_rendering() {
        assert_eq!(pdf_render_width(612.0, 792.0, 200.0).unwrap(), 1700);
        assert!(pdf_render_width(0.0, 792.0, 200.0).is_err());
        assert!(pdf_render_width(f32::NAN, 792.0, 200.0).is_err());
        assert!(pdf_render_width(612.0, f32::INFINITY, 200.0).is_err());
        assert!(pdf_render_width(1.0, 1_000_000.0, 200.0).is_err());
    }

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
