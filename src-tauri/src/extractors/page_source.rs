//! Multi-page page sourcing for OCR.
//!
//! Decodes a document file into **one image per page** so the OCR dispatch can
//! run the pipeline per page and concatenate. Multi-page **TIFF** scans are
//! split via the pure-Rust `tiff` crate (each frame → a temp PNG). Single-page
//! image formats return the original path unchanged (zero-copy). The optional
//! `pdf-render` feature rasterizes PDFs with PDFium and retains each page's
//! physical dimensions. Invalid or unsupported frames fail the entire import.
//!
//! `image` decodes only the first TIFF frame, so multi-frame handling uses the
//! `tiff` decoder directly and re-encodes each frame to PNG via `image`.

use anyhow::{Context, Result};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

/// Per-page rendered images. Holds the `TempDir` so split pages outlive use.
#[derive(Debug)]
pub struct PageImages {
    paths: Vec<PathBuf>,
    physical_sizes: Vec<Option<(f64, f64)>>,
    orientations: Vec<u32>,
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
    /// Physical page dimensions in PDF points (1/72 inch), when the source
    /// carries them. Raster-only sources leave resolution policy to the caller.
    pub fn page_size_points(&self, index: usize) -> Option<(f64, f64)> {
        self.physical_sizes.get(index).copied().flatten()
    }
    /// Orientation still to apply to a source image. TIFF/PDF frames have
    /// already been normalized, so return 1 for those rendered assets.
    pub fn page_orientation(&self, index: usize) -> u32 {
        self.orientations.get(index).copied().unwrap_or(1)
    }
    /// The single original path (no decode) — the safe fallback.
    pub fn single(path: &Path) -> Self {
        let metadata = raster_metadata(path);
        let size = image::image_dimensions(path).ok().and_then(|(w, h)| {
            metadata.dpi.and_then(|(x, y)| physical_size_from_dpi(w, h, x, y))
        });
        PageImages { paths: vec![path.to_path_buf()], physical_sizes: vec![size],
            orientations: vec![metadata.orientation], _tmp: None }
    }
}

/// Bake a remaining EXIF orientation into a temporary PNG once, so every OCR
/// engine sees the same upright pixels. TIFF/PDF frames already carry `1`.
pub fn normalized_page(path: &Path, orientation: u32) -> Result<Option<tempfile::NamedTempFile>> {
    anyhow::ensure!((1..=8).contains(&orientation), "Invalid OCR page orientation {orientation}");
    if orientation == 1 { return Ok(None); }
    super::image_input::validate_file(path).context("Validate oriented OCR page")?;
    let (width, height) = image::image_dimensions(path).context("Read oriented OCR page dimensions")?;
    anyhow::ensure!(width > 0 && height > 0 && u64::from(width) * u64::from(height) <= 100_000_000,
        "OCR page exceeds the supported 100 megapixel limit");
    let image = image::open(path).context("Decode oriented OCR page")?;
    let image = match orientation {
        2 => image.fliph(), 3 => image.rotate180(), 4 => image.flipv(),
        5 => image.rotate90().fliph(), 6 => image.rotate90(),
        7 => image.rotate90().flipv(), 8 => image.rotate270(), _ => image,
    };
    let file = tempfile::Builder::new().prefix("ocr_oriented_").suffix(".png").tempfile()?;
    image.save(file.path()).context("Save oriented OCR page")?;
    Ok(Some(file))
}

const MAX_RASTER_METADATA_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
struct RasterMetadata {
    dpi: Option<(f64, f64)>,
    orientation: u32,
}

fn checked_dpi(x: f64, y: f64) -> Option<(f64, f64)> {
    // Reject placeholders, zero denominators and implausible damaged tags.
    // This range includes ordinary screen, document, photo and film scans.
    [x, y].iter().all(|v| v.is_finite() && (10.0..=9600.0).contains(v)).then_some((x, y))
}

fn physical_size_from_dpi(width: u32, height: u32, x: f64, y: f64) -> Option<(f64, f64)> {
    let (x, y) = checked_dpi(x, y)?;
    let size = (f64::from(width) * 72.0 / x, f64::from(height) * 72.0 / y);
    [size.0, size.1].iter().all(|v| v.is_finite() && (0.01..=14400.0).contains(v)).then_some(size)
}

fn density_in_dpi(x: f64, y: f64, unit: u32) -> Option<(f64, f64)> {
    let factor = match unit { 2 => 1.0, 3 => 2.54, _ => return None };
    checked_dpi(x * factor, y * factor)
}

