//! Document conversion — the single routing implementation.
//!
//! Deciding *which* converter reads a file, and what it may emit, is a small
//! pile of rules: `.pptx` has a native reader and everything else goes
//! through anydoc; `docx`/`rtf` output hangs off the native reader's slide
//! model; anydoc needs a Cargo feature that may not be compiled in. Those
//! rules live here once, so the CLI (`crispsorter convert`) and the GUI
//! (`convert_document`) cannot drift apart — the earlier version had the
//! rules inline in the CLI, which is exactly how a GUI ends up quietly
//! accepting a combination the CLI rejects.

use super::{anydoc_conv, pptx};
use anyhow::{bail, Result};
use std::path::Path;

/// What the caller wants out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Emit {
    #[default]
    Markdown,
    /// Markdown with the ATX markers stripped.
    Text,
    /// Just the outline.
    Headings,
    /// Word. Needs an output path and the native reader.
    Docx,
    /// Rich Text. Needs the native reader.
    Rtf,
}

impl Emit {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "text" | "txt" => Some(Self::Text),
            "headings" | "outline" => Some(Self::Headings),
            "docx" | "word" => Some(Self::Docx),
            "rtf" => Some(Self::Rtf),
            _ => None,
        }
    }

    /// Whether this output is a file rather than something printable.
    pub fn is_binary(self) -> bool {
        matches!(self, Self::Docx)
    }
}

/// Which converter to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// Native reader where one exists, anydoc otherwise.
    #[default]
    Auto,
    /// Force the dedicated reader; error if the format has none.
    Native,
    /// Force the generic converter.
    Anydoc,
}

impl Engine {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "native" => Some(Self::Native),
            "anydoc" | "generic" => Some(Self::Anydoc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub emit: Emit,
    pub engine: Engine,
    pub wrap_width: usize,
    pub include_notes: bool,
    pub include_comments: bool,
    /// Sort slide shapes by position rather than XML order.
    pub visual_order: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            emit: Emit::default(),
            engine: Engine::default(),
            wrap_width: 0,
            include_notes: true,
            include_comments: true,
            visual_order: true,
        }
    }
}

/// The result of a conversion.
#[derive(Debug, Clone, Default)]
pub struct Converted {
    /// Printable output. `None` for [`Emit::Docx`], which only ever lands
    /// in a file.
    pub body: Option<String>,
    pub headings: Vec<String>,
    /// Slide count — `None` for formats that have no slides.
    pub slides: Option<usize>,
    pub ext: String,
    /// Which converter actually ran: `"native"` or `"anydoc"`.
    pub engine_used: &'static str,
    /// Set when `out` was supplied and a file was written.
    pub written_path: Option<String>,
}

/// Whether a dedicated (non-anydoc) reader exists for this extension.
pub fn has_native_reader(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("pptx")
}

/// Everything this build can convert, native and anydoc together.
///
/// Used by the GUI to decide what to put in a file picker, so it must not
/// advertise formats a feature-less build would then refuse.
pub fn convertible_extensions() -> Vec<String> {
    let mut out = vec!["pptx".to_string()];
    if anydoc_conv::is_available() {
        for e in anydoc_conv::ANYDOC_ONLY_EXTS
            .iter()
            .chain(anydoc_conv::ANYDOC_OVERLAP_EXTS)
        {
            if !out.iter().any(|x| x == e) {
                out.push((*e).to_string());
            }
        }
    }
    out.sort();
    out
}

