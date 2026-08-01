//! Stage Q — per-shard backup-state tracking.
//!
//! Persists the last-backup timestamp and VPS watermark for every
//! shard prefix so incremental backups skip unchanged shards.
//! Lives at `<data-dir>/backup_state.db` (SQLite, one row per prefix).

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS shard_backups (
    prefix         TEXT NOT NULL PRIMARY KEY,
    last_backup_at INTEGER NOT NULL DEFAULT 0,  -- epoch-ms of last successful backup
    last_watermark INTEGER NOT NULL DEFAULT 0,  -- VPS max_indexed_at at backup time
    drive_id       TEXT NOT NULL DEFAULT '',    -- drive the backup was uploaded to
    drive_path     TEXT NOT NULL DEFAULT ''     -- path on the drive
);

CREATE TABLE IF NOT EXISTS backup_jobs (
    id              TEXT NOT NULL PRIMARY KEY,
    source_root     TEXT NOT NULL,
    drive_id        TEXT NOT NULL,
    remote_root     TEXT NOT NULL,
    schedule        TEXT NOT NULL,
    retention_count INTEGER NOT NULL DEFAULT 7,
    verify_integrity INTEGER NOT NULL DEFAULT 1,
    enabled         INTEGER NOT NULL DEFAULT 1,
    last_run_at     INTEGER,
    last_status     TEXT,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS backup_runs (
    id              TEXT NOT NULL PRIMARY KEY,
    job_id          TEXT NOT NULL,
    status          TEXT NOT NULL,
    planned         INTEGER NOT NULL DEFAULT 0,
    completed       INTEGER NOT NULL DEFAULT 0,
    failed          INTEGER NOT NULL DEFAULT 0,
    verified        INTEGER NOT NULL DEFAULT 0,
    bytes           INTEGER NOT NULL DEFAULT 0,
    error           TEXT,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER
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

/// Schedule policy for a configured backup job.
///
/// `Manual` is useful while a job is being configured and for callers that
/// trigger execution explicitly. Scheduling itself is intentionally outside
/// this persistence layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BackupSchedule {
    Manual,
    IntervalMinutes { minutes: u64 },
    Daily { hour: u8, minute: u8 },
}

impl BackupSchedule {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Manual => Ok(()),
            Self::IntervalMinutes { minutes } if *minutes > 0 => Ok(()),
            Self::IntervalMinutes { .. } => anyhow::bail!("backup interval must be greater than zero"),
            Self::Daily { hour, minute } if *hour < 24 && *minute < 60 => Ok(()),
            Self::Daily { .. } => anyhow::bail!("daily backup time must be a valid 24-hour time"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupJob {
    pub id: String,
    pub source_root: String,
    pub drive_id: String,
    pub remote_root: String,
    pub schedule: BackupSchedule,
    pub retention_count: u32,
    pub verify_integrity: bool,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub last_status: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupRunStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupRun {
    pub id: String,
    pub job_id: String,
    pub status: BackupRunStatus,
    pub planned: u64,
    pub completed: u64,
    pub failed: u64,
    pub verified: bool,
    pub bytes: u64,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl BackupState {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("backup_state.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("opening backup_state.db at {}", path.display()))?;
        conn.execute_batch(SCHEMA).context("backup_state schema")?;
        conn.execute(
            "UPDATE backup_runs SET status = 'interrupted', finished_at = ?1,
             error = COALESCE(error, 'process restarted while backup was running')
             WHERE status = 'running'",
            params![now_ms()],
        ).context("recover interrupted backup runs")?;
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

    pub fn upsert_job(&self, mut job: BackupJob) -> Result<BackupJob> {
        if job.id.trim().is_empty() || job.source_root.trim().is_empty()
            || job.drive_id.trim().is_empty() || job.remote_root.trim().is_empty()
        {
            anyhow::bail!("backup job id, source root, drive id, and remote root are required");
        }
        if job.retention_count == 0 {
            anyhow::bail!("backup retention count must be greater than zero");
        }
        job.schedule.validate()?;
        job.updated_at = now_ms();
        let schedule = serde_json::to_string(&job.schedule).context("serialize backup schedule")?;
        self.conn.execute(
            "INSERT INTO backup_jobs
             (id, source_root, drive_id, remote_root, schedule, retention_count,
              verify_integrity, enabled, last_run_at, last_status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
              source_root = excluded.source_root, drive_id = excluded.drive_id,
              remote_root = excluded.remote_root, schedule = excluded.schedule,
              retention_count = excluded.retention_count,
              verify_integrity = excluded.verify_integrity, enabled = excluded.enabled,
              updated_at = excluded.updated_at",
            params![job.id, job.source_root, job.drive_id, job.remote_root, schedule,
                job.retention_count, job.verify_integrity, job.enabled,
                job.last_run_at, job.last_status, job.updated_at],
        ).context("upsert backup job")?;
        Ok(job)
    }

    pub fn list_jobs(&self) -> Result<Vec<BackupJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_root, drive_id, remote_root, schedule, retention_count,
                    verify_integrity, enabled, last_run_at, last_status, updated_at
             FROM backup_jobs ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let schedule_json: String = row.get(4)?;
            let schedule = serde_json::from_str(&schedule_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4, rusqlite::types::Type::Text, Box::new(e),
                )
            })?;
            Ok(BackupJob {
                id: row.get(0)?, source_root: row.get(1)?, drive_id: row.get(2)?,
                remote_root: row.get(3)?, schedule, retention_count: row.get(5)?,
                verify_integrity: row.get(6)?, enabled: row.get(7)?,
                last_run_at: row.get(8)?, last_status: row.get(9)?, updated_at: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("list backup jobs")
    }

    pub fn delete_job(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM backup_jobs WHERE id = ?", params![id])? > 0)
    }

    pub fn start_run(&self, job_id: &str, planned: u64) -> Result<BackupRun> {
        if job_id.trim().is_empty() {
            anyhow::bail!("backup run job id is required");
        }
        let run = BackupRun {
            id: uuid::Uuid::new_v4().to_string(), job_id: job_id.to_owned(),
            status: BackupRunStatus::Running, planned, completed: 0, failed: 0,
            verified: false, bytes: 0, error: None, started_at: now_ms(), finished_at: None,
        };
        self.conn.execute(
            "INSERT INTO backup_runs
             (id, job_id, status, planned, completed, failed, verified, bytes, error, started_at)
             VALUES (?1, ?2, 'running', ?3, 0, 0, 0, 0, NULL, ?4)",
            params![run.id, run.job_id, run.planned, run.started_at],
        ).context("start backup run")?;
        Ok(run)
    }

    pub fn finish_run(
        &self,
        id: &str,
        completed: u64,
        failed: u64,
        verified: bool,
        bytes: u64,
    ) -> Result<BackupRun> {
        let status = if failed == 0 { "completed" } else { "failed" };
        self.conn.execute(
            "UPDATE backup_runs SET status = ?1, completed = ?2, failed = ?3,
             verified = ?4, bytes = ?5, finished_at = ?6 WHERE id = ?7",
            params![status, completed, failed, verified, bytes, now_ms(), id],
        ).context("finish backup run")?;
        self.run(id)?.ok_or_else(|| anyhow::anyhow!("backup run '{id}' not found"))
    }

    pub fn fail_run(&self, id: &str, error: &str) -> Result<BackupRun> {
        self.conn.execute(
            "UPDATE backup_runs SET status = 'failed', error = ?1, finished_at = ?2 WHERE id = ?3",
            params![error, now_ms(), id],
        ).context("fail backup run")?;
        self.run(id)?.ok_or_else(|| anyhow::anyhow!("backup run '{id}' not found"))
    }

    pub fn run(&self, id: &str) -> Result<Option<BackupRun>> {
        self.conn.query_row(
            "SELECT id, job_id, status, planned, completed, failed, verified, bytes,
                    error, started_at, finished_at FROM backup_runs WHERE id = ?",
            params![id], |row| Self::run_from_row(row),
        ).optional().context("read backup run")
    }

    pub fn list_runs(&self, job_id: &str, limit: usize) -> Result<Vec<BackupRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, status, planned, completed, failed, verified, bytes,
                    error, started_at, finished_at FROM backup_runs
             WHERE job_id = ? ORDER BY started_at DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![job_id, limit as i64], Self::run_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().context("list backup runs")
    }

    fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackupRun> {
        let status: String = row.get(2)?;
        let status = match status.as_str() {
            "running" => BackupRunStatus::Running,
            "completed" => BackupRunStatus::Completed,
            "failed" => BackupRunStatus::Failed,
            "interrupted" => BackupRunStatus::Interrupted,
            other => return Err(rusqlite::Error::InvalidColumnType(
                2, other.to_owned(), rusqlite::types::Type::Text,
            )),
        };
        Ok(BackupRun {
            id: row.get(0)?, job_id: row.get(1)?, status,
            planned: row.get(3)?, completed: row.get(4)?, failed: row.get(5)?,
            verified: row.get(6)?, bytes: row.get(7)?, error: row.get(8)?,
            started_at: row.get(9)?, finished_at: row.get(10)?,
        })
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64).unwrap_or(0)
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

    #[test]
    fn backup_job_round_trip_and_validation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bs = BackupState::open(tmp.path()).unwrap();
        let job = BackupJob {
            id: "documents".into(), source_root: "/data/docs".into(),
            drive_id: "filen".into(), remote_root: "/backups/docs".into(),
            schedule: BackupSchedule::Daily { hour: 2, minute: 30 },
            retention_count: 14, verify_integrity: true, enabled: true,
            last_run_at: None, last_status: None, updated_at: 0,
        };
        let saved = bs.upsert_job(job.clone()).unwrap();
        assert!(saved.updated_at > 0);
        let listed = bs.list_jobs().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].schedule, job.schedule);
        assert!(bs.delete_job("documents").unwrap());
        assert!(bs.list_jobs().unwrap().is_empty());
        assert!(BackupSchedule::IntervalMinutes { minutes: 0 }.validate().is_err());
    }

    #[test]
    fn backup_run_lifecycle_and_restart_recovery() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bs = BackupState::open(tmp.path()).unwrap();
        let run = bs.start_run("documents", 3).unwrap();
        let finished = bs.finish_run(&run.id, 3, 0, true, 42).unwrap();
        assert_eq!(finished.status, BackupRunStatus::Completed);
        assert_eq!(bs.list_runs("documents", 10).unwrap().len(), 1);

        let interrupted = bs.start_run("documents", 1).unwrap();
        drop(bs);
        let reopened = BackupState::open(tmp.path()).unwrap();
        let recovered = reopened.run(&interrupted.id).unwrap().unwrap();
        assert_eq!(recovered.status, BackupRunStatus::Interrupted);
        assert!(recovered.finished_at.is_some());
    }
}
