/// SQLite-backed batch session store.
///
/// Replaces the single-JSON-blob persistence model (`saveSetting('lastSession',…)`)
/// with per-row writes so that:
/// - One `upsertItem` call touches exactly one row, not the whole file.
/// - Concurrent workers never race on a single write; SQLite WAL + one
///   connection-per-process serialises writes cheaply.
/// - Full `extractedText` is stored in a separate `extracted_texts` table so
///   the hot path (item list, status updates) stays small.
///
/// DB path: `<data_dir>/batch_session.db`.
/// WAL + synchronous=NORMAL — same proven pattern as `jobs/mod.rs`.

pub mod tauri_commands;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Magic session id — we keep a single "current" session for now.
pub const SESSION_ID: &str = "current";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS batch_sessions (
    id          TEXT    PRIMARY KEY,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS batch_items (
    id                        TEXT    PRIMARY KEY,
    session_id                TEXT    NOT NULL REFERENCES batch_sessions(id) ON DELETE CASCADE,
    original_path             TEXT    NOT NULL,
    original_name             TEXT    NOT NULL,
    size_bytes                INTEGER NOT NULL,
    extension                 TEXT    NOT NULL,
    modified_at               INTEGER NOT NULL,
    status                    TEXT    NOT NULL,
    status_detail             TEXT,
    error_message             TEXT,
    is_accepted               INTEGER NOT NULL DEFAULT 0,
    is_ignored                INTEGER,
    suggested_title           TEXT,
    suggested_author          TEXT,
    suggested_year            TEXT,
    target_path               TEXT,
    detected_language         TEXT,
    audio_duration_seconds    REAL,
    audio_codec               TEXT,
    audio_channels            INTEGER,
    audio_sample_rate_hz      INTEGER,
    audio_bitrate_kbps        REAL,
    is_duplicate_primary      INTEGER,
    duplicate_group_id        TEXT,
    chapter_group_id          TEXT,
    chapter_suffix            TEXT,
    is_chapter_representative INTEGER,
    chapter_group_size        INTEGER,
    chapter_is_edited_volume  INTEGER,
    extracted_text_preview    TEXT,
    metadata_read_status      TEXT
);

CREATE INDEX IF NOT EXISTS batch_items_session_idx ON batch_items(session_id);
CREATE INDEX IF NOT EXISTS batch_items_status_idx  ON batch_items(session_id, status);

CREATE TABLE IF NOT EXISTS extracted_texts (
    item_id      TEXT    PRIMARY KEY REFERENCES batch_items(id) ON DELETE CASCADE,
    text         TEXT    NOT NULL,
    extracted_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS processed_history (
    sha256           TEXT    PRIMARY KEY,
    filename         TEXT    NOT NULL,
    size_bytes       INTEGER NOT NULL,
    suggested_title  TEXT,
    suggested_author TEXT,
    suggested_year   TEXT,
    target_path      TEXT,
    processed_at     INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS processed_history_filename_idx ON processed_history(filename);
"#;

// ── Data types ────────────────────────────────────────────────────────────────

/// One batch item as exchanged with the frontend.
///
/// Field names are snake_case here; `rename_all = "camelCase"` makes Tauri
/// serialize them to the camelCase names the TypeScript `BatchItem` expects.
/// The one mismatch (`size_bytes` → TS `size`) is handled by an explicit
/// `rename` attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchItemRow {
    pub id: String,
    pub original_path: String,
    pub original_name: String,
    pub extension: String,
    /// TS BatchItem calls this `size` (not `sizeBytes`).
    #[serde(rename = "size")]
    pub size_bytes: i64,
    pub modified_at: i64,
    pub status: String,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub status_detail: Option<String>,
    #[serde(default)]
    pub detected_language: Option<String>,
    #[serde(default)]
    pub audio_duration_seconds: Option<f64>,
    #[serde(default)]
    pub audio_codec: Option<String>,
    #[serde(default)]
    pub audio_sample_rate_hz: Option<i64>,
    #[serde(default)]
    pub audio_channels: Option<i64>,
    #[serde(default)]
    pub audio_bitrate_kbps: Option<f64>,
    #[serde(default)]
    pub metadata_read_status: Option<String>,
    #[serde(default)]
    pub suggested_title: Option<String>,
    #[serde(default)]
    pub suggested_author: Option<String>,
    #[serde(default)]
    pub suggested_year: Option<String>,
    #[serde(default)]
    pub target_path: Option<String>,
    pub is_accepted: bool,
    #[serde(default)]
    pub is_ignored: Option<bool>,
    #[serde(default)]
    pub duplicate_group_id: Option<String>,
    #[serde(default)]
    pub is_duplicate_primary: Option<bool>,
    #[serde(default)]
    pub chapter_group_id: Option<String>,
    #[serde(default)]
    pub chapter_suffix: Option<String>,
    #[serde(default)]
    pub is_chapter_representative: Option<bool>,
    #[serde(default)]
    pub chapter_group_size: Option<i64>,
    #[serde(default)]
    pub chapter_is_edited_volume: Option<bool>,
    /// Bounded ≤500 char preview stored in the items row for cheap loads.
    /// Full text lives in `extracted_texts`; call `get_extracted_text` to hydrate.
    #[serde(default)]
    pub extracted_text_preview: Option<String>,
}

/// One row in the `processed_history` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedHistoryRow {
    pub sha256: String,
    pub filename: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: i64,
    #[serde(default)]
    pub suggested_title: Option<String>,
    #[serde(default)]
    pub suggested_author: Option<String>,
    #[serde(default)]
    pub suggested_year: Option<String>,
    #[serde(default)]
    pub target_path: Option<String>,
    pub processed_at: i64,
}

// ── BatchSessionStore ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct BatchSessionStore {
    conn: Arc<Mutex<Connection>>,
}

