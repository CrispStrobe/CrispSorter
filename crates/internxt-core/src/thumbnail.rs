//! Image thumbnail generation + upload (feature `thumbnails`). Rust port of og's
//! `ThumbnailService` / `ThumbnailUtils` (which used `sharp`): decode a source
//! image, resize it to fit inside a 300x300 box, re-encode as PNG, upload the PNG
//! to the network and register it via `POST /files/thumbnail`.
//!
//! Thumbnail generation is best-effort — a decode/upload failure must never fail
//! the parent file upload. The orchestration helper returns `Ok(None)` when the
//! file isn't thumbnailable and surfaces real errors as `Err` for the caller to
//! log-and-ignore (mirroring og's `tryUploadThumbnail`, which reports and swallows).

use anyhow::Result;
use std::path::Path;

use crate::api::DriveApi;
use crate::models::Thumbnail;
use crate::network::NetworkApi;
use crate::transfer::upload_stream_to_network;

/// og `ThumbnailConfig` — a 300x300 PNG.
pub const MAX_WIDTH: u32 = 300;
pub const MAX_HEIGHT: u32 = 300;
pub const THUMBNAIL_TYPE: &str = "png";

/// og `ThumbnailUtils.MAX_IMAGE_THUMBNAILABLE_SIZE_IN_MB` (500MB, despite the name).
const MAX_IMAGE_THUMBNAILABLE_SIZE: u64 = 500 * 1024 * 1024;

/// The source extensions og will build an image thumbnail from (its
/// `thumbnailableImageExtension` set).
const THUMBNAILABLE_IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", // jpg
    "png",  // png
    "webp", // webp
    "gif",  // gif
    "tif", "tiff", // tiff
];

/// og `ThumbnailUtils.isImageThumbnailable`: a known image extension under the
/// size cap. `file_type` is the bare extension (no dot).
pub fn is_image_thumbnailable(file_type: &str, size: u64) -> bool {
    if size == 0 || size > MAX_IMAGE_THUMBNAILABLE_SIZE {
        return false;
    }
    let ext = file_type.trim().to_lowercase();
    !ext.is_empty() && THUMBNAILABLE_IMAGE_EXTENSIONS.contains(&ext.as_str())
}

/// Read an image's `(width, height)` from its bytes without fully decoding it.
/// Used when registering a custom (`--raw`) thumbnail to record its real
/// dimensions. `None` when the bytes aren't a recognizable image.
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Decode `bytes` as an image, resize to fit inside `MAX_WIDTH`x`MAX_HEIGHT`
/// (aspect preserved, like sharp's `fit: 'inside'`) and encode as PNG.
/// CPU-bound; run under `spawn_blocking` from an async context.
pub fn generate_thumbnail_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)?;
    // `resize` preserves aspect ratio, scaling so the image fits within the box.
    let thumb = img.resize(MAX_WIDTH, MAX_HEIGHT, image::imageops::FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// If `path` is a thumbnailable image, generate its PNG preview, upload it to the
/// network under `bucket` and register it against `file_uuid`. Returns the created
/// `Thumbnail` on success, `Ok(None)` when the file isn't thumbnailable or produced
/// no image. Errors (decode/upload/API) are returned for the caller to log and
/// ignore — they must not fail the parent file upload.
#[cfg(feature = "fs")]
#[allow(clippy::too_many_arguments)]
pub async fn try_upload_thumbnail_from_path(
    net: &NetworkApi,
    api: &DriveApi,
    token: &str,
    bucket: &str,
    mnemonic: &str,
    file_uuid: &str,
    file_type: &str,
    path: &Path,
    size: u64,
) -> Result<Option<Thumbnail>> {
    if !is_image_thumbnailable(file_type, size) {
        return Ok(None);
    }
    let bytes = tokio::fs::read(path).await?;
    upload_thumbnail_bytes(net, api, token, bucket, mnemonic, file_uuid, &bytes).await
}

/// As [`try_upload_thumbnail_from_path`] but the source image bytes are already in
/// memory (e.g. a serve backend that spooled the upload). The caller decides
/// thumbnailability; this always attempts generation.
#[allow(clippy::too_many_arguments)]
pub async fn upload_thumbnail_bytes(
    net: &NetworkApi,
    api: &DriveApi,
    token: &str,
    bucket: &str,
    mnemonic: &str,
    file_uuid: &str,
    image_bytes: &[u8],
) -> Result<Option<Thumbnail>> {
    let owned = image_bytes.to_vec();
    let thumb = tokio::task::spawn_blocking(move || generate_thumbnail_png(&owned)).await??;
    if thumb.is_empty() {
        return Ok(None);
    }
    let size = thumb.len() as u64;

    let bucket_file = upload_stream_to_network(
        net,
        bucket,
        mnemonic,
        std::io::Cursor::new(thumb),
        size,
        None,
    )
    .await?;

    let created = api
        .create_thumbnail_entry(
            token,
            file_uuid,
            THUMBNAIL_TYPE,
            size,
            MAX_WIDTH,
            MAX_HEIGHT,
            bucket,
            &bucket_file,
        )
        .await?;
    Ok(Some(created))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnailable_extensions_and_size_cap() {
        assert!(is_image_thumbnailable("JPG", 1024));
        assert!(is_image_thumbnailable("png", 1024));
        assert!(is_image_thumbnailable("tiff", 1024));
        assert!(!is_image_thumbnailable("pdf", 1024)); // og only images here
        assert!(!is_image_thumbnailable("png", 0)); // zero-size
        assert!(!is_image_thumbnailable("png", MAX_IMAGE_THUMBNAILABLE_SIZE + 1));
    }

    #[test]
    fn generates_fit_inside_png() {
        // 600x400 source -> fit inside 300x300 preserving aspect -> 300x200 PNG.
        let src = image::RgbImage::from_fn(600, 400, |x, _| {
            image::Rgb([(x % 256) as u8, 0, 0])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(src)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let out = generate_thumbnail_png(&buf.into_inner()).unwrap();
        let thumb = image::load_from_memory(&out).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (300, 200));
    }
}

