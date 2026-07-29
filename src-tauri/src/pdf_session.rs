//! Stateful PDF editing sessions (P32.1a).
//!
//! The functions in [`crate::pdf_ops`] are one-shot: they take an input
//! path, apply one operation, and write an output path.  That shape is
//! right for the CLI, but it forces the GUI to raise a save dialog per
//! edit, which rules out the direct-manipulation page editor.
//!
//! A session loads a document once, keeps a *pristine* copy plus an
//! ordered list of [`EditOp`]s, and materialises the edited document by
//! replaying those ops.  Undo/redo is then just moving a cursor along
//! that list — no snapshot of the document per step, and the op list is
//! serialisable, so an edit history can be inspected or persisted.
//!
//! ## Page identity
//!
//! Replaying ops tells us what the document looks like, but the GUI also
//! needs to know *where each page came from* so it can render thumbnails
//! once and then reorder DOM nodes locally instead of re-rasterising the
//! whole document after every drag.  So the session tracks a parallel
//! [`PageOrigin`] list, permuted by the same ops.  A page that still
//! carries `{source: <original file>, index: 3}` is bit-identical to page
//! 3 of the file the user opened, whatever has happened to its position.

use crate::pdf_ops::{
    self, MetadataEdit, PageNumberConfig, PdfInfo, TextBoxConfig, WatermarkConfig,
};
use lopdf::Document;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ── Edit operations ────────────────────────────────────────────────────

/// One reversible edit.  Reversible in the sense that the session can
/// reconstruct any prior state by replaying a prefix of the op list —
/// individual ops are not required to have an inverse.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditOp {
    /// New page order, as indices into the *current* page list.
    Reorder { order: Vec<usize> },
    RemovePages { pages: Vec<usize> },
    /// Keep only these pages, in this order (extract-in-place).
    KeepPages { pages: Vec<usize> },
    Rotate { pages: Vec<usize>, degrees: i64 },
    Crop { pages: Vec<usize>, x: f64, y: f64, w: f64, h: f64 },
    InsertBlank { position: usize, width: f64, height: f64 },
    /// Insert pages from another file.  Empty `pages` means all of them.
    InsertFrom { path: String, pages: Vec<usize>, position: usize },
    PageNumbers { config: PageNumberConfig },
    Watermark { config: WatermarkConfig, pages: Option<Vec<usize>> },
    TextBox { config: TextBoxConfig },
    Metadata { edits: MetadataEdit },
}

