//! anydoc-backed document conversion.
//!
//! [`anydoc`](https://github.com/firecrawl/anydoc) is a pure-Rust
//! office-document → GitHub-Flavored-Markdown converter (MIT). It fills the
//! formats this tree never had a native extractor for — PowerPoint,
//! spreadsheets, OpenDocument, RTF, EPUB and legacy `.doc` — without a
//! subprocess, an ML model or a network call, so it is sandbox-safe for the
//! Mac App Store SKU (unlike shelling out to LibreOffice or pandoc).
//!
//! Two entry paths, mirroring the two things "conversion" can mean here:
//!
//! * [`extract`] — the ingest path. Converts to Markdown and maps it onto
//!   [`ExtractedDocument`] so office formats flow into the same
//!   embedding + Tantivy pipeline as everything else.
//! * [`to_markdown`] — the user-facing path, behind `crispsorter convert`.
//!   Hands back the Markdown verbatim for writing to a file or stdout.
//!
//! **Deliberately not the default for `pdf` / `docx` / `csv`.** Those already
//! have native extractors that do strictly more: the PDF path carries a
//! four-tier OCR ladder plus layout and math recognition, and the DOCX path
//! goes through `crisp-docx-core`, which infers heading levels from direct
//! formatting. anydoc only ever sees those files when the caller explicitly
//! asks via [`AnydocMode::Prefer`] — see [`try_preferred`].
//!
//! Headings are recovered by re-parsing the ATX (`#`) lines out of the
//! generated Markdown rather than by reaching into `anydoc::model::Document`.
//! The Markdown surface is the crate's stable, documented contract; the
//! information model is 0.1.x and still moving.

use crate::extractors::{AnydocMode, ExtractedDocument};
use anyhow::Result;
use std::path::Path;

/// Extensions anydoc converts that have **no** native extractor in this
/// tree. These are pure gain — before anydoc they hit the
/// `no extractor for .ext` arm and were indexed as L1 metadata only.
pub const ANYDOC_ONLY_EXTS: &[&str] = &[
    // Word — legacy binary + macro-enabled. `.docx` is NOT here: it belongs
    // to crisp-docx-core.
    "doc", "docm", //
    // PowerPoint. `.pptx` is NOT here — it has a native reader in
    // `super::pptx` that recovers visual order and comments. The other
    // PowerPoint spellings have no native path.
    "ppt", "pptm", "pps", "ppsx", "ppsm", "pot", //
    // Excel.
    "xls", "xlsx", "xlsm", "xlsb", //
    // OpenDocument text / spreadsheet / presentation.
    "odt", "ods", "odp", //
    // Rich Text + e-books.
    "rtf", "epub",
];

/// Extensions anydoc supports that already have a **better** native
/// extractor. Only reachable via [`AnydocMode::Prefer`].
pub const ANYDOC_OVERLAP_EXTS: &[&str] = &["pdf", "docx", "csv", "pptx"];

/// A conversion shorter than this is treated as a failed parse, so the
/// caller falls back to the native extractor instead of indexing an empty
/// document. Picked to clear "a stray heading and nothing else" without
/// discarding genuinely tiny files.
const MIN_USEFUL_CHARS: usize = 16;

/// Whether the `anydoc` feature was compiled in.
pub fn is_available() -> bool {
    cfg!(feature = "anydoc")
}

/// Every extension anydoc can handle, native-covered ones included.
pub fn handles(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    ANYDOC_ONLY_EXTS.contains(&e.as_str()) || ANYDOC_OVERLAP_EXTS.contains(&e.as_str())
}

/// Whether this extension reaches anydoc under `mode`.
///
/// `Auto` (the default) restricts it to the formats nothing else covers;
/// `Prefer` opens up the overlap set too; `Never` disables it outright.
pub fn will_handle(ext: &str, mode: AnydocMode) -> bool {
    if !is_available() {
        return false;
    }
    let e = ext.to_ascii_lowercase();
    match mode {
        AnydocMode::Never => false,
        AnydocMode::Auto => ANYDOC_ONLY_EXTS.contains(&e.as_str()),
        AnydocMode::Prefer => handles(&e),
    }
}

/// Convert `path` to Markdown, verbatim.
///
/// This is the raw conversion — no heading extraction, no
/// [`ExtractedDocument`] wrapping. Used by `crispsorter convert`.
#[cfg(feature = "anydoc")]
pub fn to_markdown(path: &Path) -> Result<String> {
    anydoc::to_markdown(path)
        .map_err(|e| anyhow::anyhow!("anydoc could not convert {}: {e}", path.display()))
}

