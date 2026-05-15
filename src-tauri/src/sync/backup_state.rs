//! Stage Q — per-shard backup-state tracking.
//!
//! Persists the last-backup timestamp and VPS watermark for every
//! shard prefix so incremental backups skip unchanged shards.
//! Lives at `<data-dir>/backup_state.db` (SQLite, one row per prefix).

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS shard_backups (
    prefix         TEXT NOT NULL PRIMARY KEY,
    last_backup_at INTEGER NOT NULL DEFAULT 0,  -- epoch-ms of last successful backup
    last_watermark INTEGER NOT NULL DEFAULT 0,  -- VPS max_indexed_at at backup time
    drive_id       TEXT NOT NULL DEFAULT '',    -- drive the backup was uploaded to
    drive_path     TEXT NOT NULL DEFAULT ''     -- path on the drive
);
";

pub struct BackupState {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct BackupRecord {
    pub prefix:         String,
    pub last_backup_at: i64,
    pub last_watermark: i64,
    pub drive_id:       String,
    pub drive_path:     String,
}

impl BackupState {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("backup_state.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("opening backup_state.db at {}", path.display()))?;
        conn.execute_batch(SCHEMA).context("backup_state schema")?;
        Ok(Self { conn })
    }

    /// Return the last backup record for `prefix`, if any.
    pub fn last_backup(&self, prefix: &str) -> Result<Option<BackupRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT prefix, last_backup_at, last_watermark, drive_id, drive_path
             FROM shard_backups WHERE prefix = ?"
        )?;
        let mut rows = stmt.query(params![prefix])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(BackupRecord {
                prefix:         row.get(0)?,
                last_backup_at: row.get(1)?,
                last_watermark: row.get(2)?,
                drive_id:       row.get(3)?,
                drive_path:     row.get(4)?,
            }));
        }
        Ok(None)
    }

    /// Record a successful backup for `prefix`.
    pub fn record_backup(
        &self,
        prefix:    &str,
        watermark: i64,
        drive_id:  &str,
        drive_path: &str,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO shard_backups (prefix, last_backup_at, last_watermark, drive_id, drive_path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(prefix) DO UPDATE SET
               last_backup_at = excluded.last_backup_at,
               last_watermark = excluded.last_watermark,
               drive_id       = excluded.drive_id,
               drive_path     = excluded.drive_path",
            params![prefix, now, watermark, drive_id, drive_path],
        ).context("record_backup upsert")?;
        Ok(())
    }

    /// All recorded backup entries, newest first.
    pub fn all(&self) -> Result<Vec<BackupRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT prefix, last_backup_at, last_watermark, drive_id, drive_path
             FROM shard_backups ORDER BY last_backup_at DESC"
        )?;
        let rows = stmt.query_map([], |r| Ok(BackupRecord {
            prefix:         r.get(0)?,
            last_backup_at: r.get(1)?,
            last_watermark: r.get(2)?,
            drive_id:       r.get(3)?,
            drive_path:     r.get(4)?,
        }))?;
        rows.collect::<Result<Vec<_>, _>>().context("backup_state all")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_backup_record() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bs = BackupState::open(tmp.path()).unwrap();
        assert!(bs.last_backup("aa").unwrap().is_none());

        bs.record_backup("aa", 12345, "drive1", "cb-backups/2026-05-15/aa.tar.gz").unwrap();
        let rec = bs.last_backup("aa").unwrap().unwrap();
        assert_eq!(rec.prefix, "aa");
        assert_eq!(rec.last_watermark, 12345);
        assert_eq!(rec.drive_id, "drive1");

        // Upsert updates in-place.
        bs.record_backup("aa", 99999, "drive1", "cb-backups/2026-05-16/aa.tar.gz").unwrap();
        assert_eq!(bs.all().unwrap().len(), 1);
        assert_eq!(bs.last_backup("aa").unwrap().unwrap().last_watermark, 99999);
    }
}
