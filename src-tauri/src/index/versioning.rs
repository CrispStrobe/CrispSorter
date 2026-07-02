//! Document versioning (P25.1).
//!
//! Tracks changes to the same file over time.  Each document's
//! canonical path is hashed (SHA-256) to produce a `version_group_id`.
//! On re-ingest of the same path, the `version_seq` counter increments.
//! The version history can be queried per document to show all prior
//! versions with their timestamps and metadata diffs.
//!
//! Stored in a WAL-mode SQLite database (`versions.db`) alongside
//! the main LanceDB index.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS doc_versions (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    version_group_id TEXT    NOT NULL,  -- SHA-256 of canonical path
    version_seq      INTEGER NOT NULL,  -- 1, 2, 3, ...
    doc_id           TEXT    NOT NULL,  -- references LanceDB doc_id
    canonical_path   TEXT    NOT NULL,  -- the original file path
    indexed_at       INTEGER NOT NULL,  -- epoch millis
    title            TEXT,
    author           TEXT,
    file_size        INTEGER,
    file_hash        TEXT,              -- content SHA-256 (if computed)
    UNIQUE(version_group_id, version_seq)
);
CREATE INDEX IF NOT EXISTS idx_ver_group ON doc_versions(version_group_id);
CREATE INDEX IF NOT EXISTS idx_ver_docid ON doc_versions(doc_id);
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub id: i64,
    pub version_group_id: String,
    pub version_seq: i32,
    pub doc_id: String,
    pub canonical_path: String,
    pub indexed_at: i64,
    pub title: Option<String>,
    pub author: Option<String>,
    pub file_size: Option<i64>,
    pub file_hash: Option<String>,
}

#[derive(Clone)]
pub struct VersionStore {
    conn: Arc<Mutex<Connection>>,
}

impl VersionStore {
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("versions.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening versions DB {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("creating versions schema")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Compute the version group ID for a file path.
    pub fn group_id(canonical_path: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(canonical_path.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Record a new version of a document.  Returns the assigned version_seq.
    pub fn record_version(
        &self,
        canonical_path: &str,
        doc_id: &str,
        title: Option<&str>,
        author: Option<&str>,
        file_size: Option<i64>,
        file_hash: Option<&str>,
    ) -> Result<i32> {
        let group_id = Self::group_id(canonical_path);
        let conn = self.conn.lock().unwrap();
        let next_seq: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version_seq), 0) + 1 FROM doc_versions WHERE version_group_id = ?1",
                params![group_id],
                |row| row.get(0),
            )
            .unwrap_or(1);
        let now = now_ms();
        conn.execute(
            "INSERT INTO doc_versions (version_group_id, version_seq, doc_id, canonical_path, indexed_at, title, author, file_size, file_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![group_id, next_seq, doc_id, canonical_path, now, title, author, file_size, file_hash],
        )?;
        Ok(next_seq)
    }

    /// Get the version history for a document (by doc_id or path).
    pub fn get_versions(&self, doc_id: Option<&str>, path: Option<&str>) -> Result<Vec<VersionEntry>> {
        let conn = self.conn.lock().unwrap();
        let (sql, param): (&str, String) = if let Some(did) = doc_id {
            // Find the group_id for this doc_id, then return all versions in that group
            let gid: Option<String> = conn.query_row(
                "SELECT version_group_id FROM doc_versions WHERE doc_id = ?1 LIMIT 1",
                params![did],
                |row| row.get(0),
            ).ok();
            match gid {
                Some(g) => ("SELECT * FROM doc_versions WHERE version_group_id = ?1 ORDER BY version_seq DESC", g),
                None => return Ok(vec![]),
            }
        } else if let Some(p) = path {
            let gid = Self::group_id(p);
            ("SELECT * FROM doc_versions WHERE version_group_id = ?1 ORDER BY version_seq DESC", gid)
        } else {
            return Ok(vec![]);
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![param], |row| {
            Ok(VersionEntry {
                id: row.get(0)?,
                version_group_id: row.get(1)?,
                version_seq: row.get(2)?,
                doc_id: row.get(3)?,
                canonical_path: row.get(4)?,
                indexed_at: row.get(5)?,
                title: row.get(6)?,
                author: row.get(7)?,
                file_size: row.get(8)?,
                file_hash: row.get(9)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Get the current (latest) version seq for a path.
    pub fn current_version(&self, canonical_path: &str) -> Result<Option<i32>> {
        let group_id = Self::group_id(canonical_path);
        let conn = self.conn.lock().unwrap();
        let seq: Option<i32> = conn
            .query_row(
                "SELECT MAX(version_seq) FROM doc_versions WHERE version_group_id = ?1",
                params![group_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(seq)
    }

    /// Total versioned documents (distinct groups).
    pub fn count_groups(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT version_group_id) FROM doc_versions",
            [],
            |row| row.get(0),
        )?;
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

    async fn get_store(state: &State<'_, AppState>) -> Result<VersionStore, String> {
        let data_dir = state.data_dir.lock().await;
        let dir = data_dir.as_ref().ok_or("App data dir not set")?;
        VersionStore::open_or_create(dir).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn version_record(
        state: State<'_, AppState>,
        canonical_path: String,
        doc_id: String,
        title: Option<String>,
        author: Option<String>,
        file_size: Option<i64>,
        file_hash: Option<String>,
    ) -> Result<i32, String> {
        let store = get_store(&state).await?;
        store.record_version(
            &canonical_path, &doc_id,
            title.as_deref(), author.as_deref(),
            file_size, file_hash.as_deref(),
        ).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn version_history(
        state: State<'_, AppState>,
        doc_id: Option<String>,
        path: Option<String>,
    ) -> Result<Vec<VersionEntry>, String> {
        let store = get_store(&state).await?;
        store.get_versions(doc_id.as_deref(), path.as_deref())
            .map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn version_current(
        state: State<'_, AppState>,
        canonical_path: String,
    ) -> Result<Option<i32>, String> {
        let store = get_store(&state).await?;
        store.current_version(&canonical_path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn version_lifecycle() {
        let dir = TempDir::new().unwrap();
        let store = VersionStore::open_or_create(dir.path()).unwrap();

        let seq1 = store.record_version("/doc/report.pdf", "d1", Some("Report v1"), None, Some(1024), None).unwrap();
        assert_eq!(seq1, 1);

        let seq2 = store.record_version("/doc/report.pdf", "d2", Some("Report v2"), None, Some(2048), None).unwrap();
        assert_eq!(seq2, 2);

        let versions = store.get_versions(Some("d1"), None).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version_seq, 2); // most recent first

        let cur = store.current_version("/doc/report.pdf").unwrap();
        assert_eq!(cur, Some(2));

        assert_eq!(store.count_groups().unwrap(), 1);
    }

    #[test]
    fn separate_groups() {
        let dir = TempDir::new().unwrap();
        let store = VersionStore::open_or_create(dir.path()).unwrap();
        store.record_version("/a.pdf", "a1", None, None, None, None).unwrap();
        store.record_version("/b.pdf", "b1", None, None, None, None).unwrap();
        assert_eq!(store.count_groups().unwrap(), 2);
    }
}