#[cfg(not(feature = "anydoc"))]
pub fn to_markdown(path: &Path) -> Result<String> {
    Err(unavailable(path))
}

/// The error every stub path returns. Actionable on purpose: the most
/// likely reader is someone who just hit an office file on a build that
/// did not compile the converter in.
#[cfg(not(feature = "anydoc"))]
fn unavailable(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "converting {} needs the `anydoc` feature, which this build does not \
         have (rebuild with `--features anydoc`)",
        path.display()
    )
}

/// Convert `path` and map the Markdown onto an [`ExtractedDocument`].
///
/// The Markdown is kept as the body rather than being flattened to plain
/// text: `#` markers give the BM25 heading boost something to bite on, and
/// GFM tables keep spreadsheet cells on their original rows, which plain
/// text would smear together.
pub fn extract(path: &Path, ext: &str) -> Result<ExtractedDocument> {
    let markdown = to_markdown(path)?;
    let headings = headings_from_markdown(&markdown);
    Ok(ExtractedDocument {
        full_text: markdown,
        headings,
        ext: ext.to_ascii_lowercase(),
        ..Default::default()
    })
}

/// Try anydoc *first* for a format that also has a native extractor.
///
/// Returns `None` — meaning "run the native extractor" — whenever anydoc is
/// not wanted here, is not compiled in, fails, or produces too little text
/// to be worth indexing. That last case is the important one: a scanned PDF
/// converts "successfully" into almost nothing, and falling through gets it
/// to the OCR ladder instead of indexing a blank document.
pub fn try_preferred(path: &Path, ext: &str, mode: AnydocMode) -> Option<ExtractedDocument> {
    if mode != AnydocMode::Prefer || !is_available() {
        return None;
    }
    if !handles(ext) {
        return None;
    }
    match extract(path, ext) {
        Ok(doc) if doc.full_text.trim().chars().count() >= MIN_USEFUL_CHARS => Some(doc),
        Ok(_) => {
            eprintln!(
                "[anydoc] {} converted to (near-)empty text; falling back to the \
                 native extractor",
                path.display()
            );
            None
        }
        Err(e) => {
            eprintln!("[anydoc] {e:#}; falling back to the native extractor");
            None
        }
    }
}