fn exif_metadata(bytes: &[u8]) -> RasterMetadata {
    let mut owner = exif::Reader::new();
    let reader = owner.continue_on_error(true);
    let exif = match reader.read_from_container(&mut std::io::Cursor::new(bytes)) {
        Ok(exif) => exif,
        Err(exif::Error::PartialResult(partial)) => partial.into_inner().0,
        Err(_) => return RasterMetadata { dpi: None, orientation: 1 },
    };
    let uint = |tag| exif.get_field(tag, exif::In::PRIMARY).and_then(|field| field.value.get_uint(0));
    let orientation = uint(exif::Tag::Orientation).filter(|v| (1..=8).contains(v)).unwrap_or(1);
    let rational = |tag| {
        match &exif.get_field(tag, exif::In::PRIMARY)?.value {
            exif::Value::Rational(values) if values.len() == 1 && values[0].denom != 0 =>
                Some(f64::from(values[0].num) / f64::from(values[0].denom)),
            _ => None,
        }
    };
    // TIFF/EXIF define an omitted resolution unit as inches. A present but
    // unreadable or unitless value must not be treated as an inch measurement.
    let unit = match exif.get_field(exif::Tag::ResolutionUnit, exif::In::PRIMARY) {
        Some(field) => field.value.get_uint(0),
        None => Some(2),
    };
    let dpi = rational(exif::Tag::XResolution).zip(rational(exif::Tag::YResolution))
        .zip(unit).and_then(|((x, y), unit)| density_in_dpi(x, y, unit));
    RasterMetadata { dpi, orientation }
}

/// Read at most one MiB, without following data beyond the prefix. Normal
/// JPEG APP0/APP1 and PNG pre-IDAT metadata fit here; unusually late or corrupt
/// metadata deliberately leaves the caller's explicit fallback in effect.
fn raster_metadata(path: &Path) -> RasterMetadata {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_RASTER_METADATA_BYTES).read_to_end(&mut bytes));
    if read.is_err() { return RasterMetadata { dpi: None, orientation: 1 }; }
    let mut metadata = exif_metadata(&bytes);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        metadata.dpi = png_density(&bytes).or(metadata.dpi);
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        metadata.dpi = metadata.dpi.or_else(|| jpeg_density(&bytes));
    }
    metadata
}

fn png_crc(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1)); }
    }
    !crc
}

fn png_density(bytes: &[u8]) -> Option<(f64, f64)> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") { return None; }
    let mut offset = 8usize;
    while offset.checked_add(12)? <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        if kind == b"IDAT" || kind == b"IEND" { break; }
        let end = offset.checked_add(12)?.checked_add(length)?;
        if end > bytes.len() { break; }
        if kind == b"pHYs" {
            if length != 9 || bytes[offset + 16] != 1 { return None; }
            let expected = u32::from_be_bytes(bytes[end - 4..end].try_into().ok()?);
            if png_crc(&bytes[offset + 4..end - 4]) != expected { return None; }
            let x = u32::from_be_bytes(bytes[offset + 8..offset + 12].try_into().ok()?);
            let y = u32::from_be_bytes(bytes[offset + 12..offset + 16].try_into().ok()?);
            // PNG unit 1 is pixels per metre, not per centimetre.
            return checked_dpi(f64::from(x) * 0.0254, f64::from(y) * 0.0254);
        }
        offset = end;
    }
    None
}

fn jpeg_density(bytes: &[u8]) -> Option<(f64, f64)> {
    if !bytes.starts_with(&[0xff, 0xd8]) { return None; }
    let mut offset = 2usize;
    while offset < bytes.len() {
        if bytes[offset] != 0xff { break; }
        while bytes.get(offset) == Some(&0xff) { offset += 1; }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if marker == 0xda || marker == 0xd9 { break; } // Do not scan entropy-coded pixels.
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) { continue; }
        let size = u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if size < 2 { return None; }
        let end = offset.checked_add(size)?;
        let segment = bytes.get(offset + 2..end)?;
        if marker == 0xe0 && segment.len() >= 14 && segment.starts_with(b"JFIF\0") {
            let unit = match segment[7] { 1 => 2, 2 => 3, _ => return None };
            let x = u16::from_be_bytes(segment[8..10].try_into().ok()?);
            let y = u16::from_be_bytes(segment[10..12].try_into().ok()?);
            return density_in_dpi(f64::from(x), f64::from(y), unit);
        }
        offset = end;
    }
    None
}

