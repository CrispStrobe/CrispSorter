//! Kindle clippings → annotation store (P32.4, the wiring).
//!
//! Joins [`crate::kindle_clippings`] (parse) to [`crate::kindle_match`]
//! (locate in the document) and lands the result in the `highlights`
//! table, where the reading list and FTS already look.
//!
//! Matching is optional. Without a document to match against, clippings
//! still import — they simply carry no offsets, so they are listed but
//! not positioned. That is strictly better than refusing the import,
//! which is why `document_text` is an `Option`.

use crate::index::annotations::AnnotationStore;
use crate::kindle_clippings::{dedupe, parse_clippings, Clipping, ClippingKind};
use crate::kindle_match::{find_passage, MatchMethod};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default similarity floor for the fuzzy tier. Below this a passage is
/// left unanchored rather than pinned to the wrong paragraph.
pub const DEFAULT_MIN_SCORE: f32 = 0.82;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportSummary {
    /// Records found in the file.
    pub parsed: usize,
    /// Records left after collapsing the device's duplicate runs.
    pub deduped: usize,
    /// Rows written to the store.
    pub imported: usize,
    /// Of those, how many were located in the document text.
    pub matched: usize,
    /// Located only by the fuzzy tier — worth surfacing, since these are
    /// the ones most likely to be slightly off.
    pub fuzzy_matched: usize,
    /// Titles seen, for a "which books did this cover?" confirmation.
    pub titles: Vec<String>,
    /// Skipped as already present.
    pub duplicates_skipped: usize,
}

/// Read a document's plain text for matching.
///
/// Deliberately narrow: PDFs and plain text. Wiring the whole extractor
/// stack in here would drag OCR and its model downloads into what should
/// be a cheap import.
pub fn document_text_for(path: &Path) -> Result<String, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => pdf_extract::extract_text(path).map_err(|e| format!("extract text: {e}")),
        "txt" | "md" | "markdown" | "text" => {
            std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))
        }
        other => Err(format!(
            "cannot read text from .{other} here — pass the text directly, \
             or use a .pdf/.txt/.md source"
        )),
    }
}

fn color_for(kind: ClippingKind) -> &'static str {
    match kind {
        // Same defaults the annotations schema uses.
        ClippingKind::Highlight => "#facc15",
        ClippingKind::Note => "#60a5fa",
        ClippingKind::Bookmark => "#a1a1aa",
    }
}

/// Import clippings for one document.
///
/// `title_filter`, when given, restricts the import to clippings whose
/// title contains it — a `My Clippings.txt` covers a whole library, and
/// importing all of it against one document would be nonsense.
pub fn import_clippings(
    store: &AnnotationStore,
    clippings_text: &str,
    doc_id: &str,
    title_filter: Option<&str>,
    document_text: Option<&str>,
    min_score: f32,
) -> Result<ImportSummary, String> {
    let all = parse_clippings(clippings_text);
    let mut summary = ImportSummary { parsed: all.len(), ..Default::default() };

    let filtered: Vec<Clipping> = match title_filter {
        Some(f) if !f.trim().is_empty() => {
            let needle = f.to_lowercase();
            all.into_iter()
                .filter(|c| c.title.to_lowercase().contains(&needle))
                .collect()
        }
        _ => all,
    };

    let clippings = dedupe(filtered);
    summary.deduped = clippings.len();

    let existing = store.get_highlights(doc_id).map_err(|e| e.to_string())?;
    let seen: std::collections::HashSet<String> =
        existing.iter().map(|h| h.text.trim().to_string()).collect();

    for c in &clippings {
        if !summary.titles.contains(&c.title) {
            summary.titles.push(c.title.clone());
        }
        // Bookmarks carry no passage; there is nothing to store or match.
        if c.kind == ClippingKind::Bookmark {
            continue;
        }
        if seen.contains(c.text.trim()) {
            summary.duplicates_skipped += 1;
            continue;
        }

        let (start, end) = match document_text {
            Some(doc) => match find_passage(doc, &c.text, min_score) {
                Some(m) => {
                    summary.matched += 1;
                    if m.method == MatchMethod::Fuzzy {
                        summary.fuzzy_matched += 1;
                    }
                    (m.start as i32, m.end as i32)
                }
                None => (0, 0),
            },
            None => (0, 0),
        };

        // The note text goes in `note`; for a highlight it holds the
        // provenance line so the user can trace it back to the device.
        let note = c
            .added_raw
            .clone()
            .unwrap_or_else(|| "imported from Kindle clippings".to_string());

        store
            .add_highlight(doc_id, 0, start, end, &c.text, &note, color_for(c.kind))
            .map_err(|e| e.to_string())?;
        summary.imported += 1;
    }

    Ok(summary)
}

pub mod tauri_commands {
    use super::*;
    use crate::AppState;
    use tauri::State;

    async fn get_store(state: &State<'_, AppState>) -> Result<AnnotationStore, String> {
        let data_dir = state.data_dir.lock().await;
        let dir = data_dir.as_ref().ok_or("App data dir not set")?;
        AnnotationStore::open_or_create(dir).map_err(|e| e.to_string())
    }