/// Pull ATX headings (`# …` through `###### …`) out of Markdown.
///
/// Fenced code blocks are skipped so a shell comment inside a ``` block
/// does not get promoted into the boosted headings field — spreadsheets
/// and slide decks carrying code snippets hit this in practice.
fn headings_from_markdown(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !trimmed.starts_with('#') {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&hashes) {
            continue;
        }
        // ATX requires whitespace between the run of `#` and the text;
        // `#hashtag` is a word, not a heading.
        let rest = &trimmed[hashes..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        // Trailing `#`s are decoration in closed ATX form (`## Title ##`).
        let text = rest.trim().trim_end_matches('#').trim();
        if !text.is_empty() {
            out.push(text.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_and_overlap_sets_are_disjoint() {
        for e in ANYDOC_ONLY_EXTS {
            assert!(
                !ANYDOC_OVERLAP_EXTS.contains(e),
                "`{e}` is in both the anydoc-only and the overlap set"
            );
        }
    }

    #[test]
    fn docx_is_not_claimed_as_anydoc_only() {
        // crisp-docx-core owns .docx — it infers heading levels, which
        // anydoc cannot. Regression guard against someone "completing"
        // the Word list.
        assert!(!ANYDOC_ONLY_EXTS.contains(&"docx"));
        assert!(ANYDOC_OVERLAP_EXTS.contains(&"docx"));
    }

    #[test]
    fn auto_covers_only_the_uncovered_formats() {
        if !is_available() {
            return; // stub build: will_handle is always false
        }
        assert!(will_handle("odp", AnydocMode::Auto));
        assert!(will_handle("EPUB", AnydocMode::Auto), "case-insensitive");
        // Native extractors keep these under Auto — including `.pptx`,
        // which `super::pptx` reads with visual ordering and comments.
        assert!(!will_handle("pdf", AnydocMode::Auto));
        assert!(!will_handle("docx", AnydocMode::Auto));
        assert!(!will_handle("csv", AnydocMode::Auto));
        assert!(!will_handle("pptx", AnydocMode::Auto));
        // The legacy binary spelling has no native reader, so it stays.
        assert!(will_handle("ppt", AnydocMode::Auto));
    }

    #[test]
    fn prefer_opens_up_the_overlap_set() {
        if !is_available() {
            return;
        }
        assert!(will_handle("pdf", AnydocMode::Prefer));
        assert!(will_handle("docx", AnydocMode::Prefer));
        assert!(will_handle("pptx", AnydocMode::Prefer));
        // Still not a converter for things it cannot read.
        assert!(!will_handle("mp3", AnydocMode::Prefer));
        assert!(!will_handle("png", AnydocMode::Prefer));
    }

    #[test]
    fn never_disables_every_format() {
        for e in ANYDOC_ONLY_EXTS.iter().chain(ANYDOC_OVERLAP_EXTS) {
            assert!(!will_handle(e, AnydocMode::Never));
        }
    }

    #[test]
    fn try_preferred_declines_unless_asked() {
        let p = Path::new("/nonexistent/deck.pptx");
        assert!(try_preferred(p, "pptx", AnydocMode::Auto).is_none());
        assert!(try_preferred(p, "pptx", AnydocMode::Never).is_none());
    }

    #[test]
    fn try_preferred_falls_back_when_conversion_fails() {
        // Missing file → anydoc errors → None, so the caller runs native.
        let p = Path::new("/nonexistent/missing.pdf");
        assert!(try_preferred(p, "pdf", AnydocMode::Prefer).is_none());
    }

    #[test]
    fn headings_are_lifted_from_atx_lines() {
        let md = "# Title\n\nbody text\n\n## Section ##\n\n### Deep\n";
        assert_eq!(
            headings_from_markdown(md),
            vec!["Title", "Section", "Deep"]
        );
    }

    #[test]
    fn headings_skip_fenced_code_and_hashtags() {
        let md = "# Real\n\n```sh\n# not a heading\n```\n\n#hashtag\n\n####### too deep\n";
        assert_eq!(headings_from_markdown(md), vec!["Real"]);
    }

    #[test]
    fn headings_of_plain_text_are_empty() {
        assert!(headings_from_markdown("no headings here\nat all\n").is_empty());
    }

    /// End-to-end conversion against a real document, not a mock.
    ///
    /// RTF is the one format in the set whose container is plain text, so
    /// the fixture can be written inline — every other format would mean
    /// committing a binary blob to get the same coverage.
    #[cfg(feature = "anydoc")]
    #[test]
    fn converts_a_real_rtf_document_end_to_end() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("kapitel.rtf");
        std::fs::write(
            &p,
            br"{\rtf1\ansi\deff0 {\b Kapitel Eins}\par Der Fliesstext steht hier.\par}",
        )
        .unwrap();

        let doc = extract(&p, "rtf").expect("rtf should convert");
        assert_eq!(doc.ext, "rtf");
        assert!(
            doc.full_text.contains("Kapitel Eins"),
            "heading text lost: {:?}",
            doc.full_text
        );
        assert!(
            doc.full_text.contains("Der Fliesstext steht hier."),
            "body text lost: {:?}",
            doc.full_text
        );
    }

    /// The dispatcher only calls `try_preferred` for pdf/docx/csv, but the
    /// guard lives in this module, so it is tested here: a format anydoc
    /// handles well must still be declined when the mode is not `Prefer`.
    #[cfg(feature = "anydoc")]
    #[test]
    fn try_preferred_returns_the_conversion_when_asked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("rows.csv");
        std::fs::write(&p, b"Region,Revenue\nNord,12500\nSued,9800\n").unwrap();

        assert!(
            try_preferred(&p, "csv", AnydocMode::Auto).is_none(),
            "csv has a native path; Auto must not divert it"
        );
        let doc = try_preferred(&p, "csv", AnydocMode::Prefer)
            .expect("Prefer should convert a well-formed csv");
        assert!(
            doc.full_text.contains("Nord") && doc.full_text.contains("12500"),
            "cell values lost: {:?}",
            doc.full_text
        );
    }

    #[cfg(not(feature = "anydoc"))]
    #[test]
    fn stub_build_reports_unavailable_with_an_actionable_message() {
        assert!(!is_available());
        let err = to_markdown(Path::new("/x/deck.pptx")).unwrap_err().to_string();
        assert!(err.contains("--features anydoc"), "got: {err}");
    }
}
