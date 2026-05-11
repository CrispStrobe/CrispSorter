//! P13 slice A2 — on-demand thumbnail generation.
//!
//! Spec calls for "no cache" — every call decodes the source image
//! fresh.  Browser-side image cache and the IntersectionObserver
//! lazy-load in the grid handle re-render economics so we don't need
//! a server-side LRU here.  If the index ever grows large enough that
//! per-tile decode latency dominates scroll, a follow-up slice can
//! introduce a small SQLite-backed cache without changing the wire
//! shape.
//!
//! Output is always **PNG**.  PNG is decode-everywhere in browsers,
//! lossless (so a 256-px thumbnail of a 24-MP shot still looks crisp),
//! and the encoded payload at 256-px square is small enough that the
//! Tauri serde bridge handles it without complaint (~25 kB typical).
//! WebP would be smaller but `image` 0.25's WebP encoder is still
//! marked "limited"; PNG is the safer default for now.
//!
//! ## Format coverage
//!
//! The `image` crate's default features cover JPEG, PNG, WebP, TIFF,
//! BMP, GIF, ICO, QOI.  HEIC / HEIF are **not** in `image` — they need
//! `libheif` (system dep) or `libheif-rs` / `heif-rs` (FFI binding).
//! For A2 we return a typed `ThumbnailError::UnsupportedFormat` for
//! HEIC so the UI can fall back to the placeholder tile gracefully.
//! HEIC decode is tracked as a follow-up; punting it keeps slice A2
//! within its 6-hour budget without leaving the user with a panic.

use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};

/// Default longest-edge size for grid-tile thumbnails.  Matches the
/// `bilderThumbnailSize` setting in the spec (`docs/P13_Bilder_integration.md`),
/// which defaults to 256.  Callers can override via the `size` arg
/// to [`generate_thumbnail`].
pub const DEFAULT_THUMBNAIL_SIZE: u32 = 256;

/// Hard ceiling on caller-supplied `size`.  Keeps a malicious /
/// fat-fingered request from asking us to encode a 16k × 16k PNG.
pub const MAX_THUMBNAIL_SIZE: u32 = 4096;

/// Typed thumbnail-generation errors.  `Display` matches the format
/// the Tauri command boundary serialises into the JS-side error
/// string; the UI inspects the prefix (`unsupported format:`) to
/// decide whether to keep the placeholder tile vs. surface a banner.
#[derive(Debug)]
pub enum ThumbnailError {
    NotFound(String),
    UnsupportedFormat(String),
    Decode(String),
    Encode(String),
    InvalidSize { size: u32 },
}

impl std::fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThumbnailError::NotFound(p)          => write!(f, "file not found: {p}"),
            ThumbnailError::UnsupportedFormat(e) => write!(f, "unsupported format: {e}"),
            ThumbnailError::Decode(e)            => write!(f, "image decode failed: {e}"),
            ThumbnailError::Encode(e)            => write!(f, "image encode failed: {e}"),
            ThumbnailError::InvalidSize { size } => write!(
                f,
                "invalid size: {size} (must be 1..={max})",
                max = MAX_THUMBNAIL_SIZE
            ),
        }
    }
}

impl std::error::Error for ThumbnailError {}

