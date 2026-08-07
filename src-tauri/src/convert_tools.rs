//! Document-conversion Tauri commands.
//!
//! The GUI half of `crispsorter convert`. Both call
//! [`extractors::convert::convert`], so the rules about which converter reads
//! what, and which outputs each can produce, are decided in exactly one
//! place — a GUI that reimplemented them would drift the first time either
//! side gained a format.

use crate::extractors::convert::{self, ConvertOptions, Emit, Engine};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What this build can do with a given file, answered before converting.
///
/// The panel needs this to disable outputs rather than offer them and fail:
/// Word/RTF exist only on the native path, and every non-`.pptx` format
/// needs a Cargo feature that may not be compiled in.
#[derive(Debug, Clone, Serialize)]
pub struct ConvertCapabilities {
    /// Lowercased extension of the file that was probed, `""` if it has none.
    pub ext: String,
    /// A dedicated reader exists for this format (today: `.pptx`).
    pub native_reader: bool,
    /// The generic converter is compiled into this build.
    pub anydoc_available: bool,
    /// This build can convert the file at all.
    pub convertible: bool,
    /// Word / Rich Text are reachable for this file.
    pub rich_output: bool,
    /// The slide-specific knobs (notes, comments, ordering) apply.
    pub slide_options: bool,
    /// Every extension this build can convert — for file pickers.
    pub extensions: Vec<String>,
}

#[tauri::command]
pub fn convert_capabilities(path: Option<String>) -> ConvertCapabilities {
    let ext = path
        .as_deref()
        .map(Path::new)
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let native = convert::has_native_reader(&ext);
    let extensions = convert::convertible_extensions();
    let convertible = !ext.is_empty() && extensions.iter().any(|e| *e == ext);

    ConvertCapabilities {
        ext,
        native_reader: native,
        anydoc_available: crate::extractors::anydoc_conv::is_available(),
        convertible,
        // Both only exist on the native path, so they track it exactly.
        rich_output: native,
        slide_options: native,
        extensions,
    }
}

/// The result handed back to the panel.
#[derive(Debug, Clone, Serialize)]
pub struct ConvertOutput {
    /// Printable output, or `null` for Word (which only lands in a file).
    pub content: Option<String>,
    pub headings: Vec<String>,
    pub slides: Option<usize>,
    /// `"native"` or `"anydoc"` — shown so the panel can say which ran,
    /// rather than leaving the user to infer it from the output shape.
    pub engine_used: String,
    pub written_path: Option<String>,
}

/// Convert one document.
///
/// `out_path` is optional: without it the conversion is returned for preview
/// and nothing touches the disk. Word output requires it, and the shared
/// layer rejects the combination rather than inventing a path.
#[tauri::command]
pub async fn convert_document(
    path: String,
    emit: String,
    engine: String,
    wrap_text: Option<usize>,
    include_notes: Option<bool>,
    include_comments: Option<bool>,
    visual_order: Option<bool>,
    out_path: Option<String>,
) -> Result<ConvertOutput, String> {
    let emit = Emit::from_name(&emit).ok_or_else(|| format!("unknown output format: {emit}"))?;
    let engine =
        Engine::from_name(&engine).ok_or_else(|| format!("unknown converter: {engine}"))?;

    let opts = ConvertOptions {
        emit,
        engine,
        wrap_width: wrap_text.unwrap_or(0),
        include_notes: include_notes.unwrap_or(true),
        include_comments: include_comments.unwrap_or(true),
        visual_order: visual_order.unwrap_or(true),
    };

    let src = PathBuf::from(path);
    let out = out_path.map(PathBuf::from);

    // Conversion is CPU-bound file work — a multi-hundred-slide deck would
    // otherwise stall the webview's command loop.
    tokio::task::spawn_blocking(move || {
        convert::convert(&src, &opts, out.as_deref()).map(|c| ConvertOutput {
            content: c.body,
            headings: c.headings,
            slides: c.slides,
            engine_used: c.engine_used.to_string(),
            written_path: c.written_path,
        })
    })
    .await
    .map_err(|e| format!("conversion task failed: {e}"))?
    .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_track_the_build_for_a_deck() {
        let caps = convert_capabilities(Some("/x/deck.pptx".into()));
        assert_eq!(caps.ext, "pptx");
        assert!(caps.native_reader);
        assert!(caps.convertible, "pptx needs no feature");
        assert!(caps.rich_output, "Word/RTF ride on the native reader");
        assert!(caps.slide_options);
        assert!(caps.extensions.contains(&"pptx".to_string()));
    }

    #[test]
    fn capabilities_for_an_anydoc_format_follow_the_feature() {
        let caps = convert_capabilities(Some("/x/book.epub".into()));
        assert_eq!(caps.ext, "epub");
        assert!(!caps.native_reader);
        assert!(!caps.rich_output, "no Word output off the native path");
        assert!(!caps.slide_options, "an e-book has no slides");
        assert_eq!(caps.convertible, caps.anydoc_available);
    }

    #[test]
    fn a_format_nothing_reads_is_not_advertised_as_convertible() {
        let caps = convert_capabilities(Some("/x/photo.png".into()));
        assert!(!caps.convertible);
        assert!(!caps.native_reader);
        // And a path with no extension at all must not claim convertibility.
        let none = convert_capabilities(Some("/x/README".into()));
        assert!(none.ext.is_empty());
        assert!(!none.convertible);
    }

    #[tokio::test]
    async fn converting_a_deck_returns_slides_and_the_engine_used() {
        let tmp = tempfile::TempDir::new().unwrap();
        let deck = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let got = convert_document(
            deck.to_string_lossy().into_owned(),
            "md".into(),
            "auto".into(),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("deck should convert");
        assert_eq!(got.engine_used, "native");
        assert_eq!(got.slides, Some(2));
        assert!(got.written_path.is_none(), "no destination means no write");
        assert!(got.content.unwrap().contains("## Slide 1: Erste Folie"));
    }

    #[tokio::test]
    async fn an_unknown_output_name_is_refused_before_any_work() {
        let err = convert_document(
            "/nonexistent/deck.pptx".into(),
            "pdf".into(),
            "auto".into(),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unknown output format"), "got: {err}");
    }
}