/// Convert `path`, optionally writing the result to `out`.
///
/// When `out` is supplied every emit writes there, including the text ones —
/// the caller gets the body back as well so a GUI can preview what it just
/// saved without converting twice.
pub fn convert(path: &Path, opts: &ConvertOptions, out: Option<&Path>) -> Result<Converted> {
    if !path.is_file() {
        bail!("not a file: {}", path.display());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let native_available = has_native_reader(&ext);
    if opts.engine == Engine::Native && !native_available {
        bail!(
            "no dedicated reader for `.{ext}` (only .pptx has one); use engine \
             auto or anydoc"
        );
    }
    let use_native = native_available && opts.engine != Engine::Anydoc;

    if opts.emit.is_binary() && out.is_none() {
        bail!("this output is a file; supply a destination path");
    }
    if matches!(opts.emit, Emit::Docx | Emit::Rtf) && !use_native {
        bail!(if native_available {
            "Word and Rich Text output need the native reader; switch the engine \
             away from anydoc"
                .to_string()
        } else {
            format!(
                "Word and Rich Text output are only implemented for .pptx today; \
                 `.{ext}` can emit markdown, text or headings"
            )
        });
    }

    let mut result = if use_native {
        convert_pptx(path, &ext, opts, out)?
    } else {
        convert_via_anydoc(path, &ext, opts)?
    };

    // The native DOCX path writes its own file; everything else is text and
    // is written here so the two paths agree on the reporting.
    if let (Some(p), Some(body)) = (out, result.body.as_deref()) {
        std::fs::write(p, body.as_bytes())
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", p.display()))?;
    }
    if let Some(p) = out {
        result.written_path = Some(p.display().to_string());
    }
    Ok(result)
}

fn convert_pptx(
    path: &Path,
    ext: &str,
    opts: &ConvertOptions,
    out: Option<&Path>,
) -> Result<Converted> {
    let deck = pptx::read_deck(
        path,
        &pptx::ReadOptions {
            include_notes: opts.include_notes,
            include_comments: opts.include_comments,
            order: if opts.visual_order {
                pptx::ShapeOrder::Visual
            } else {
                pptx::ShapeOrder::Xml
            },
        },
    )?;
    let render = pptx::RenderOptions { wrap_width: opts.wrap_width };
    let headings: Vec<String> = deck.slides.iter().map(pptx::Slide::heading).collect();

    let body = match opts.emit {
        Emit::Docx => {
            let p = out.expect("binary emit without a destination is rejected above");
            pptx::write_docx(&deck, &render, p)?;
            None
        }
        Emit::Rtf => Some(pptx::render_rtf(&deck, &render)),
        Emit::Text => Some(pptx::render_text(&deck, &render)),
        Emit::Headings => Some(headings.join("\n")),
        Emit::Markdown => Some(pptx::render_markdown(&deck, &render)),
    };

    Ok(Converted {
        body,
        slides: Some(deck.slides.len()),
        headings,
        ext: ext.to_string(),
        engine_used: "native",
        written_path: None,
    })
}

fn convert_via_anydoc(path: &Path, ext: &str, opts: &ConvertOptions) -> Result<Converted> {
    if !anydoc_conv::is_available() {
        bail!(
            "converting `.{ext}` needs the `anydoc` feature, which this build does \
             not have (rebuild with `--features anydoc`); .pptx works without it"
        );
    }
    if !anydoc_conv::handles(ext) {
        bail!(
            "`.{ext}` is not a convertible document format. For images and scanned \
             pages use OCR instead."
        );
    }

    let doc = anydoc_conv::extract(path, ext)?;
    let body = match opts.emit {
        Emit::Headings => doc.headings.join("\n"),
        Emit::Text => doc
            .full_text
            .lines()
            .map(|l| l.trim_start_matches('#').trim_start())
            .collect::<Vec<_>>()
            .join("\n"),
        // Docx / Rtf are rejected before we get here.
        _ => doc.full_text.clone(),
    };

    Ok(Converted {
        body: Some(body),
        headings: doc.headings,
        slides: None,
        ext: ext.to_string(),
        engine_used: "anydoc",
        written_path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_and_engine_names_round_trip() {
        assert_eq!(Emit::from_name("MD"), Some(Emit::Markdown));
        assert_eq!(Emit::from_name(" rtf "), Some(Emit::Rtf));
        assert_eq!(Emit::from_name("outline"), Some(Emit::Headings));
        assert_eq!(Emit::from_name("pdf"), None);
        assert_eq!(Engine::from_name("anydoc"), Some(Engine::Anydoc));
        assert_eq!(Engine::from_name("nope"), None);
        assert!(Emit::Docx.is_binary());
        assert!(!Emit::Rtf.is_binary(), "rtf is text and can be previewed");
    }

    #[test]
    fn pptx_is_the_only_format_with_a_native_reader() {
        assert!(has_native_reader("pptx"));
        assert!(has_native_reader("PPTX"));
        assert!(!has_native_reader("ppt"), "legacy binary .ppt goes to anydoc");
        assert!(!has_native_reader("docx"));
    }

    #[test]
    fn advertised_formats_never_exceed_what_the_build_can_read() {
        let exts = convertible_extensions();
        assert!(exts.contains(&"pptx".to_string()));
        if !anydoc_conv::is_available() {
            assert_eq!(exts, vec!["pptx".to_string()], "got: {exts:?}");
        } else {
            assert!(exts.contains(&"epub".to_string()));
            // No duplicates, even though pptx is in the overlap list too.
            let mut sorted = exts.clone();
            sorted.dedup();
            assert_eq!(sorted, exts);
        }
    }

    #[test]
    fn engine_native_is_rejected_for_formats_without_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("book.epub");
        std::fs::write(&p, b"stub").unwrap();
        let err = convert(
            &p,
            &ConvertOptions { engine: Engine::Native, ..Default::default() },
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no dedicated reader"), "got: {err}");
    }

    #[test]
    fn binary_output_without_a_destination_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let err = convert(
            &p,
            &ConvertOptions { emit: Emit::Docx, ..Default::default() },
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("destination"), "got: {err}");
    }

    #[test]
    fn word_output_is_refused_when_the_engine_is_forced_to_anydoc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let out = tmp.path().join("x.docx");
        let err = convert(
            &p,
            &ConvertOptions {
                emit: Emit::Docx,
                engine: Engine::Anydoc,
                ..Default::default()
            },
            Some(&out),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("native reader"), "got: {err}");
    }

    #[test]
    fn converting_a_deck_reports_slides_headings_and_the_engine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let got = convert(&p, &ConvertOptions::default(), None).unwrap();
        assert_eq!(got.engine_used, "native");
        assert_eq!(got.slides, Some(2));
        assert_eq!(got.headings, vec!["Slide 1: Erste Folie", "Slide 2"]);
        assert!(got.body.unwrap().contains("## Slide 1: Erste Folie"));
        assert!(got.written_path.is_none());
    }

    #[test]
    fn supplying_a_destination_writes_the_text_output_too() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let out = tmp.path().join("deck.md");
        let got = convert(&p, &ConvertOptions::default(), Some(&out)).unwrap();
        assert_eq!(got.written_path.as_deref(), Some(out.to_string_lossy().as_ref()));
        let on_disk = std::fs::read_to_string(&out).unwrap();
        // The caller gets the same bytes back for preview — no second convert.
        assert_eq!(Some(on_disk), got.body);
    }

    #[test]
    fn the_read_opt_outs_reach_the_reader() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let got = convert(
            &p,
            &ConvertOptions {
                include_notes: false,
                include_comments: false,
                visual_order: false,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let body = got.body.unwrap();
        assert!(!body.contains("Nicht zu schnell sprechen"), "{body}");
        assert!(!body.contains("Jana"), "{body}");
        // XML order puts the bottom shape first.
        let bottom = body.find("BOTTOM stored first").unwrap();
        let middle = body.find("MIDDLE stored last").unwrap();
        assert!(bottom < middle, "visual_order=false must keep XML order:\n{body}");
    }

    #[cfg(feature = "anydoc")]
    #[test]
    fn forcing_anydoc_on_a_deck_changes_the_converter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = crate::extractors::pptx::tests_support::write_sample_deck(tmp.path());
        let generic = convert(
            &p,
            &ConvertOptions { engine: Engine::Anydoc, ..Default::default() },
            None,
        )
        .unwrap();
        assert_eq!(generic.engine_used, "anydoc");
        assert_eq!(generic.slides, None, "anydoc has no slide model");
        let body = generic.body.unwrap();
        assert!(body.contains("Erste Folie"), "{body}");
        assert!(!body.contains("## Slide 1:"), "{body}");
    }
}