    /// Distinct book titles in a `My Clippings.txt`, sorted.
    ///
    /// Shared by the Tauri command and the CLI so both list the same
    /// thing from the same parse.
    pub fn kindle_list_books_sync(path: &Path) -> Result<Vec<String>, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut titles: Vec<String> = Vec::new();
        for c in parse_clippings(&text) {
            if !titles.contains(&c.title) {
                titles.push(c.title);
            }
        }
        titles.sort();
        Ok(titles)
    }

    /// List the distinct books in a `My Clippings.txt`, so the UI can ask
    /// which one to import before doing any work.
    #[tauri::command]
    pub async fn kindle_list_books(clippings_path: String) -> Result<Vec<String>, String> {
        tokio::task::spawn_blocking(move || kindle_list_books_sync(Path::new(&clippings_path)))
            .await
            .map_err(|e| format!("join: {e}"))?
    }

    /// Import a `My Clippings.txt` into the annotation store.
    ///
    /// `document_path` is the file to anchor the passages in. It is
    /// optional: without it the clippings still import, just unpositioned,
    /// which beats refusing the import outright.
    #[tauri::command]
    #[allow(clippy::too_many_arguments)]
    pub async fn kindle_import(
        state: State<'_, AppState>,
        clippings_path: String,
        doc_id: String,
        title_filter: Option<String>,
        document_path: Option<String>,
        min_score: Option<f32>,
    ) -> Result<ImportSummary, String> {
        let store = get_store(&state).await?;
        tokio::task::spawn_blocking(move || {
            let text = std::fs::read_to_string(&clippings_path)
                .map_err(|e| format!("read {clippings_path}: {e}"))?;
            let doc_text = match document_path {
                Some(p) => Some(document_text_for(Path::new(&p))?),
                None => None,
            };
            import_clippings(
                &store,
                &text,
                &doc_id,
                title_filter.as_deref(),
                doc_text.as_deref(),
                min_score.unwrap_or(DEFAULT_MIN_SCORE),
            )
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIPPINGS: &str = "Thinking, Fast and Slow (Daniel Kahneman)\n\
- Your Highlight on page 12 | Location 234-236 | Added on Monday, 1 January 2024 12:00:00\n\
\n\
The confidence people have in their beliefs\n\
==========\n\
Another Book (Someone Else)\n\
- Your Highlight on page 3 | Location 10-12 | Added on Tuesday, 2 January 2024 12:00:00\n\
\n\
Text from a different book entirely\n\
==========\n";

    const DOC: &str = "Chapter One. The confidence people have in their beliefs is not a \
measure of the quality of evidence.";

    fn store() -> (AnnotationStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = AnnotationStore::open_or_create(dir.path()).unwrap();
        (s, dir)
    }

    #[test]
    fn title_filter_restricts_to_one_book() {
        let (s, _d) = store();
        let sum = import_clippings(&s, CLIPPINGS, "doc1", Some("Thinking"), None, DEFAULT_MIN_SCORE)
            .unwrap();
        assert_eq!(sum.parsed, 2);
        assert_eq!(sum.deduped, 1);
        assert_eq!(sum.imported, 1);
        assert_eq!(sum.titles, vec!["Thinking, Fast and Slow"]);
    }

    #[test]
    fn without_a_document_clippings_still_import_unpositioned() {
        let (s, _d) = store();
        let sum = import_clippings(&s, CLIPPINGS, "doc1", None, None, DEFAULT_MIN_SCORE).unwrap();
        assert_eq!(sum.imported, 2);
        assert_eq!(sum.matched, 0);
        let rows = s.get_highlights("doc1").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|h| h.start_offset == 0 && h.end_offset == 0));
    }

    #[test]
    fn with_a_document_passages_get_real_offsets() {
        let (s, _d) = store();
        let sum =
            import_clippings(&s, CLIPPINGS, "doc1", Some("Thinking"), Some(DOC), DEFAULT_MIN_SCORE)
                .unwrap();
        assert_eq!(sum.matched, 1);
        let rows = s.get_highlights("doc1").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].end_offset > rows[0].start_offset, "offsets not set: {:?}", rows[0]);
        let slice = &DOC[rows[0].start_offset as usize..rows[0].end_offset as usize];
        assert!(slice.contains("confidence"), "offsets point at the wrong text: {slice:?}");
    }

    #[test]
    fn a_passage_absent_from_the_document_imports_unanchored() {
        let (s, _d) = store();
        // "Another Book" text does not occur in DOC.
        let sum = import_clippings(&s, CLIPPINGS, "doc1", Some("Another"), Some(DOC), DEFAULT_MIN_SCORE)
            .unwrap();
        assert_eq!(sum.imported, 1);
        assert_eq!(sum.matched, 0, "must not force a match onto unrelated text");
    }

    #[test]
    fn re_importing_skips_what_is_already_there() {
        let (s, _d) = store();
        import_clippings(&s, CLIPPINGS, "doc1", None, None, DEFAULT_MIN_SCORE).unwrap();
        let again = import_clippings(&s, CLIPPINGS, "doc1", None, None, DEFAULT_MIN_SCORE).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.duplicates_skipped, 2);
        assert_eq!(s.get_highlights("doc1").unwrap().len(), 2);
    }

    #[test]
    fn bookmarks_are_counted_but_not_stored() {
        let src = "A Book (An Author)\n\
- Your Bookmark on page 7 | Location 99 | Added on X\n\
\n\
==========\n";
        let (s, _d) = store();
        let sum = import_clippings(&s, src, "doc1", None, None, DEFAULT_MIN_SCORE).unwrap();
        assert_eq!(sum.deduped, 1);
        assert_eq!(sum.imported, 0, "a bookmark has no passage to store");
        assert!(s.get_highlights("doc1").unwrap().is_empty());
    }

    #[test]
    fn unreadable_document_type_is_reported_clearly() {
        let err = document_text_for(Path::new("/tmp/whatever.docx")).unwrap_err();
        assert!(err.contains("docx"), "error should name the extension: {err}");
    }
}