impl EditOp {
    /// Short human-readable label, for the undo affordance in the UI.
    /// Deliberately English — the frontend maps these to i18n keys.
    pub fn label(&self) -> &'static str {
        match self {
            EditOp::Reorder { .. } => "reorder_pages",
            EditOp::RemovePages { .. } => "remove_pages",
            EditOp::KeepPages { .. } => "extract_pages",
            EditOp::Rotate { .. } => "rotate_pages",
            EditOp::Crop { .. } => "crop_pages",
            EditOp::InsertBlank { .. } => "insert_blank",
            EditOp::InsertFrom { .. } => "insert_pages",
            EditOp::PageNumbers { .. } => "page_numbers",
            EditOp::Watermark { .. } => "watermark",
            EditOp::TextBox { .. } => "text_box",
            EditOp::Metadata { .. } => "metadata",
        }
    }

    fn apply(&self, doc: &mut Document) -> Result<(), String> {
        match self {
            EditOp::Reorder { order } => pdf_ops::reorder_pages_doc(doc, order),
            EditOp::RemovePages { pages } => pdf_ops::remove_pages_doc(doc, pages),
            EditOp::KeepPages { pages } => pdf_ops::extract_pages_doc(doc, pages),
            EditOp::Rotate { pages, degrees } => pdf_ops::rotate_pages_doc(doc, pages, *degrees),
            EditOp::Crop { pages, x, y, w, h } => pdf_ops::crop_pages_doc(doc, pages, *x, *y, *w, *h),
            EditOp::InsertBlank { position, width, height } => {
                pdf_ops::insert_blank_page_doc(doc, *position, *width, *height)
            }
            EditOp::InsertFrom { path, pages, position } => {
                pdf_ops::insert_pages_from_doc(doc, Path::new(path), pages, *position).map(|_| ())
            }
            EditOp::PageNumbers { config } => pdf_ops::add_page_numbers_doc(doc, config),
            EditOp::Watermark { config, pages } => {
                pdf_ops::add_watermark_doc(doc, config, pages.as_deref())
            }
            EditOp::TextBox { config } => pdf_ops::add_text_box_doc(doc, config),
            EditOp::Metadata { edits } => pdf_ops::edit_metadata_doc(doc, edits),
        }
    }

    /// Mirror `apply` on the page-origin list.
    ///
    /// Ops that only stamp content onto existing pages leave the list
    /// alone but mark the affected pages dirty, so the GUI knows which
    /// thumbnails to re-render.
    fn apply_origins(&self, origins: &mut Vec<PageOrigin>) -> Result<(), String> {
        let n = origins.len();
        match self {
            EditOp::Reorder { order } => {
                for &i in order {
                    if i >= n { return Err(format!("page index {i} out of range (0..{n})")); }
                }
                *origins = order.iter().map(|&i| origins[i].clone()).collect();
            }
            EditOp::RemovePages { pages } => {
                let drop: std::collections::HashSet<usize> = pages.iter().copied().collect();
                for &i in &drop {
                    if i >= n { return Err(format!("page {i} out of range")); }
                }
                let kept: Vec<PageOrigin> = origins
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !drop.contains(i))
                    .map(|(_, o)| o.clone())
                    .collect();
                if kept.is_empty() { return Err("Cannot remove all pages".into()); }
                *origins = kept;
            }
            EditOp::KeepPages { pages } => {
                for &i in pages {
                    if i >= n { return Err(format!("page {i} out of range")); }
                }
                *origins = pages.iter().map(|&i| origins[i].clone()).collect();
            }
            EditOp::Rotate { pages, degrees } => {
                for &i in pages {
                    if i >= n { return Err(format!("page {i} out of range")); }
                    let o = &mut origins[i];
                    o.rotation = ((o.rotation + degrees) % 360 + 360) % 360;
                }
            }
            EditOp::Crop { pages, .. } => {
                for &i in pages {
                    if i >= n { return Err(format!("page {i} out of range")); }
                    origins[i].dirty = true;
                }
            }
            EditOp::InsertBlank { position, .. } => {
                if *position > n { return Err(format!("position {position} out of range (0..={n})")); }
                origins.insert(*position, PageOrigin::blank());
            }
            EditOp::InsertFrom { path, pages, position } => {
                if *position > n { return Err(format!("position {position} out of range (0..={n})")); }
                // An empty selection means "all pages", but the count is
                // only known once the file is read; the doc-side apply is
                // authoritative, so read the page count here too.
                let indices: Vec<usize> = if pages.is_empty() {
                    let src = Document::load(Path::new(path))
                        .map_err(|e| format!("load {path}: {e}"))?;
                    (0..src.page_iter().count()).collect()
                } else {
                    pages.clone()
                };
                for (k, &idx) in indices.iter().enumerate() {
                    origins.insert(*position + k, PageOrigin::from_file(path, idx));
                }
            }
            EditOp::PageNumbers { .. } => {
                for o in origins.iter_mut() { o.dirty = true; }
            }
            EditOp::Watermark { pages, .. } => match pages {
                Some(list) => {
                    for &i in list {
                        if i >= n { return Err(format!("page {i} out of range")); }
                        origins[i].dirty = true;
                    }
                }
                None => { for o in origins.iter_mut() { o.dirty = true; } }
            },
            EditOp::TextBox { config } => {
                if config.page >= n { return Err(format!("page {} out of range", config.page)); }
                origins[config.page].dirty = true;
            }
            EditOp::Metadata { .. } => {}
        }
        Ok(())
    }
}

// ── Page identity ──────────────────────────────────────────────────────

/// Where a page in the edited document came from.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct PageOrigin {
    /// File this page was read from.  `None` for a generated blank page.
    pub source: Option<String>,
    /// 0-based page index within `source`.
    pub index: Option<usize>,
    /// Accumulated rotation in degrees, normalised to 0..360.
    pub rotation: i64,
    /// Page content has been stamped since it was read, so a cached
    /// thumbnail of the source page is no longer accurate.
    pub dirty: bool,
}

impl PageOrigin {
    fn from_file(source: &str, index: usize) -> Self {
        Self { source: Some(source.to_string()), index: Some(index), rotation: 0, dirty: false }
    }
    fn blank() -> Self {
        Self { source: None, index: None, rotation: 0, dirty: false }
    }
}

// ── Session ────────────────────────────────────────────────────────────