fn tiff_density<R: Read + Seek>(decoder: &mut tiff::decoder::Decoder<R>) -> Option<(f64, f64)> {
    use tiff::{decoder::ifd::Value, tags::Tag};
    let unit = match decoder.find_tag_unsigned::<u32>(Tag::ResolutionUnit) {
        Ok(value) => value.unwrap_or(2),
        Err(_) => return None,
    };
    let rational = |value| match value {
        Value::Rational(numerator, denominator) if denominator != 0 =>
            Some(f64::from(numerator) / f64::from(denominator)),
        _ => None,
    };
    // The existing TIFF decoder caps any tag value at one MiB. Only the two
    // scalar rationals are accepted; vectors or malformed tags are ignored.
    let x = rational(decoder.find_tag(Tag::XResolution).ok()??)?;
    let y = rational(decoder.find_tag(Tag::YResolution).ok()??)?;
    density_in_dpi(x, y, unit)
}

/// Decode `path` into per-page image files. Multi-page TIFF is split into temp
/// PNGs; every other format returns the original path as a single page.
pub fn rasterize_pages(path: &Path, ext: &str) -> Result<PageImages> {
    super::image_input::validate_file(path).context("Validate source image")?;
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
    let mut physical_sizes = Vec::new();
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
        let size = tiff_density(&mut dec).and_then(|(x, y)| physical_size_from_dpi(w, h, x, y))
            .map(|(width, height)| if orientation >= 5 { (height, width) } else { (width, height) });
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
        physical_sizes.push(size);
        idx += 1;

        if dec.more_images() {
            dec.next_image().context("TIFF next_image")?;
        } else {
            break;
        }
    }

    if paths.is_empty() { anyhow::bail!("TIFF contains no readable pages"); }
    Ok(PageImages { orientations: vec![1; paths.len()], physical_sizes, paths, _tmp: Some(tmp) })
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
    //   - `bin` / `resources/bin` under the exe (portable layouts)
    //   - `../Resources/bin` + `../Frameworks` (macOS .app)
    //   - `../lib/CrispSorter/bin` (the installed Linux .deb)
    //   - `../lib` (FHS-style installs)
    let bindings = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
        .and_then(|dir| {
            let candidates = [
                dir.clone(),
                dir.join("bin"),
                dir.join("resources").join("bin"),
                dir.join("..").join("Resources").join("bin"),
                dir.join("..").join("lib").join("CrispSorter").join("bin"),
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
    let mut physical_sizes = Vec::new();
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
        physical_sizes.push(Some((f64::from(page.width().value), f64::from(page.height().value))));
    }
    if paths.is_empty() {
        anyhow::bail!("PDF has no renderable pages: {}", path.display());
    }
    Ok(PageImages { orientations: vec![1; paths.len()], paths, physical_sizes, _tmp: Some(tmp) })
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

    #[test]
    fn exif_rotation_changes_ocr_pixels_once_and_retains_the_temp_image() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("oriented.png");
        let mut image = image::RgbImage::from_pixel(3, 2, image::Rgb([255u8,255,255]));
        image.put_pixel(0,0,image::Rgb([255,0,0]));
        image.save(&path).unwrap();
        let prepared = normalized_page(&path, 6).unwrap().unwrap();
        let rotated = image::open(prepared.path()).unwrap().to_rgb8();
        assert_eq!(rotated.dimensions(), (2,3));
        assert_eq!(rotated.get_pixel(1,0), &image::Rgb([255,0,0]));
        assert_eq!(PageImages::single(prepared.path()).page_orientation(0), 1);
        assert!(normalized_page(prepared.path(), 1).unwrap().is_none());
        assert_eq!(image::image_dimensions(&path).unwrap(), (3,2), "source stays unchanged");
    }

    fn assert_points(actual: Option<(f64, f64)>, expected: (f64, f64)) {
        let actual = actual.expect("source resolution must be retained");
        assert!((actual.0 - expected.0).abs() < 0.001, "width: {actual:?} vs {expected:?}");
        assert!((actual.1 - expected.1).abs() < 0.001, "height: {actual:?} vs {expected:?}");
    }

    #[test]
    fn tiff_resolution_is_per_frame_and_follows_orientation() {
        use tiff::encoder::{colortype, Rational, TiffEncoder};
        use tiff::tags::{ResolutionUnit, Tag};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("resolution.tiff");
        {
            let mut file = std::fs::File::create(&source).unwrap();
            let mut encoder = TiffEncoder::new(&mut file).unwrap();
            for (width, height, unit, numerator, denominator, orientation) in [
                (600, 300, ResolutionUnit::Inch, 600, 1, 1),
                (254, 127, ResolutionUnit::Centimeter, 100, 1, 6),
                (600, 300, ResolutionUnit::None, 600, 1, 1),
                (600, 300, ResolutionUnit::Inch, 600, 0, 1),
            ] {
                let mut frame = encoder.new_image::<colortype::Gray8>(width, height).unwrap();
                frame.resolution(unit, Rational { n: numerator, d: denominator });
                frame.encoder().write_tag(Tag::Orientation, orientation as u16).unwrap();
                frame.write_data(&vec![255; (width * height) as usize]).unwrap();
            }
        }
        let pages = rasterize_pages(&source, "tiff").unwrap();
        assert_eq!(pages.len(), 4);
        assert_points(pages.page_size_points(0), (72.0, 36.0));
        assert_points(pages.page_size_points(1), (36.0, 72.0));
        assert_eq!(image::image_dimensions(&pages.paths()[1]).unwrap(), (127, 254));
        assert_eq!(pages.page_orientation(1), 1, "TIFF orientation is already baked");
        assert_eq!(pages.page_size_points(2), None, "unitless density is only an aspect ratio");
        assert_eq!(pages.page_size_points(3), None, "zero rational denominators are not physical measurements");

        // Rename the first IFD's ResolutionUnit tag to an unknown private tag.
        // Its valid X/YResolution then use TIFF's documented default inches.
        let mut bytes = std::fs::read(&source).unwrap();
        let little = &bytes[..2] == b"II";
        let u16_at = |slice: &[u8]| if little { u16::from_le_bytes(slice.try_into().unwrap()) } else { u16::from_be_bytes(slice.try_into().unwrap()) };
        let directory = if little { u32::from_le_bytes(bytes[4..8].try_into().unwrap()) } else { u32::from_be_bytes(bytes[4..8].try_into().unwrap()) } as usize;
        let count = u16_at(&bytes[directory..directory + 2]) as usize;
        let entry = (0..count).map(|i| directory + 2 + i * 12)
            .find(|offset| u16_at(&bytes[*offset..*offset + 2]) == 296).unwrap();
        bytes[entry..entry + 2].copy_from_slice(&if little { 65000u16.to_le_bytes() } else { 65000u16.to_be_bytes() });
        let no_unit = root.path().join("omitted-unit.tiff");
        std::fs::write(&no_unit, bytes).unwrap();
        let pages = rasterize_pages(&no_unit, "tiff").unwrap();
        assert_points(pages.page_size_points(0), (72.0, 36.0));
    }

    #[test]
    fn png_physical_density_uses_metres_and_rejects_unitless_or_corrupt_values() {
        assert_eq!(png_crc(b"123456789"), 0xcbf4_3926, "standard CRC32 reference vector");
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join("plain.png");
        image::GrayImage::from_pixel(600, 300, image::Luma([255])).save(&plain).unwrap();
        let original = std::fs::read(&plain).unwrap();
        for (name, unit, density, corrupt) in [
            ("600-dpi", 1u8, 23_622u32, false),
            ("unitless", 0, 23_622, false),
            ("implausible", 1, u32::MAX, false),
            ("bad-crc", 1, 23_622, true),
        ] {
            let mut chunk = 9u32.to_be_bytes().to_vec();
            chunk.extend_from_slice(b"pHYs");
            chunk.extend_from_slice(&density.to_be_bytes());
            chunk.extend_from_slice(&density.to_be_bytes());
            chunk.push(unit);
            let crc = png_crc(&chunk[4..]) ^ u32::from(corrupt);
            chunk.extend_from_slice(&crc.to_be_bytes());
            let mut bytes = original[..33].to_vec(); // PNG signature plus IHDR.
            bytes.extend_from_slice(&chunk);
            bytes.extend_from_slice(&original[33..]);
            let source = root.path().join(format!("{name}.png"));
            std::fs::write(&source, bytes).unwrap();
            let pages = rasterize_pages(&source, "png").unwrap();
            if name == "600-dpi" {
                assert_points(pages.page_size_points(0), (72.0, 36.0));
                assert_eq!(image::open(&source).unwrap().width(), 600);
            } else { assert_eq!(pages.page_size_points(0), None, "{name}"); }
        }
    }

    #[test]
    fn jpeg_jfif_density_distinguishes_inches_centimetres_and_aspect_ratio() {
        let jpeg = |unit: u8, x: u16, y: u16| {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
            bytes.extend_from_slice(b"JFIF\0\x01\x02");
            bytes.push(unit);
            bytes.extend_from_slice(&x.to_be_bytes());
            bytes.extend_from_slice(&y.to_be_bytes());
            bytes.extend_from_slice(&[0, 0, 0xff, 0xd9]);
            bytes
        };
        assert_eq!(jpeg_density(&jpeg(1, 600, 600)), Some((600.0, 600.0)));
        assert_eq!(jpeg_density(&jpeg(2, 100, 50)), Some((254.0, 127.0)));
        assert_eq!(jpeg_density(&jpeg(0, 600, 600)), None);
        assert_eq!(jpeg_density(&jpeg(1, 0, 600)), None);
        assert_eq!(jpeg_density(&jpeg(1, 600, 600)[..12]), None);
    }

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
