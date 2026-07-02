//! Clipboard capture for quick indexing (P24.6).
//!
//! Reads the system clipboard and returns its content as either text
//! or an image (PNG bytes).  The caller (Tauri command) can then index
//! the content as a synthetic document.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ClipboardContent {
    /// "text" or "image"
    pub kind: String,
    /// The text content (if kind == "text"). Empty for images.
    pub text: String,
    /// Path to saved PNG file (if kind == "image"). Empty for text.
    pub image_path: String,
    /// Width in pixels (images only).
    pub width: u32,
    /// Height in pixels (images only).
    pub height: u32,
}

/// Read the clipboard content.  Tries image first (screenshots),
/// then falls back to text.  Images are saved to a temp PNG.
pub fn read_clipboard() -> Result<ClipboardContent, String> {
    let mut board = arboard::Clipboard::new()
        .map_err(|e| format!("clipboard init: {e}"))?;

    // Try image first (higher priority for screenshot capture)
    if let Ok(img) = board.get_image() {
        let (path, w, h) = save_image_data(&img)?;
        return Ok(ClipboardContent {
            kind: "image".into(),
            text: String::new(),
            image_path: path.to_string_lossy().into_owned(),
            width: w,
            height: h,
        });
    }

    // Fall back to text
    let text = board.get_text()
        .map_err(|e| format!("clipboard read: {e}"))?;
    if text.is_empty() {
        return Err("Clipboard is empty".into());
    }
    Ok(ClipboardContent {
        kind: "text".into(),
        text,
        image_path: String::new(),
        width: 0,
        height: 0,
    })
}

fn save_image_data(img: &arboard::ImageData) -> Result<(std::path::PathBuf, u32, u32), String> {
    let width = img.width as u32;
    let height = img.height as u32;
    let tmp_dir = std::env::temp_dir().join("crispsorter-clipboard");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create temp dir: {e}"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = tmp_dir.join(format!("capture_{ts}.png"));
    let mut file = std::fs::File::create(&path).map_err(|e| format!("create file: {e}"))?;
    let mut encoder = image::codecs::png::PngEncoder::new(&mut file);
    use image::ImageEncoder;
    encoder.write_image(&img.bytes, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok((path, width, height))
}

/// Save clipboard image to a temp file and return the path.
/// Useful for feeding into the OCR pipeline.
pub fn save_clipboard_image_to_temp() -> Result<(std::path::PathBuf, u32, u32), String> {
    let mut board = arboard::Clipboard::new()
        .map_err(|e| format!("clipboard init: {e}"))?;
    let img = board.get_image()
        .map_err(|e| format!("no image in clipboard: {e}"))?;
    save_image_data(&img)
}

#[cfg(test)]
mod tests {
    #[test]
    fn clipboard_content_serde() {
        let c = super::ClipboardContent {
            kind: "text".into(),
            text: "hello".into(),
            image_path: String::new(),
            width: 0,
            height: 0,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"kind\":\"text\""));
    }
}