/// Generate a PNG thumbnail of `path`, scaling so the longest edge is
/// `max_dim` pixels (aspect ratio preserved).  Reads the file
/// synchronously — caller is responsible for offloading to a blocking
/// pool if invoked from an async context.
///
/// Returns the encoded PNG bytes ready for `data:image/png;base64,…`
/// serialisation by the frontend.
pub fn generate_thumbnail(path: &Path, max_dim: u32) -> Result<Vec<u8>, ThumbnailError> {
    if !(1..=MAX_THUMBNAIL_SIZE).contains(&max_dim) {
        return Err(ThumbnailError::InvalidSize { size: max_dim });
    }
    if !path.exists() {
        return Err(ThumbnailError::NotFound(path.display().to_string()));
    }

    // HEIC/HEIF detection by extension — the `image` crate's
    // `guess_format` returns Err for these, but giving the user a
    // typed "unsupported" instead of a generic decode failure keeps
    // the UI fallback path clean.  Lower-case so HEIC vs heic both hit.
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let lower = ext.to_lowercase();
        if matches!(lower.as_str(), "heic" | "heif" | "avif") {
            return Err(ThumbnailError::UnsupportedFormat(lower));
        }
    }

    let img: DynamicImage = image::open(path)
        .map_err(|e| ThumbnailError::Decode(format!("{}: {e}", path.display())))?;

    // `thumbnail` uses Lanczos3 internally for downscaling — quality
    // is fine at 256-px square.  For up-scaling (small source +
    // huge requested size) it'd be a waste; clamp output to the
    // smaller of source-longest-edge or requested.
    let (w, h) = (img.width(), img.height());
    let src_max = w.max(h);
    let target = max_dim.min(src_max);

    let resized = img.resize(target, target, FilterType::Lanczos3);

    let mut buf: Vec<u8> = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
        .map_err(|e| ThumbnailError::Encode(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use tempfile::NamedTempFile;

    /// Synth a tiny square test image with a recognisable colour
    /// gradient.  Returns (path-handle, original-dimension).  The
    /// handle keeps the file alive for the test's scope.
    fn synth_png(side: u32) -> (NamedTempFile, u32) {
        let img = ImageBuffer::from_fn(side, side, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
        });
        let tmp = NamedTempFile::with_suffix(".png").expect("named tempfile");
        img.save(tmp.path()).expect("save synth png");
        (tmp, side)
    }

    #[test]
    fn generates_png_at_requested_size() {
        let (tmp, _) = synth_png(800);
        let bytes = generate_thumbnail(tmp.path(), 256).unwrap();
        // First 8 bytes of every PNG are the magic signature 89 50 4E 47 0D 0A 1A 0A.
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG payload");
        // Round-trip: decode and assert dimension scaled correctly.
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width().max(decoded.height()), 256);
    }

    #[test]
    fn does_not_upscale_smaller_than_source() {
        let (tmp, _) = synth_png(64);
        let bytes = generate_thumbnail(tmp.path(), 256).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        // Source was 64×64; we should not blow that up to 256.
        assert_eq!(decoded.width(),  64);
        assert_eq!(decoded.height(), 64);
    }

    #[test]
    fn preserves_aspect_ratio_for_non_square_sources() {
        // 800 × 400 → max edge 256 → expect 256 × 128.
        let img = ImageBuffer::from_fn(800u32, 400u32, |x, y| Rgb([x as u8, y as u8, 0u8]));
        let tmp = NamedTempFile::with_suffix(".png").unwrap();
        img.save(tmp.path()).unwrap();

        let bytes = generate_thumbnail(tmp.path(), 256).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(),  256);
        assert_eq!(decoded.height(), 128);
    }

    #[test]
    fn returns_unsupported_for_heic() {
        // We don't need a real HEIC file -- the extension sniff fires
        // before we try to read the bytes.  Use a NamedTempFile with
        // an ".heic" suffix containing arbitrary content.
        let tmp = NamedTempFile::with_suffix(".heic").unwrap();
        std::fs::write(tmp.path(), b"not actually heic").unwrap();
        match generate_thumbnail(tmp.path(), 256) {
            Err(ThumbnailError::UnsupportedFormat(ext)) => assert_eq!(ext, "heic"),
            other => panic!("expected UnsupportedFormat(heic), got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_sizes() {
        let (tmp, _) = synth_png(64);
        assert!(matches!(
            generate_thumbnail(tmp.path(), 0),
            Err(ThumbnailError::InvalidSize { .. })
        ));
        assert!(matches!(
            generate_thumbnail(tmp.path(), MAX_THUMBNAIL_SIZE + 1),
            Err(ThumbnailError::InvalidSize { .. })
        ));
    }

    #[test]
    fn missing_file_returns_typed_error() {
        let path = std::path::Path::new("/tmp/does-not-exist-9f3a4b.png");
        match generate_thumbnail(path, 256) {
            Err(ThumbnailError::NotFound(_)) => (),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn jpeg_source_decodes_and_re_encodes_as_png() {
        let img = ImageBuffer::from_fn(400u32, 300u32, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 200u8]));
        let tmp = NamedTempFile::with_suffix(".jpg").unwrap();
        img.save(tmp.path()).unwrap();

        let bytes = generate_thumbnail(tmp.path(), 200).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 200);
        assert_eq!(decoded.height(), 150);
    }
}