impl BatchSessionStore {
    /// Open (or create) the batch session database at `<data_dir>/batch_session.db`.
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("batch_session.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening batch session DB {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enabling WAL mode")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("setting synchronous mode")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("enabling foreign keys")?;
        conn.execute_batch(SCHEMA).context("creating batch session schema")?;
        // Ensure the single 'current' session row exists.
        let now = now_ms();
        conn.execute(
            "INSERT OR IGNORE INTO batch_sessions (id, created_at, updated_at) VALUES (?1, ?2, ?2)",
            params![SESSION_ID, now],
        )
        .context("ensuring current session row")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Load all items for the current session, ordered by insertion order.
    pub fn load(&self) -> Result<Vec<BatchItemRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(LOAD_SQL).context("preparing load")?;
        let rows = stmt
            .query_map(params![SESSION_ID], row_to_item)
            .context("querying batch items")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collecting batch items")?;
        Ok(rows)
    }

    /// Upsert a single item (insert or update all mutable fields).
    pub fn upsert_item(&self, item: &BatchItemRow) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        exec_upsert(&conn, item)
    }

    /// Upsert many items in a single transaction.
    pub fn upsert_items_bulk(&self, items: &[BatchItemRow]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting bulk upsert tx")?;
        for item in items {
            exec_upsert(&tx, item)?;
        }
        tx.commit().context("committing bulk upsert")?;
        Ok(())
    }

    /// Delete items by their ids.
    pub fn delete_items(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM batch_items WHERE session_id = '{SESSION_ID}' AND id IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
            .context("deleting items")?;
        Ok(())
    }

    /// Delete all items for the current session.
    pub fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM batch_items WHERE session_id = ?1",
            params![SESSION_ID],
        )
        .context("clearing batch items")?;
        Ok(())
    }

    /// Persist the full extracted text for an item.
    pub fn set_extracted_text(&self, item_id: &str, text: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO extracted_texts (item_id, text, extracted_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(item_id) DO UPDATE SET text = excluded.text, extracted_at = excluded.extracted_at",
            params![item_id, text, now],
        )
        .context("upserting extracted text")?;
        Ok(())
    }

    /// Retrieve the full extracted text for an item, or `None` if not stored.
    pub fn get_extracted_text(&self, item_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT text FROM extracted_texts WHERE item_id = ?1",
                params![item_id],
                |r| r.get(0),
            )
            .optional()
            .context("reading extracted text")?;
        Ok(result)
    }

    /// Return `true` if the one-shot JSON migration has already been applied.
    /// Uses a sentinel row (`id = 'json_migration_done'`) in `batch_sessions`.
    pub fn is_migrated(&self) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM batch_sessions WHERE id = 'json_migration_done'",
                [],
                |r| r.get(0),
            )
            .context("checking migration sentinel")?;
        Ok(count > 0)
    }

    /// Record that the one-shot JSON migration is done.
    /// Idempotent — safe to call multiple times.
    pub fn mark_migrated(&self) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO batch_sessions (id, created_at, updated_at) VALUES ('json_migration_done', ?1, ?1)",
            params![now],
        )
        .context("inserting migration sentinel")?;
        Ok(())
    }

    /// Record a successfully-processed item in the persistent history table.
    /// Idempotent — repeated calls for the same sha256 update the record.
    pub fn record_processed(&self, row: &ProcessedHistoryRow) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO processed_history (sha256, filename, size_bytes, suggested_title, suggested_author, suggested_year, target_path, processed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(sha256) DO UPDATE SET
               filename         = excluded.filename,
               size_bytes       = excluded.size_bytes,
               suggested_title  = excluded.suggested_title,
               suggested_author = excluded.suggested_author,
               suggested_year   = excluded.suggested_year,
               target_path      = excluded.target_path,
               processed_at     = excluded.processed_at",
            params![
                row.sha256,
                row.filename,
                row.size_bytes,
                row.suggested_title,
                row.suggested_author,
                row.suggested_year,
                row.target_path,
                now,
            ],
        )
        .context("recording processed history")?;
        Ok(())
    }

    /// Look up a previously-processed item by its SHA-256.
    /// Returns `None` when the hash has never been seen.
    pub fn lookup_history_by_sha256(&self, sha256: &str) -> Result<Option<ProcessedHistoryRow>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT sha256, filename, size_bytes, suggested_title, suggested_author, suggested_year, target_path, processed_at
                 FROM processed_history WHERE sha256 = ?1",
                params![sha256],
                |r| {
                    Ok(ProcessedHistoryRow {
                        sha256: r.get(0)?,
                        filename: r.get(1)?,
                        size_bytes: r.get(2)?,
                        suggested_title: r.get(3)?,
                        suggested_author: r.get(4)?,
                        suggested_year: r.get(5)?,
                        target_path: r.get(6)?,
                        processed_at: r.get(7)?,
                    })
                },
            )
            .optional()
            .context("looking up processed history")?;
        Ok(result)
    }

    /// Total number of distinct files in the processed history.
    pub fn processed_history_count(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM processed_history", [], |r| r.get(0))
            .context("counting processed history")?;
        Ok(count)
    }
}

