//! Annotations & highlights (P25.8 + P25.9).
//!
//! P25.8 — Per-document annotations: highlight, note, rectangle, stamp
//! on specific page regions.  Stored in SQLite, searchable via FTS.
//!
//! P25.9 — Reading queue: passage highlights across documents with
//! optional notes.  "Reading List" aggregates all highlights sorted
//! by recency.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
-- P25.8 — document annotations
CREATE TABLE IF NOT EXISTS annotations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id      TEXT    NOT NULL,
    page        INTEGER NOT NULL DEFAULT 0,
    x           REAL    NOT NULL DEFAULT 0,
    y           REAL    NOT NULL DEFAULT 0,
    w           REAL    NOT NULL DEFAULT 0,
    h           REAL    NOT NULL DEFAULT 0,
    ann_type    TEXT    NOT NULL DEFAULT 'note', -- highlight, note, rectangle, stamp
    text        TEXT    NOT NULL DEFAULT '',
    color       TEXT    NOT NULL DEFAULT '#facc15', -- hex color
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ann_doc ON annotations(doc_id);

-- P25.9 — reading highlights / queue
CREATE TABLE IF NOT EXISTS highlights (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id       TEXT    NOT NULL,
    chunk_index  INTEGER NOT NULL DEFAULT 0,
    start_offset INTEGER NOT NULL DEFAULT 0,
    end_offset   INTEGER NOT NULL DEFAULT 0,
    text         TEXT    NOT NULL DEFAULT '', -- the highlighted passage
    note         TEXT    NOT NULL DEFAULT '',
    color        TEXT    NOT NULL DEFAULT '#60a5fa',
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_hl_doc ON highlights(doc_id);
CREATE INDEX IF NOT EXISTS idx_hl_created ON highlights(created_at);
";

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub id: i64,
    pub doc_id: String,
    pub page: i32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub ann_type: String,
    pub text: String,
    pub color: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Highlight {
    pub id: i64,
    pub doc_id: String,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub text: String,
    pub note: String,
    pub color: String,
    pub created_at: i64,
}

// ── Store ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AnnotationStore {
    conn: Arc<Mutex<Connection>>,
}

