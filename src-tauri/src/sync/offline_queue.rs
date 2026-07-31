//! P29.3 — Offline operation queue with replay.
//!
//! Persists failed or interrupted cloud operations to a WAL-mode SQLite
//! database and replays them when connectivity is restored.
//!
//! # Design
//!
//! Distinct from `SyncManager`'s outbox (which handles crisp-index-server
//! sync ops).  The offline queue covers *any* cloud drive operation —
//! uploads, downloads, deletes — that failed due to transient network
//! errors.  Operations are replayed in FIFO order when a health-check
//! probe succeeds.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ── Types ────────────────────────────────────────────────────────────────

/// A queued operation waiting for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedOp {
    pub id: i64,
    pub op_type: String,
    pub payload: String,
    pub provider_id: String,
    pub retry_count: i32,
    pub last_error: Option<String>,
    pub status: String,
    pub created_at: i64,
}

/// Summary stats for the offline queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStats {
    pub pending: usize,
    pub failed: usize,
    pub total: usize,
}

// ── OfflineQueue ─────────────────────────────────────────────────────────

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS queued_ops (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    op_type     TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    provider_id TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    status      TEXT    NOT NULL DEFAULT 'pending'
);

CREATE INDEX IF NOT EXISTS idx_queued_ops_status ON queued_ops(status);
";

/// Maximum retries before an operation is marked as permanently failed.
const MAX_RETRIES: i32 = 10;

#[derive(Clone)]
pub struct OfflineQueue {
    conn: Arc<Mutex<Connection>>,
}

