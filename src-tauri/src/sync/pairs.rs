//! Persisted local-folder ↔ cloud-drive sync-pair definitions.
//!
//! This is the configuration boundary for general sync.  It deliberately
//! does not execute transfers yet: cloud-backup shard sync remains separate,
//! while the future runner can consume these stable IDs, filters, and
//! watermarks without changing the on-disk format.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncPairMode {
    ToCloud,
    ToLocal,
    TwoWay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPair {
    pub id: String,
    pub local_root: String,
    pub drive_id: String,
    pub remote_root: String,
    pub mode: SyncPairMode,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub watermark: i64,
    pub enabled: bool,
    pub updated_at: i64,
}

pub struct SyncPairStore {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sync_pairs (
    id TEXT PRIMARY KEY,
    local_root TEXT NOT NULL,
    drive_id TEXT NOT NULL,
    remote_root TEXT NOT NULL,
    mode TEXT NOT NULL,
    include_globs TEXT NOT NULL,
    exclude_globs TEXT NOT NULL,
    watermark INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL
);
";

impl SyncPairStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let conn = Connection::open(data_dir.join("sync_pairs.db"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn upsert(&self, mut pair: SyncPair) -> Result<SyncPair> {
        pair.updated_at = now_ms();
        self.conn.execute(
            "INSERT INTO sync_pairs
             (id, local_root, drive_id, remote_root, mode, include_globs,
              exclude_globs, watermark, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
               local_root=excluded.local_root, drive_id=excluded.drive_id,
               remote_root=excluded.remote_root, mode=excluded.mode,
               include_globs=excluded.include_globs,
               exclude_globs=excluded.exclude_globs,
               watermark=excluded.watermark, enabled=excluded.enabled,
               updated_at=excluded.updated_at",
            params![
                pair.id,
                pair.local_root,
                pair.drive_id,
                pair.remote_root,
                serde_json::to_string(&pair.mode)?,
                serde_json::to_string(&pair.include_globs)?,
                serde_json::to_string(&pair.exclude_globs)?,
                pair.watermark,
                pair.enabled,
                pair.updated_at,
            ],
        )?;
        Ok(pair)
    }

    pub fn list(&self) -> Result<Vec<SyncPair>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, local_root, drive_id, remote_root, mode, include_globs,
                    exclude_globs, watermark, enabled, updated_at
             FROM sync_pairs ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let mode: String = row.get(4)?;
            let includes: String = row.get(5)?;
            let excludes: String = row.get(6)?;
            Ok(SyncPair {
                id: row.get(0)?,
                local_root: row.get(1)?,
                drive_id: row.get(2)?,
                remote_root: row.get(3)?,
                mode: serde_json::from_str(&mode).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                include_globs: serde_json::from_str(&includes).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                exclude_globs: serde_json::from_str(&excludes).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                watermark: row.get(7)?,
                enabled: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM sync_pairs WHERE id = ?1", [id])?
            != 0)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> SyncPair {
        SyncPair {
            id: "pair-1".into(),
            local_root: "/tmp/docs".into(),
            drive_id: "drive-1".into(),
            remote_root: "/backup/docs".into(),
            mode: SyncPairMode::TwoWay,
            include_globs: vec!["**/*.pdf".into()],
            exclude_globs: vec!["**/.cache/**".into()],
            watermark: 42,
            enabled: true,
            updated_at: 0,
        }
    }

    #[test]
    fn pair_store_round_trips_filters_mode_and_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncPairStore::open(dir.path()).unwrap();
        let saved = store.upsert(pair()).unwrap();
        assert!(saved.updated_at > 0);
        let rows = store.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mode, SyncPairMode::TwoWay);
        assert_eq!(rows[0].include_globs, vec!["**/*.pdf"]);
        assert_eq!(rows[0].watermark, 42);
    }

    #[test]
    fn upsert_is_idempotent_and_delete_reports_presence() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncPairStore::open(dir.path()).unwrap();
        store.upsert(pair()).unwrap();
        let mut changed = pair();
        changed.enabled = false;
        store.upsert(changed).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(!store.list().unwrap()[0].enabled);
        assert!(store.delete("pair-1").unwrap());
        assert!(!store.delete("pair-1").unwrap());
    }
}