impl AnnotationStore {
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("annotations.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening annotations DB {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("creating annotations schema")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    // ── Annotations (P25.8) ────────────────────────────────────────────

    pub fn add_annotation(
        &self,
        doc_id: &str, page: i32, x: f64, y: f64, w: f64, h: f64,
        ann_type: &str, text: &str, color: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO annotations (doc_id, page, x, y, w, h, ann_type, text, color, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![doc_id, page, x, y, w, h, ann_type, text, color, now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_annotations(&self, doc_id: &str) -> Result<Vec<Annotation>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, doc_id, page, x, y, w, h, ann_type, text, color, created_at FROM annotations WHERE doc_id = ?1 ORDER BY page, created_at"
        )?;
        let rows = stmt.query_map(params![doc_id], |row| {
            Ok(Annotation {
                id: row.get(0)?, doc_id: row.get(1)?, page: row.get(2)?,
                x: row.get(3)?, y: row.get(4)?, w: row.get(5)?, h: row.get(6)?,
                ann_type: row.get(7)?, text: row.get(8)?, color: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn update_annotation(&self, id: i64, text: &str, color: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE annotations SET text = ?1, color = ?2 WHERE id = ?3",
            params![text, color, id],
        )?;
        Ok(())
    }

    pub fn delete_annotation(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Search annotations by text content across all documents.
    pub fn search_annotations(&self, query: &str, limit: usize) -> Result<Vec<Annotation>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", query.replace('%', "\\%"));
        let mut stmt = conn.prepare(
            "SELECT id, doc_id, page, x, y, w, h, ann_type, text, color, created_at FROM annotations WHERE text LIKE ?1 ESCAPE '\\' ORDER BY created_at DESC LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(Annotation {
                id: row.get(0)?, doc_id: row.get(1)?, page: row.get(2)?,
                x: row.get(3)?, y: row.get(4)?, w: row.get(5)?, h: row.get(6)?,
                ann_type: row.get(7)?, text: row.get(8)?, color: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    // ── Highlights / Reading Queue (P25.9) ─────────────────────────────

    pub fn add_highlight(
        &self,
        doc_id: &str, chunk_index: i32,
        start_offset: i32, end_offset: i32,
        text: &str, note: &str, color: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO highlights (doc_id, chunk_index, start_offset, end_offset, text, note, color, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![doc_id, chunk_index, start_offset, end_offset, text, note, color, now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_highlights(&self, doc_id: &str) -> Result<Vec<Highlight>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, doc_id, chunk_index, start_offset, end_offset, text, note, color, created_at FROM highlights WHERE doc_id = ?1 ORDER BY chunk_index, start_offset"
        )?;
        let rows = stmt.query_map(params![doc_id], |row| {
            Ok(Highlight {
                id: row.get(0)?, doc_id: row.get(1)?, chunk_index: row.get(2)?,
                start_offset: row.get(3)?, end_offset: row.get(4)?,
                text: row.get(5)?, note: row.get(6)?, color: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Reading list: all highlights across all documents, newest first.
    pub fn reading_list(&self, limit: usize, offset: usize) -> Result<Vec<Highlight>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, doc_id, chunk_index, start_offset, end_offset, text, note, color, created_at FROM highlights ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
            Ok(Highlight {
                id: row.get(0)?, doc_id: row.get(1)?, chunk_index: row.get(2)?,
                start_offset: row.get(3)?, end_offset: row.get(4)?,
                text: row.get(5)?, note: row.get(6)?, color: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn update_highlight(&self, id: i64, note: &str, color: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE highlights SET note = ?1, color = ?2 WHERE id = ?3",
            params![note, color, id],
        )?;
        Ok(())
    }

    pub fn delete_highlight(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM highlights WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn highlight_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM highlights", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tauri commands ─────────────────────────────────────────────────────

pub mod tauri_commands {
    use super::*;
    use tauri::State;
    use crate::AppState;

    async fn get_store(state: &State<'_, AppState>) -> Result<AnnotationStore, String> {
        let data_dir = state.data_dir.lock().await;
        let dir = data_dir.as_ref().ok_or("App data dir not set")?;
        AnnotationStore::open_or_create(dir).map_err(|e| e.to_string())
    }

    // Annotations
    #[tauri::command]
    pub async fn annotation_add(
        state: State<'_, AppState>,
        doc_id: String, page: i32, x: f64, y: f64, w: f64, h: f64,
        ann_type: String, text: String, color: String,
    ) -> Result<i64, String> {
        let s = get_store(&state).await?;
        s.add_annotation(&doc_id, page, x, y, w, h, &ann_type, &text, &color).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn annotation_list(state: State<'_, AppState>, doc_id: String) -> Result<Vec<Annotation>, String> {
        let s = get_store(&state).await?;
        s.get_annotations(&doc_id).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn annotation_update(state: State<'_, AppState>, id: i64, text: String, color: String) -> Result<(), String> {
        let s = get_store(&state).await?;
        s.update_annotation(id, &text, &color).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn annotation_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
        let s = get_store(&state).await?;
        s.delete_annotation(id).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn annotation_search(state: State<'_, AppState>, query: String, limit: Option<usize>) -> Result<Vec<Annotation>, String> {
        let s = get_store(&state).await?;
        s.search_annotations(&query, limit.unwrap_or(50)).map_err(|e| e.to_string())
    }

    // Highlights / Reading Queue
    #[tauri::command]
    pub async fn highlight_add(
        state: State<'_, AppState>,
        doc_id: String, chunk_index: i32,
        start_offset: i32, end_offset: i32,
        text: String, note: String, color: String,
    ) -> Result<i64, String> {
        let s = get_store(&state).await?;
        s.add_highlight(&doc_id, chunk_index, start_offset, end_offset, &text, &note, &color).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn highlight_list(state: State<'_, AppState>, doc_id: String) -> Result<Vec<Highlight>, String> {
        let s = get_store(&state).await?;
        s.get_highlights(&doc_id).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn highlight_reading_list(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> Result<Vec<Highlight>, String> {
        let s = get_store(&state).await?;
        s.reading_list(limit.unwrap_or(50), offset.unwrap_or(0)).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn highlight_update(state: State<'_, AppState>, id: i64, note: String, color: String) -> Result<(), String> {
        let s = get_store(&state).await?;
        s.update_highlight(id, &note, &color).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn highlight_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
        let s = get_store(&state).await?;
        s.delete_highlight(id).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn highlight_count(state: State<'_, AppState>) -> Result<usize, String> {
        let s = get_store(&state).await?;
        s.highlight_count().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn annotation_crud() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();

        let id = store.add_annotation("doc1", 1, 10.0, 20.0, 100.0, 50.0, "note", "Important!", "#facc15").unwrap();
        let anns = store.get_annotations("doc1").unwrap();
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].text, "Important!");

        store.update_annotation(id, "Updated note", "#ef4444").unwrap();
        let anns = store.get_annotations("doc1").unwrap();
        assert_eq!(anns[0].text, "Updated note");
        assert_eq!(anns[0].color, "#ef4444");

        store.delete_annotation(id).unwrap();
        assert_eq!(store.get_annotations("doc1").unwrap().len(), 0);
    }

    #[test]
    fn annotation_search() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        store.add_annotation("d1", 0, 0.0, 0.0, 0.0, 0.0, "note", "contract clause 7", "#fff").unwrap();
        store.add_annotation("d2", 0, 0.0, 0.0, 0.0, 0.0, "note", "deadline info", "#fff").unwrap();

        let found = store.search_annotations("contract", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].doc_id, "d1");
    }

    #[test]
    fn highlight_reading_list() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();

        store.add_highlight("d1", 0, 10, 50, "passage one", "interesting", "#60a5fa").unwrap();
        store.add_highlight("d2", 1, 0, 30, "passage two", "", "#60a5fa").unwrap();

        let list = store.reading_list(10, 0).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].doc_id, "d2"); // most recent first

        assert_eq!(store.highlight_count().unwrap(), 2);

        let doc1 = store.get_highlights("d1").unwrap();
        assert_eq!(doc1.len(), 1);
        assert_eq!(doc1[0].text, "passage one");
    }

    #[test]
    fn highlight_update_and_delete() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        let id = store.add_highlight("d1", 0, 0, 10, "text", "note1", "#fff").unwrap();
        store.update_highlight(id, "note2", "#f00").unwrap();
        let hl = store.get_highlights("d1").unwrap();
        assert_eq!(hl[0].note, "note2");
        assert_eq!(hl[0].color, "#f00");

        store.delete_highlight(id).unwrap();
        assert_eq!(store.highlight_count().unwrap(), 0);
    }

    #[test]
    fn annotation_multiple_pages() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        store.add_annotation("d1", 1, 0.0, 0.0, 10.0, 10.0, "note", "page1 note", "#fff").unwrap();
        store.add_annotation("d1", 2, 0.0, 0.0, 10.0, 10.0, "highlight", "page2 hl", "#ff0").unwrap();
        store.add_annotation("d1", 1, 50.0, 50.0, 10.0, 10.0, "note", "page1 note2", "#fff").unwrap();
        let anns = store.get_annotations("d1").unwrap();
        assert_eq!(anns.len(), 3);
        // Ordered by page, then created_at
        assert_eq!(anns[0].page, 1);
        assert_eq!(anns[2].page, 2);
    }

    #[test]
    fn annotation_empty_doc() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        let anns = store.get_annotations("nonexistent").unwrap();
        assert!(anns.is_empty());
    }

    #[test]
    fn annotation_search_special_chars() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        store.add_annotation("d1", 0, 0.0, 0.0, 0.0, 0.0, "note", "100% complete", "#fff").unwrap();
        // The % should be escaped in LIKE
        let found = store.search_annotations("100%", 10).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn reading_list_pagination() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        for i in 0..5 {
            store.add_highlight("d1", i, 0, 10, &format!("hl{i}"), "", "#fff").unwrap();
        }
        let page1 = store.reading_list(2, 0).unwrap();
        let page2 = store.reading_list(2, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        assert_ne!(page1[0].id, page2[0].id);
    }

    #[test]
    fn update_nonexistent_annotation_succeeds() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        // SQLite UPDATE on missing row is a no-op, not an error
        let result = store.update_annotation(99999, "updated text", "#000");
        assert!(result.is_ok());
    }

    #[test]
    fn delete_nonexistent_no_error() {
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        assert!(store.delete_annotation(99999).is_ok());
        assert!(store.delete_highlight(99999).is_ok());
    }

    #[test]
    fn highlight_end_before_start() {
        // Schema doesn't enforce offset ordering — verify it stores and retrieves
        let dir = TempDir::new().unwrap();
        let store = AnnotationStore::open_or_create(dir.path()).unwrap();
        let id = store.add_highlight("d1", 0, 100, 50, "reversed", "", "#fff").unwrap();
        let hls = store.get_highlights("d1").unwrap();
        assert_eq!(hls.len(), 1);
        assert_eq!(hls[0].id, id);
        assert_eq!(hls[0].start_offset, 100);
        assert_eq!(hls[0].end_offset, 50);
    }
}