impl OfflineQueue {
    /// Open or create the offline queue database.
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("offline_queue.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening offline queue at {}", db_path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Enqueue a new operation.
    pub fn enqueue(&self, op_type: &str, payload: &str, provider_id: &str) -> Result<i64> {
        let now = now_unix_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO queued_ops (op_type, payload, provider_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![op_type, payload, provider_id, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Dequeue a batch of pending operations (FIFO order).
    pub fn dequeue_batch(&self, limit: usize) -> Result<Vec<QueuedOp>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, op_type, payload, provider_id, retry_count, last_error, status, created_at
             FROM queued_ops
             WHERE status = 'pending' AND retry_count < ?1
             ORDER BY created_at ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![MAX_RETRIES, limit as i64], |row| {
                Ok(QueuedOp {
                    id: row.get(0)?,
                    op_type: row.get(1)?,
                    payload: row.get(2)?,
                    provider_id: row.get(3)?,
                    retry_count: row.get(4)?,
                    last_error: row.get(5)?,
                    status: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Return all queued operations, including failed/cancelled records.
    pub fn list(&self) -> Result<Vec<QueuedOp>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, op_type, payload, provider_id, retry_count, last_error, status, created_at
             FROM queued_ops ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(QueuedOp {
                    id: row.get(0)?,
                    op_type: row.get(1)?,
                    payload: row.get(2)?,
                    provider_id: row.get(3)?,
                    retry_count: row.get(4)?,
                    last_error: row.get(5)?,
                    status: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark an operation as successfully completed (removes it).
    pub fn mark_done(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM queued_ops WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Mark an operation as failed, incrementing retry count.
    /// If retries exceed `MAX_RETRIES`, status changes to 'failed'.
    pub fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE queued_ops
             SET retry_count = retry_count + 1,
                 last_error = ?2,
                 status = CASE WHEN retry_count + 1 >= ?3 THEN 'failed' ELSE 'pending' END
             WHERE id = ?1",
            params![id, error, MAX_RETRIES],
        )?;
        Ok(())
    }

    /// Cancel a pending operation while retaining its diagnostic record.
    pub fn cancel(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE queued_ops SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
            params![id],
        )?;
        Ok(changed != 0)
    }

    /// Number of pending operations (retries not exhausted).
    pub fn pending_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM queued_ops WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Queue statistics.
    pub fn stats(&self) -> Result<QueueStats> {
        let conn = self.conn.lock().unwrap();
        let pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM queued_ops WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )?;
        let failed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM queued_ops WHERE status = 'failed'",
            [],
            |r| r.get(0),
        )?;
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM queued_ops", [], |r| r.get(0))?;
        Ok(QueueStats {
            pending: pending as usize,
            failed: failed as usize,
            total: total as usize,
        })
    }

    /// Retry all permanently-failed operations (reset status to pending,
    /// reset retry count).
    pub fn retry_all_failed(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE queued_ops SET status = 'pending', retry_count = 0 WHERE status = 'failed'",
            [],
        )?;
        Ok(n)
    }

    /// Purge all completed and failed operations older than `max_age`.
    pub fn purge_old(&self, max_age: Duration) -> Result<usize> {
        let cutoff = now_unix_ms() - max_age.as_millis() as i64;
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM queued_ops WHERE status = 'failed' AND created_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_queue() -> (tempfile::TempDir, OfflineQueue) {
        let dir = tempfile::tempdir().unwrap();
        let q = OfflineQueue::open(dir.path()).unwrap();
        (dir, q)
    }

    #[test]
    fn enqueue_and_dequeue_fifo() {
        let (_dir, q) = make_queue();
        q.enqueue("upload", r#"{"path":"/a"}"#, "drive-1").unwrap();
        q.enqueue("upload", r#"{"path":"/b"}"#, "drive-1").unwrap();
        q.enqueue("download", r#"{"path":"/c"}"#, "drive-2")
            .unwrap();

        let batch = q.dequeue_batch(10).unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].op_type, "upload");
        assert!(batch[0].payload.contains("/a"));
        assert_eq!(batch[2].op_type, "download");
    }

    #[test]
    fn mark_done_removes_from_queue() {
        let (_dir, q) = make_queue();
        let id = q.enqueue("upload", "{}", "d").unwrap();
        assert_eq!(q.pending_count().unwrap(), 1);

        q.mark_done(id).unwrap();
        assert_eq!(q.pending_count().unwrap(), 0);
    }

    #[test]
    fn mark_failed_increments_retries() {
        let (_dir, q) = make_queue();
        let id = q.enqueue("upload", "{}", "d").unwrap();

        q.mark_failed(id, "timeout").unwrap();
        let batch = q.dequeue_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].retry_count, 1);
        assert_eq!(batch[0].last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn retries_exhausted_marks_as_failed() {
        let (_dir, q) = make_queue();
        let id = q.enqueue("upload", "{}", "d").unwrap();

        // Exhaust retries.
        for i in 0..MAX_RETRIES {
            q.mark_failed(id, &format!("attempt {i}")).unwrap();
        }

        // Should no longer appear in pending.
        assert_eq!(q.pending_count().unwrap(), 0);

        let stats = q.stats().unwrap();
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.total, 1);
    }

    #[test]
    fn retry_all_failed_resets_status() {
        let (_dir, q) = make_queue();
        let id = q.enqueue("upload", "{}", "d").unwrap();

        for _ in 0..MAX_RETRIES {
            q.mark_failed(id, "err").unwrap();
        }
        assert_eq!(q.pending_count().unwrap(), 0);

        let n = q.retry_all_failed().unwrap();
        assert_eq!(n, 1);
        assert_eq!(q.pending_count().unwrap(), 1);

        let batch = q.dequeue_batch(10).unwrap();
        assert_eq!(batch[0].retry_count, 0);
    }

    #[test]
    fn stats_reports_correct_counts() {
        let (_dir, q) = make_queue();
        q.enqueue("a", "{}", "d").unwrap();
        q.enqueue("b", "{}", "d").unwrap();
        let id3 = q.enqueue("c", "{}", "d").unwrap();

        // Exhaust one.
        for _ in 0..MAX_RETRIES {
            q.mark_failed(id3, "err").unwrap();
        }

        let stats = q.stats().unwrap();
        assert_eq!(stats.pending, 2);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.total, 3);
    }

    #[test]
    fn list_and_cancel_preserve_operation_diagnostics() {
        let (_dir, q) = make_queue();
        let id = q.enqueue("upload", r#"{"path":"/a"}"#, "drive-1").unwrap();
        assert!(q.cancel(id).unwrap());
        assert!(!q.cancel(id).unwrap());
        let listed = q.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "cancelled");
        assert_eq!(q.stats().unwrap().total, 1);
    }
}