/// A snapshot of session state, sent to the frontend after every change.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionState {
    pub id: String,
    /// Path the session was opened from.
    pub source: String,
    /// Page count / dimensions / metadata of the *edited* document.
    pub info: PdfInfo,
    /// Provenance per page, parallel to `info.pages`.
    pub origins: Vec<PageOrigin>,
    pub can_undo: bool,
    pub can_redo: bool,
    /// Label of the op that undo would reverse, if any.
    pub undo_label: Option<String>,
    /// Unsaved changes pending.
    pub modified: bool,
    /// Full op list, for an edit-history view.
    pub ops: Vec<String>,
}

pub struct PdfEditSession {
    source: PathBuf,
    /// Pristine document as loaded; never mutated.
    base: Document,
    base_origins: Vec<PageOrigin>,
    ops: Vec<EditOp>,
    /// Number of ops currently applied.  `ops[cursor..]` are redoable.
    cursor: usize,
    /// Materialised document at `cursor`, invalidated on any move.
    cache: Option<(Document, Vec<PageOrigin>)>,
    /// Ops applied at the point of the last successful save.
    saved_at: usize,
    /// Scratch directory for temp copies, created on first use and reused.
    /// One per session rather than one per export, so repeatedly printing
    /// or re-rendering the preview does not strew directories around.
    temp_dir: Option<PathBuf>,
}