// ── SQL helpers ───────────────────────────────────────────────────────────────

const LOAD_SQL: &str = r#"
SELECT
    id, original_path, original_name, size_bytes, extension, modified_at,
    status, status_detail, error_message,
    is_accepted, is_ignored,
    suggested_title, suggested_author, suggested_year, target_path,
    detected_language, audio_duration_seconds, audio_codec,
    audio_channels, audio_sample_rate_hz, audio_bitrate_kbps,
    is_duplicate_primary, duplicate_group_id,
    chapter_group_id, chapter_suffix, is_chapter_representative,
    chapter_group_size, chapter_is_edited_volume,
    extracted_text_preview, metadata_read_status
FROM batch_items
WHERE session_id = ?1
ORDER BY rowid ASC
"#;

const UPSERT_SQL: &str = r#"
INSERT INTO batch_items (
    id, session_id,
    original_path, original_name, size_bytes, extension, modified_at,
    status, status_detail, error_message,
    is_accepted, is_ignored,
    suggested_title, suggested_author, suggested_year, target_path,
    detected_language, audio_duration_seconds, audio_codec,
    audio_channels, audio_sample_rate_hz, audio_bitrate_kbps,
    is_duplicate_primary, duplicate_group_id,
    chapter_group_id, chapter_suffix, is_chapter_representative,
    chapter_group_size, chapter_is_edited_volume,
    extracted_text_preview, metadata_read_status
) VALUES (
    ?1, 'current',
    ?2, ?3, ?4, ?5, ?6,
    ?7, ?8, ?9,
    ?10, ?11,
    ?12, ?13, ?14, ?15,
    ?16, ?17, ?18,
    ?19, ?20, ?21,
    ?22, ?23,
    ?24, ?25, ?26,
    ?27, ?28,
    ?29, ?30
)
ON CONFLICT(id) DO UPDATE SET
    original_path             = excluded.original_path,
    original_name             = excluded.original_name,
    size_bytes                = excluded.size_bytes,
    extension                 = excluded.extension,
    modified_at               = excluded.modified_at,
    status                    = excluded.status,
    status_detail             = excluded.status_detail,
    error_message             = excluded.error_message,
    is_accepted               = excluded.is_accepted,
    is_ignored                = excluded.is_ignored,
    suggested_title           = excluded.suggested_title,
    suggested_author          = excluded.suggested_author,
    suggested_year            = excluded.suggested_year,
    target_path               = excluded.target_path,
    detected_language         = excluded.detected_language,
    audio_duration_seconds    = excluded.audio_duration_seconds,
    audio_codec               = excluded.audio_codec,
    audio_channels            = excluded.audio_channels,
    audio_sample_rate_hz      = excluded.audio_sample_rate_hz,
    audio_bitrate_kbps        = excluded.audio_bitrate_kbps,
    is_duplicate_primary      = excluded.is_duplicate_primary,
    duplicate_group_id        = excluded.duplicate_group_id,
    chapter_group_id          = excluded.chapter_group_id,
    chapter_suffix            = excluded.chapter_suffix,
    is_chapter_representative = excluded.is_chapter_representative,
    chapter_group_size        = excluded.chapter_group_size,
    chapter_is_edited_volume  = excluded.chapter_is_edited_volume,
    extracted_text_preview    = excluded.extracted_text_preview,
    metadata_read_status      = excluded.metadata_read_status