impl Drop for PdfEditSession {
    fn drop(&mut self) {
        if let Some(dir) = self.temp_dir.take() {
            // Best-effort: a failure here just leaves the OS to reclaim it.
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl PdfEditSession {
    pub fn open(path: &Path) -> Result<Self, String> {
        let base = Document::load(path).map_err(|e| format!("load: {e}"))?;
        let src = path.to_string_lossy().to_string();
        let base_origins = (0..base.page_iter().count())
            .map(|i| PageOrigin::from_file(&src, i))
            .collect();
        Ok(Self {
            source: path.to_path_buf(),
            base,
            base_origins,
            ops: Vec::new(),
            cursor: 0,
            cache: None,
            saved_at: 0,
            temp_dir: None,
        })
    }

    /// Materialise the document at the current cursor, replaying from the
    /// pristine copy.  Cached until the cursor or op list changes.
    fn materialise(&mut self) -> Result<&(Document, Vec<PageOrigin>), String> {
        if self.cache.is_none() {
            let mut doc = self.base.clone();
            let mut origins = self.base_origins.clone();
            for op in &self.ops[..self.cursor] {
                op.apply(&mut doc)?;
                op.apply_origins(&mut origins)?;
            }
            self.cache = Some((doc, origins));
        }
        Ok(self.cache.as_ref().unwrap())
    }

    /// Apply a new op.  Anything previously undone is discarded, which is
    /// the conventional linear-history behaviour.
    pub fn apply(&mut self, op: EditOp) -> Result<(), String> {
        // Validate against a throwaway replay before committing, so a
        // rejected op leaves the session exactly as it was.
        let (doc, origins) = self.materialise()?;
        let mut probe_doc = doc.clone();
        let mut probe_origins = origins.clone();
        op.apply(&mut probe_doc)?;
        op.apply_origins(&mut probe_origins)?;

        self.ops.truncate(self.cursor);
        self.ops.push(op);
        self.cursor += 1;
        if self.saved_at > self.cursor { self.saved_at = usize::MAX; }
        self.cache = Some((probe_doc, probe_origins));
        Ok(())
    }

    pub fn undo(&mut self) -> Result<(), String> {
        if self.cursor == 0 { return Err("nothing to undo".into()); }
        self.cursor -= 1;
        self.cache = None;
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), String> {
        if self.cursor >= self.ops.len() { return Err("nothing to redo".into()); }
        self.cursor += 1;
        self.cache = None;
        Ok(())
    }

    pub fn modified(&self) -> bool {
        self.saved_at != self.cursor
    }

    /// Serialise the edited document to `out_path`.  `None` overwrites the
    /// file the session was opened from.
    pub fn save(&mut self, out_path: Option<&Path>) -> Result<PathBuf, String> {
        let target = out_path.map(|p| p.to_path_buf()).unwrap_or_else(|| self.source.clone());
        {
            let (doc, _) = self.materialise()?;
            let mut doc = doc.clone();
            doc.save(&target).map_err(|e| format!("save: {e}"))?;
        }
        self.saved_at = self.cursor;
        Ok(target)
    }

    /// Write a throwaway copy of the edited document.
    ///
    /// Unlike [`save`](Self::save) this does not change the saved/dirty
    /// state: a temp copy backs print, share and extract-selection, none
    /// of which persist the user's document.
    pub fn save_temp_copy(&mut self, out_path: &Path) -> Result<PathBuf, String> {
        let prev_saved_at = self.saved_at;
        let result = self.save(Some(out_path));
        self.saved_at = prev_saved_at;
        result
    }

    /// Write a temp copy named `filename` into this session's scratch dir.
    pub fn temp_copy(&mut self, filename: &str) -> Result<PathBuf, String> {
        if self.temp_dir.is_none() {
            let dir = tempfile::Builder::new()
                .prefix("crispsorter-pdf-")
                .tempdir()
                .map_err(|e| format!("tempdir: {e}"))?;
            // Detach from TempDir's own Drop; our Drop owns it now, so the
            // path stays valid for as long as the session does.
            self.temp_dir = Some(dir.keep());
        }
        let out = self.temp_dir.as_ref().unwrap().join(filename);
        self.save_temp_copy(&out)
    }

    /// Serialise to a byte buffer — for print, share, and rendering the
    /// edited document in the viewer without touching the filesystem.
    pub fn to_bytes(&mut self) -> Result<Vec<u8>, String> {
        let (doc, _) = self.materialise()?;
        let mut doc = doc.clone();
        let mut buf = Vec::new();
        doc.save_to(&mut buf).map_err(|e| format!("serialise: {e}"))?;
        Ok(buf)
    }

    pub fn state(&mut self, id: &str) -> Result<SessionState, String> {
        let can_undo = self.cursor > 0;
        let can_redo = self.cursor < self.ops.len();
        let undo_label = if can_undo { Some(self.ops[self.cursor - 1].label().to_string()) } else { None };
        let modified = self.modified();
        let op_labels: Vec<String> = self.ops[..self.cursor].iter().map(|o| o.label().to_string()).collect();
        let source = self.source.to_string_lossy().to_string();
        let (doc, origins) = self.materialise()?;
        let info = pdf_ops::pdf_info_from_doc(doc);
        Ok(SessionState {
            id: id.to_string(),
            source,
            info,
            origins: origins.clone(),
            can_undo,
            can_redo,
            undo_label,
            modified,
            ops: op_labels,
        })
    }
}

// ── Session store (Tauri managed state) ────────────────────────────────

#[derive(Default)]
pub struct PdfSessions(pub Mutex<HashMap<String, PdfEditSession>>);

pub mod tauri_commands {
    use super::*;
    use tauri::State;

    fn with_session<T>(
        sessions: &State<'_, PdfSessions>,
        id: &str,
        f: impl FnOnce(&mut PdfEditSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut map = sessions.0.lock().map_err(|_| "session store poisoned".to_string())?;
        let s = map.get_mut(id).ok_or_else(|| format!("no such PDF session: {id}"))?;
        f(s)
    }

    #[tauri::command]
    pub async fn pdf_session_open(
        sessions: State<'_, PdfSessions>,
        path: String,
    ) -> Result<SessionState, String> {
        let mut session = PdfEditSession::open(Path::new(&path))?;
        let id = uuid::Uuid::new_v4().to_string();
        let state = session.state(&id)?;
        sessions
            .0
            .lock()
            .map_err(|_| "session store poisoned".to_string())?
            .insert(id, session);
        Ok(state)
    }

    #[tauri::command]
    pub async fn pdf_session_apply(
        sessions: State<'_, PdfSessions>,
        id: String,
        op: EditOp,
    ) -> Result<SessionState, String> {
        with_session(&sessions, &id, |s| {
            s.apply(op)?;
            s.state(&id)
        })
    }

    #[tauri::command]
    pub async fn pdf_session_undo(
        sessions: State<'_, PdfSessions>,
        id: String,
    ) -> Result<SessionState, String> {
        with_session(&sessions, &id, |s| {
            s.undo()?;
            s.state(&id)
        })
    }

    #[tauri::command]
    pub async fn pdf_session_redo(
        sessions: State<'_, PdfSessions>,
        id: String,
    ) -> Result<SessionState, String> {
        with_session(&sessions, &id, |s| {
            s.redo()?;
            s.state(&id)
        })
    }

    #[tauri::command]
    pub async fn pdf_session_state(
        sessions: State<'_, PdfSessions>,
        id: String,
    ) -> Result<SessionState, String> {
        with_session(&sessions, &id, |s| s.state(&id))
    }

    #[tauri::command]
    pub async fn pdf_session_save(
        sessions: State<'_, PdfSessions>,
        id: String,
        out_path: Option<String>,
    ) -> Result<String, String> {
        with_session(&sessions, &id, |s| {
            let p = s.save(out_path.as_deref().map(Path::new))?;
            Ok(p.to_string_lossy().to_string())
        })
    }

    /// Serialise the edited document to a temp file and return its path.
    /// Used by print / share, which need a real file to hand to the OS.
    #[tauri::command]
    pub async fn pdf_session_export_temp(
        sessions: State<'_, PdfSessions>,
        id: String,
        filename: Option<String>,
    ) -> Result<String, String> {
        with_session(&sessions, &id, |s| {
            let stem = filename.unwrap_or_else(|| {
                s.source
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "document.pdf".into())
            });
            // Writing a temp copy is not persisting the user's document, so
            // the dirty flag has to survive it — otherwise printing, sharing
            // or extracting a selection would silently mark the document
            // saved (or unsaved) and the close guard would be wrong.
            let out = s.temp_copy(&stem)?;
            Ok(out.to_string_lossy().to_string())
        })
    }

    #[tauri::command]
    pub async fn pdf_session_close(
        sessions: State<'_, PdfSessions>,
        id: String,
    ) -> Result<(), String> {
        sessions
            .0
            .lock()
            .map_err(|_| "session store poisoned".to_string())?
            .remove(&id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal in-memory PDF with `n` blank pages.
    fn make_pdf(n: usize) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let kids: Vec<lopdf::Object> = (0..n)
            .map(|_| {
                let page = lopdf::Dictionary::from_iter(vec![
                    ("Type", lopdf::Object::Name(b"Page".to_vec())),
                    ("Parent", lopdf::Object::Reference(pages_id)),
                    ("MediaBox", lopdf::Object::Array(vec![
                        lopdf::Object::Integer(0), lopdf::Object::Integer(0),
                        lopdf::Object::Integer(612), lopdf::Object::Integer(792),
                    ])),
                ]);
                lopdf::Object::Reference(doc.add_object(lopdf::Object::Dictionary(page)))
            })
            .collect();
        let pages = lopdf::Dictionary::from_iter(vec![
            ("Type", lopdf::Object::Name(b"Pages".to_vec())),
            ("Count", lopdf::Object::Integer(n as i64)),
            ("Kids", lopdf::Object::Array(kids)),
        ]);
        doc.objects.insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog = lopdf::Dictionary::from_iter(vec![
            ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
            ("Pages", lopdf::Object::Reference(pages_id)),
        ]);
        let cat_id = doc.add_object(lopdf::Object::Dictionary(catalog));
        doc.trailer.set("Root", lopdf::Object::Reference(cat_id));
        doc
    }

    fn session_with(n: usize) -> PdfEditSession {
        let doc = make_pdf(n);
        let origins = (0..n).map(|i| PageOrigin::from_file("/test.pdf", i)).collect();
        PdfEditSession {
            source: PathBuf::from("/test.pdf"),
            base: doc,
            base_origins: origins,
            ops: Vec::new(),
            cursor: 0,
            cache: None,
            saved_at: 0,
            temp_dir: None,
        }
    }

    fn page_count(s: &mut PdfEditSession) -> usize {
        s.materialise().unwrap().0.page_iter().count()
    }

    fn origin_indices(s: &mut PdfEditSession) -> Vec<Option<usize>> {
        s.materialise().unwrap().1.iter().map(|o| o.index).collect()
    }

    #[test]
    fn reorder_permutes_pages_and_origins() {
        let mut s = session_with(3);
        s.apply(EditOp::Reorder { order: vec![2, 0, 1] }).unwrap();
        assert_eq!(page_count(&mut s), 3);
        assert_eq!(origin_indices(&mut s), vec![Some(2), Some(0), Some(1)]);
    }

    #[test]
    fn remove_drops_pages_and_origins_together() {
        let mut s = session_with(4);
        s.apply(EditOp::RemovePages { pages: vec![1, 2] }).unwrap();
        assert_eq!(page_count(&mut s), 2);
        assert_eq!(origin_indices(&mut s), vec![Some(0), Some(3)]);
    }

    #[test]
    fn undo_restores_previous_state() {
        let mut s = session_with(3);
        s.apply(EditOp::RemovePages { pages: vec![0] }).unwrap();
        assert_eq!(page_count(&mut s), 2);
        s.undo().unwrap();
        assert_eq!(page_count(&mut s), 3);
        assert_eq!(origin_indices(&mut s), vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn redo_reapplies_undone_op() {
        let mut s = session_with(3);
        s.apply(EditOp::RemovePages { pages: vec![0] }).unwrap();
        s.undo().unwrap();
        s.redo().unwrap();
        assert_eq!(page_count(&mut s), 2);
        assert_eq!(origin_indices(&mut s), vec![Some(1), Some(2)]);
    }

    #[test]
    fn applying_after_undo_discards_the_redo_tail() {
        let mut s = session_with(4);
        s.apply(EditOp::RemovePages { pages: vec![0] }).unwrap();
        s.apply(EditOp::RemovePages { pages: vec![0] }).unwrap();
        s.undo().unwrap();
        s.undo().unwrap();
        s.apply(EditOp::Reorder { order: vec![3, 2, 1, 0] }).unwrap();
        assert!(!s.state("x").unwrap().can_redo);
        assert_eq!(origin_indices(&mut s), vec![Some(3), Some(2), Some(1), Some(0)]);
    }

    #[test]
    fn rejected_op_leaves_session_untouched() {
        let mut s = session_with(2);
        let before = origin_indices(&mut s);
        // Page 9 does not exist.
        assert!(s.apply(EditOp::Rotate { pages: vec![9], degrees: 90 }).is_err());
        assert_eq!(origin_indices(&mut s), before);
        assert!(!s.state("x").unwrap().can_undo);
        assert!(!s.modified());
    }

    #[test]
    fn removing_every_page_is_rejected() {
        let mut s = session_with(2);
        assert!(s.apply(EditOp::RemovePages { pages: vec![0, 1] }).is_err());
        assert_eq!(page_count(&mut s), 2);
    }

    #[test]
    fn rotation_accumulates_and_normalises() {
        let mut s = session_with(1);
        s.apply(EditOp::Rotate { pages: vec![0], degrees: 270 }).unwrap();
        s.apply(EditOp::Rotate { pages: vec![0], degrees: 180 }).unwrap();
        let rot = s.materialise().unwrap().1[0].rotation;
        assert_eq!(rot, 90, "270 + 180 should normalise into 0..360");
    }

    #[test]
    fn insert_blank_adds_an_originless_page() {
        let mut s = session_with(2);
        s.apply(EditOp::InsertBlank { position: 1, width: 612.0, height: 792.0 }).unwrap();
        assert_eq!(page_count(&mut s), 3);
        assert_eq!(origin_indices(&mut s), vec![Some(0), None, Some(1)]);
    }

    #[test]
    fn temp_copies_share_one_directory_and_vanish_with_the_session() {
        let dir_path;
        {
            let mut s = session_with(2);
            let a = s.temp_copy("one.pdf").unwrap();
            let b = s.temp_copy("two.pdf").unwrap();
            assert_eq!(a.parent(), b.parent(), "repeated exports must reuse one scratch dir");
            assert!(a.exists() && b.exists());
            dir_path = a.parent().unwrap().to_path_buf();
        }
        assert!(!dir_path.exists(), "closing the session must reclaim its scratch dir");
    }

    #[test]
    fn temp_copy_does_not_clear_the_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = session_with(3);
        s.apply(EditOp::Reorder { order: vec![2, 1, 0] }).unwrap();
        assert!(s.modified());
        s.save_temp_copy(&dir.path().join("preview.pdf")).unwrap();
        assert!(s.modified(), "printing or sharing must not mark the document saved");
    }

    #[test]
    fn temp_copy_does_not_set_the_dirty_flag_on_a_clean_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = session_with(3);
        assert!(!s.modified());
        s.save_temp_copy(&dir.path().join("preview.pdf")).unwrap();
        assert!(!s.modified(), "an untouched document stays untouched");
    }

    #[test]
    fn modified_flag_tracks_saves() {
        let mut s = session_with(2);
        assert!(!s.modified());
        s.apply(EditOp::Reorder { order: vec![1, 0] }).unwrap();
        assert!(s.modified());
        s.saved_at = s.cursor; // stand-in for a successful save
        assert!(!s.modified());
        s.undo().unwrap();
        assert!(s.modified(), "undoing past the save point is a modification");
    }
}