"#;

fn exec_upsert(conn: &Connection, item: &BatchItemRow) -> Result<()> {
    conn.execute(
        UPSERT_SQL,
        params![
            item.id,
            item.original_path,
            item.original_name,
            item.size_bytes,
            item.extension,
            item.modified_at,
            item.status,
            item.status_detail,
            item.error_message,
            item.is_accepted as i64,
            item.is_ignored.map(|b| b as i64),
            item.suggested_title,
            item.suggested_author,
            item.suggested_year,
            item.target_path,
            item.detected_language,
            item.audio_duration_seconds,
            item.audio_codec,
            item.audio_channels,
            item.audio_sample_rate_hz,
            item.audio_bitrate_kbps,
            item.is_duplicate_primary.map(|b| b as i64),
            item.duplicate_group_id,
            item.chapter_group_id,
            item.chapter_suffix,
            item.is_chapter_representative.map(|b| b as i64),
            item.chapter_group_size,
            item.chapter_is_edited_volume.map(|b| b as i64),
            item.extracted_text_preview,
            item.metadata_read_status,
        ],
    )
    .context("upserting batch item")?;
    Ok(())
}

fn row_to_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<BatchItemRow> {
    Ok(BatchItemRow {
        id: r.get(0)?,
        original_path: r.get(1)?,
        original_name: r.get(2)?,
        size_bytes: r.get(3)?,
        extension: r.get(4)?,
        modified_at: r.get(5)?,
        status: r.get(6)?,
        status_detail: r.get(7)?,
        error_message: r.get(8)?,
        is_accepted: r.get::<_, i64>(9).map(|v| v != 0)?,
        is_ignored: r.get::<_, Option<i64>>(10)?.map(|v| v != 0),
        suggested_title: r.get(11)?,
        suggested_author: r.get(12)?,
        suggested_year: r.get(13)?,
        target_path: r.get(14)?,
        detected_language: r.get(15)?,
        audio_duration_seconds: r.get(16)?,
        audio_codec: r.get(17)?,
        audio_channels: r.get(18)?,
        audio_sample_rate_hz: r.get(19)?,
        audio_bitrate_kbps: r.get(20)?,
        is_duplicate_primary: r.get::<_, Option<i64>>(21)?.map(|v| v != 0),
        duplicate_group_id: r.get(22)?,
        chapter_group_id: r.get(23)?,
        chapter_suffix: r.get(24)?,
        is_chapter_representative: r.get::<_, Option<i64>>(25)?.map(|v| v != 0),
        chapter_group_size: r.get(26)?,
        chapter_is_edited_volume: r.get::<_, Option<i64>>(27)?.map(|v| v != 0),
        extracted_text_preview: r.get(28)?,
        metadata_read_status: r.get(29)?,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> (BatchSessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = BatchSessionStore::open_or_create(dir.path()).unwrap();
        (s, dir)
    }

    fn sample_item(id: &str) -> BatchItemRow {
        BatchItemRow {
            id: id.to_owned(),
            original_path: format!("/docs/{id}.pdf"),
            original_name: format!("{id}.pdf"),
            extension: "pdf".to_owned(),
            size_bytes: 12345,
            modified_at: 1_700_000_000_000,
            status: "queued".to_owned(),
            error_message: None,
            status_detail: None,
            detected_language: None,
            audio_duration_seconds: None,
            audio_codec: None,
            audio_sample_rate_hz: None,
            audio_channels: None,
            audio_bitrate_kbps: None,
            metadata_read_status: None,
            suggested_title: None,
            suggested_author: None,
            suggested_year: None,
            target_path: None,
            is_accepted: false,
            is_ignored: None,
            duplicate_group_id: None,
            is_duplicate_primary: None,
            chapter_group_id: None,
            chapter_suffix: None,
            is_chapter_representative: None,
            chapter_group_size: None,
            chapter_is_edited_volume: None,
            extracted_text_preview: None,
        }
    }

    #[test]
    fn upsert_and_load_roundtrip() {
        let (store, _dir) = make_store();
        let item = sample_item("abc-001");
        store.upsert_item(&item).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "abc-001");
        assert_eq!(loaded[0].size_bytes, 12345);
        assert!(!loaded[0].is_accepted);
    }

    #[test]
    fn upsert_updates_existing() {
        let (store, _dir) = make_store();
        let mut item = sample_item("abc-002");
        store.upsert_item(&item).unwrap();
        item.status = "review".to_owned();
        item.suggested_title = Some("Kant, Critique".to_owned());
        item.is_accepted = true;
        store.upsert_item(&item).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status, "review");
        assert_eq!(loaded[0].suggested_title.as_deref(), Some("Kant, Critique"));
        assert!(loaded[0].is_accepted);
    }

    #[test]
    fn bulk_upsert_100_items() {
        let (store, _dir) = make_store();
        let items: Vec<BatchItemRow> = (0..100).map(|i| sample_item(&format!("item-{i:03}"))).collect();
        store.upsert_items_bulk(&items).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 100);
    }

    #[test]
    fn delete_items() {
        let (store, _dir) = make_store();
        let items: Vec<BatchItemRow> = (0..5).map(|i| sample_item(&format!("del-{i}"))).collect();
        store.upsert_items_bulk(&items).unwrap();
        store.delete_items(&["del-1".to_owned(), "del-3".to_owned()]).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(!loaded.iter().any(|i| i.id == "del-1" || i.id == "del-3"));
    }

    #[test]
    fn clear_removes_all() {
        let (store, _dir) = make_store();
        store.upsert_item(&sample_item("x1")).unwrap();
        store.upsert_item(&sample_item("x2")).unwrap();
        store.clear().unwrap();
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn extracted_text_set_and_get() {
        let (store, _dir) = make_store();
        store.upsert_item(&sample_item("txt-001")).unwrap();
        store.set_extracted_text("txt-001", "The full body text.").unwrap();
        let got = store.get_extracted_text("txt-001").unwrap();
        assert_eq!(got.as_deref(), Some("The full body text."));
    }

    #[test]
    fn get_extracted_text_missing_returns_none() {
        let (store, _dir) = make_store();
        let got = store.get_extracted_text("no-such-id").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn open_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = BatchSessionStore::open_or_create(dir.path()).unwrap();
            s.upsert_item(&sample_item("persist-check")).unwrap();
        }
        let s2 = BatchSessionStore::open_or_create(dir.path()).unwrap();
        let loaded = s2.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "persist-check");
    }

    #[test]
    fn migration_sentinel_roundtrip() {
        let (store, _dir) = make_store();
        assert!(!store.is_migrated().unwrap());
        store.mark_migrated().unwrap();
        assert!(store.is_migrated().unwrap());
        // idempotent
        store.mark_migrated().unwrap();
        assert!(store.is_migrated().unwrap());
    }

    #[test]
    fn optional_bool_fields_roundtrip() {
        let (store, _dir) = make_store();
        let mut item = sample_item("bool-test");
        item.is_ignored = Some(true);
        item.is_duplicate_primary = Some(false);
        item.is_chapter_representative = Some(true);
        item.chapter_is_edited_volume = Some(false);
        store.upsert_item(&item).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded[0].is_ignored, Some(true));
        assert_eq!(loaded[0].is_duplicate_primary, Some(false));
        assert_eq!(loaded[0].is_chapter_representative, Some(true));
        assert_eq!(loaded[0].chapter_is_edited_volume, Some(false));
    }

    #[test]
    fn processed_history_roundtrip() {
        let (store, _dir) = make_store();
        let row = ProcessedHistoryRow {
            sha256: "abc123".to_owned(),
            filename: "kant.pdf".to_owned(),
            size_bytes: 98765,
            suggested_title: Some("Critique of Pure Reason".to_owned()),
            suggested_author: Some("Kant, Immanuel".to_owned()),
            suggested_year: Some("1781".to_owned()),
            target_path: Some("/sorted/Kant/kant.pdf".to_owned()),
            processed_at: 1_700_000_000_000,
        };
        store.record_processed(&row).unwrap();
        let found = store.lookup_history_by_sha256("abc123").unwrap();
        assert!(found.is_some());
        let f = found.unwrap();
        assert_eq!(f.sha256, "abc123");
        assert_eq!(f.suggested_title.as_deref(), Some("Critique of Pure Reason"));
        assert_eq!(f.suggested_author.as_deref(), Some("Kant, Immanuel"));
        // Missing sha256 returns None
        assert!(store.lookup_history_by_sha256("no-such-hash").unwrap().is_none());
    }

    #[test]
    fn processed_history_upsert_updates() {
        let (store, _dir) = make_store();
        let mut row = ProcessedHistoryRow {
            sha256: "dup-hash".to_owned(),
            filename: "file.pdf".to_owned(),
            size_bytes: 1000,
            suggested_title: Some("Old Title".to_owned()),
            suggested_author: None,
            suggested_year: None,
            target_path: None,
            processed_at: 1_700_000_000_000,
        };
        store.record_processed(&row).unwrap();
        row.suggested_title = Some("New Title".to_owned());
        store.record_processed(&row).unwrap(); // idempotent upsert
        let found = store.lookup_history_by_sha256("dup-hash").unwrap().unwrap();
        assert_eq!(found.suggested_title.as_deref(), Some("New Title"));
        assert_eq!(store.processed_history_count().unwrap(), 1);
    }
}
