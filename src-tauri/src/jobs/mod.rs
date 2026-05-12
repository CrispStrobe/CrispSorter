/// Durable, resumable job queue for long-running ingest operations.
///
/// Answers the 500K-file / multi-day scenario: a user starts indexing
/// a 2 TB archive, the app is closed after two hours, and on restart
/// exactly the unfinished files are picked back up — no re-scan, no
/// duplicate rows, no lost progress.
///
/// # Storage
///
/// Two SQLite tables in `<data_dir>/crisp_jobs.db`:
///
/// * `ingest_jobs`  — one row per user-initiated job (name, source paths,
///   target level, aggregate counts).
/// * `file_queue`   — one row per file within a job.  Tracks status,
///   retry count, and error text at per-file granularity.
///
/// The file_queue stores only path + doc_id references — never embedding
/// payloads — so the database stays small (< 1 MB per 100 K files).
///
/// # Concurrency
///
/// All public methods that touch SQLite are synchronous and intended to
/// be called via `tokio::task::spawn_blocking` from async callers.  The
/// connection is protected by a `std::sync::Mutex`; only one writer runs
/// at a time, which is fine given SQLite WAL and the low write rate.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

pub mod tauri_commands;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Paused,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Pending,
    InProgress,
    Done,
    Error,
    Skipped,
}

/// Summary returned by `list_jobs` and `get_job`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: String,
    pub job_type: String,
    pub source_paths: Vec<String>,
    pub target_level: u8,
    pub total_files: i64,
    pub done_files: i64,
    pub error_files: i64,
    pub skipped_files: i64,
    pub in_progress_files: i64,
}

/// One file entry returned by `claim_batch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedFile {
    pub row_id: i64,
    pub job_id: String,
    pub file_path: String,
    pub doc_id: Option<String>,
    pub target_level: u8,
    pub retry_count: i32,
}

/// One file entry returned by `list_files` (for display / UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedFile {
    pub row_id: i64,
    pub job_id: String,
    pub file_path: String,
    pub doc_id: Option<String>,
    pub target_level: u8,
    pub status: String,
    pub error_text: Option<String>,
    pub retry_count: i32,
}

/// Input for `add_files`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub file_path: String,
    /// SHA-256 of the file bytes if already computed; `None` if the caller
    /// will hash it later and call `set_doc_id`.
    pub doc_id: Option<String>,
    pub target_level: u8,
}

// ── JobQueue ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct JobQueue {
    conn: Arc<Mutex<Connection>>,
}

impl JobQueue {
    /// Open (or create) the jobs database at `<data_dir>/crisp_jobs.db`.
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("crisp_jobs.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening jobs database {}", db_path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("setting busy timeout")?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enabling WAL mode")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("setting synchronous mode")?;
        conn.execute_batch(SCHEMA).context("creating jobs schema")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Borrow the underlying connection handle for callers that need
    /// to host their own tables alongside the job queue.  Used by
    /// the schema-migration ledger and the P13.5 translation cache —
    /// both want to live in the same `crisp_jobs.db` (one file at
    /// startup, no extra connections) rather than spinning up sibling
    /// SQLite databases for tiny admin/cache tables.
    ///
    /// The returned `Arc<Mutex<Connection>>` shares the same WAL-mode
    /// connection + 5 s busy-timeout the rest of `JobQueue` uses, so
    /// concurrent writers serialise correctly.
    pub fn conn_arc(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    // ── Job lifecycle ──────────────────────────────────────────────────────────

    /// Create a new job and return its UUID.
    pub fn create_job(
        &self,
        job_type: &str,
        source_paths: &[String],
        target_level: u8,
        config_json: Option<&str>,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let paths_json = serde_json::to_string(source_paths).context("serializing source_paths")?;
        self.conn.lock().unwrap().execute(
            r#"
            INSERT INTO ingest_jobs
                (id, created_at, updated_at, status, job_type,
                 source_paths, target_level, config_json,
                 total_files, done_files, error_files, skipped_files)
            VALUES (?1, ?2, ?2, 'pending', ?3, ?4, ?5, ?6, 0, 0, 0, 0)
            "#,
            params![id, now, job_type, paths_json, i64::from(target_level), config_json],
        )
        .context("inserting job")?;
        Ok(id)
    }

    pub fn set_job_status(&self, job_id: &str, status: &str) -> Result<()> {
        let now = now_ms();
        self.conn.lock().unwrap().execute(
            "UPDATE ingest_jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, job_id],
        )
        .context("updating job status")?;
        Ok(())
    }

    pub fn get_job(&self, job_id: &str) -> Result<Option<JobSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(JOB_SELECT_SQL).context("preparing job select")?;
        stmt.query_row(params![job_id], job_row_to_summary)
            .optional()
            .context("reading job")
    }

    pub fn list_jobs(&self) -> Result<Vec<JobSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!("{JOB_SELECT_ALL_SQL} ORDER BY created_at DESC"))
            .context("preparing list jobs")?;
        let rows = stmt
            .query_map([], job_row_to_summary)
            .context("querying jobs")?;
        rows.collect::<std::result::Result<Vec<_>, _>>().context("collecting jobs")
    }

    pub fn delete_job(&self, job_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting delete transaction")?;
        tx.execute("DELETE FROM file_queue WHERE job_id = ?1", params![job_id])
            .context("deleting file_queue rows")?;
        tx.execute("DELETE FROM ingest_jobs WHERE id = ?1", params![job_id])
            .context("deleting job row")?;
        tx.commit().context("committing delete")?;
        Ok(())
    }

    // ── File queue management ─────────────────────────────────────────────────

    /// Bulk-insert files into the queue for a job.
    /// Files already present (same job_id + file_path) are silently ignored
    /// via `INSERT OR IGNORE`.
    pub fn add_files(&self, job_id: &str, files: &[FileEntry]) -> Result<usize> {
        if files.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting add_files transaction")?;
        let mut inserted = 0usize;
        for f in files {
            let n = tx.execute(
                r#"
                INSERT OR IGNORE INTO file_queue
                    (job_id, file_path, doc_id, target_level, status,
                     error_text, retry_count, last_attempted)
                VALUES (?1, ?2, ?3, ?4, 'pending', NULL, 0, NULL)
                "#,
                params![job_id, f.file_path, f.doc_id, i64::from(f.target_level)],
            )
            .context("inserting file_queue row")?;
            inserted += n;
        }
        // Refresh total_files count.
        tx.execute(
            r#"
            UPDATE ingest_jobs
            SET total_files = (
                SELECT COUNT(*) FROM file_queue WHERE job_id = ?1
            ),
            updated_at = ?2
            WHERE id = ?1
            "#,
            params![job_id, now_ms()],
        )
        .context("refreshing total_files")?;
        tx.commit().context("committing add_files")?;
        Ok(inserted)
    }

    /// Claim up to `batch_size` pending files and mark them `in_progress`.
    /// Returns the claimed rows; empty means the job's pending queue is drained.
    pub fn claim_batch(&self, job_id: &str, batch_size: usize) -> Result<Vec<QueuedFile>> {
        let limit = i64::try_from(batch_size).unwrap_or(i64::MAX);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("starting claim transaction")?;

        let ids: Vec<i64> = {
            let mut stmt = tx
                .prepare(
                    r#"
                    SELECT id FROM file_queue
                    WHERE job_id = ?1 AND status = 'pending'
                    ORDER BY id ASC
                    LIMIT ?2
                    "#,
                )
                .context("preparing claim select")?;
            let rows = stmt
                .query_map(params![job_id, limit], |r: &Row<'_>| r.get(0))
                .context("querying pending files")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("collecting claim ids")?
        };

        if ids.is_empty() {
            tx.commit().context("committing empty claim")?;
            return Ok(vec![]);
        }

        let now = now_ms();
        for &id in &ids {
            tx.execute(
                "UPDATE file_queue SET status = 'in_progress', last_attempted = ?1 WHERE id = ?2",
                params![now, id],
            )
            .context("marking in_progress")?;
        }

        let files: Vec<QueuedFile> = {
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, job_id, file_path, doc_id, target_level, retry_count \
                 FROM file_queue WHERE id IN ({placeholders})"
            );
            let mut stmt = tx.prepare(&sql).context("preparing claim read")?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(ids.iter()), |r: &Row<'_>| {
                    Ok(QueuedFile {
                        row_id: r.get(0)?,
                        job_id: r.get(1)?,
                        file_path: r.get(2)?,
                        doc_id: r.get(3)?,
                        target_level: r.get::<_, i64>(4).map(|v| v as u8)?,
                        retry_count: r.get(5)?,
                    })
                })
                .context("reading claimed files")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("collecting claimed files")?
        };

        tx.commit().context("committing claim")?;
        Ok(files)
    }

    /// Mark a batch of files as done and increment the job's `done_files` counter.
    pub fn mark_done(&self, job_id: &str, row_ids: &[i64]) -> Result<()> {
        if row_ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting mark_done transaction")?;
        for &id in row_ids {
            tx.execute(
                "UPDATE file_queue SET status = 'done', error_text = NULL WHERE id = ?1",
                params![id],
            )
            .context("marking file done")?;
        }
        let n = i64::try_from(row_ids.len()).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE ingest_jobs SET done_files = done_files + ?1, updated_at = ?2 WHERE id = ?3",
            params![n, now_ms(), job_id],
        )
        .context("incrementing done_files")?;
        tx.commit().context("committing mark_done")?;
        Ok(())
    }

    /// Mark one file as errored.  If `retry_count < max_retries`, it is
    /// re-queued (status back to `pending`) with an incremented counter.
    /// Otherwise it stays `error`.
    pub fn mark_error(
        &self,
        job_id: &str,
        row_id: i64,
        error: &str,
        max_retries: i32,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting mark_error transaction")?;
        let retry_count: i32 = tx
            .query_row(
                "SELECT retry_count FROM file_queue WHERE id = ?1",
                params![row_id],
                |r: &Row<'_>| r.get(0),
            )
            .optional()
            .context("reading retry_count")?
            .unwrap_or(0);

        if retry_count < max_retries {
            tx.execute(
                r#"
                UPDATE file_queue
                SET status = 'pending', retry_count = retry_count + 1, error_text = ?1
                WHERE id = ?2
                "#,
                params![error, row_id],
            )
            .context("re-queuing errored file")?;
        } else {
            tx.execute(
                r#"
                UPDATE file_queue
                SET status = 'error', retry_count = retry_count + 1, error_text = ?1
                WHERE id = ?2
                "#,
                params![error, row_id],
            )
            .context("marking file error")?;
            tx.execute(
                "UPDATE ingest_jobs SET error_files = error_files + 1, updated_at = ?1 WHERE id = ?2",
                params![now_ms(), job_id],
            )
            .context("incrementing error_files")?;
        }
        tx.commit().context("committing mark_error")?;
        Ok(())
    }

    /// Mark a file as skipped (DRM, unsupported format, etc.).
    pub fn mark_skipped(&self, job_id: &str, row_id: i64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting mark_skipped transaction")?;
        tx.execute(
            "UPDATE file_queue SET status = 'skipped' WHERE id = ?1",
            params![row_id],
        )
        .context("marking file skipped")?;
        tx.execute(
            "UPDATE ingest_jobs SET skipped_files = skipped_files + 1, updated_at = ?1 WHERE id = ?2",
            params![now_ms(), job_id],
        )
        .context("incrementing skipped_files")?;
        tx.commit().context("committing mark_skipped")?;
        Ok(())
    }

    /// Update the `doc_id` for a file after hashing completes.
    pub fn set_doc_id(&self, row_id: i64, doc_id: &str) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE file_queue SET doc_id = ?1 WHERE id = ?2",
                params![doc_id, row_id],
            )
            .context("setting doc_id")?;
        Ok(())
    }

    /// Reset all `in_progress` files for a job back to `pending`.
    /// Called on startup to reclaim work that was in-flight when the
    /// app was previously closed.
    pub fn reclaim_in_progress(&self, job_id: &str) -> Result<usize> {
        let n = self
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE file_queue SET status = 'pending' WHERE job_id = ?1 AND status = 'in_progress'",
                params![job_id],
            )
            .context("reclaiming in_progress files")?;
        Ok(n)
    }

    /// How many files in this job are still pending or in_progress.
    pub fn pending_count(&self, job_id: &str) -> Result<i64> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM file_queue WHERE job_id = ?1 AND status IN ('pending','in_progress')",
                params![job_id],
                |r: &Row<'_>| r.get(0),
            )
            .context("counting pending files")
    }

    /// List files in a job for display.  Optional `status_filter` restricts
    /// to a single status value (e.g. `"pending"`, `"error"`).  Returns at
    /// most `limit` rows starting at `offset`.
    pub fn list_files(
        &self,
        job_id: &str,
        status_filter: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ListedFile>> {
        let conn = self.conn.lock().unwrap();
        let map_row = |r: &Row<'_>| {
            Ok(ListedFile {
                row_id: r.get(0)?,
                job_id: r.get(1)?,
                file_path: r.get(2)?,
                doc_id: r.get(3)?,
                target_level: r.get::<_, i64>(4).map(|v| v as u8)?,
                status: r.get(5)?,
                error_text: r.get(6)?,
                retry_count: r.get(7)?,
            })
        };
        let files: Vec<ListedFile> = if let Some(st) = status_filter {
            let mut stmt = conn
                .prepare(
                    "SELECT id, job_id, file_path, doc_id, target_level, status, error_text, retry_count \
                     FROM file_queue WHERE job_id = ?1 AND status = ?2 ORDER BY id ASC LIMIT ?3 OFFSET ?4",
                )
                .context("preparing list_files (filtered)")?;
            let collected = stmt
                .query_map(params![job_id, st, limit, offset], map_row)
                .context("querying file_queue (filtered)")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("collecting listed files (filtered)")?;
            collected
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, job_id, file_path, doc_id, target_level, status, error_text, retry_count \
                     FROM file_queue WHERE job_id = ?1 ORDER BY id ASC LIMIT ?2 OFFSET ?3",
                )
                .context("preparing list_files")?;
            let collected = stmt
                .query_map(params![job_id, limit, offset], map_row)
                .context("querying file_queue")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("collecting listed files")?;
            collected
        };
        Ok(files)
    }

    /// Delete a single file-queue row.  Returns `true` if a row was deleted.
    pub fn remove_file(&self, row_id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let job_id: Option<String> = conn
            .query_row(
                "SELECT job_id FROM file_queue WHERE id = ?1",
                params![row_id],
                |r: &Row<'_>| r.get(0),
            )
            .optional()
            .context("reading job_id for remove_file")?;
        let Some(job_id) = job_id else { return Ok(false) };
        conn.execute("DELETE FROM file_queue WHERE id = ?1", params![row_id])
            .context("deleting file_queue row")?;
        conn.execute(
            "UPDATE ingest_jobs SET total_files = (SELECT COUNT(*) FROM file_queue WHERE job_id = ?1), updated_at = ?2 WHERE id = ?1",
            params![job_id, now_ms()],
        )
        .context("refreshing total_files after remove")?;
        Ok(true)
    }

    /// Delete all file-queue rows for a job that match `status`.
    /// Returns the number of rows deleted.
    pub fn remove_files_by_status(&self, job_id: &str, status: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "DELETE FROM file_queue WHERE job_id = ?1 AND status = ?2",
                params![job_id, status],
            )
            .context("deleting file_queue rows by status")?;
        conn.execute(
            "UPDATE ingest_jobs SET total_files = (SELECT COUNT(*) FROM file_queue WHERE job_id = ?1), updated_at = ?2 WHERE id = ?1",
            params![job_id, now_ms()],
        )
        .context("refreshing total_files after bulk remove")?;
        Ok(n)
    }
}

// ── SQL ───────────────────────────────────────────────────────────────────────

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ingest_jobs (
    id            TEXT    PRIMARY KEY,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    status        TEXT    NOT NULL DEFAULT 'pending',
    job_type      TEXT    NOT NULL DEFAULT 'ingest',
    source_paths  TEXT    NOT NULL,
    target_level  INTEGER NOT NULL DEFAULT 3,
    config_json   TEXT,
    total_files   INTEGER NOT NULL DEFAULT 0,
    done_files    INTEGER NOT NULL DEFAULT 0,
    error_files   INTEGER NOT NULL DEFAULT 0,
    skipped_files INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS file_queue (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id         TEXT    NOT NULL REFERENCES ingest_jobs(id),
    file_path      TEXT    NOT NULL,
    doc_id         TEXT,
    target_level   INTEGER NOT NULL DEFAULT 3,
    status         TEXT    NOT NULL DEFAULT 'pending',
    error_text     TEXT,
    retry_count    INTEGER NOT NULL DEFAULT 0,
    last_attempted INTEGER,
    UNIQUE(job_id, file_path)
);

CREATE INDEX IF NOT EXISTS fq_status ON file_queue(job_id, status);
"#;

const JOB_SELECT_SQL: &str = r#"
SELECT
    j.id, j.created_at, j.updated_at, j.status, j.job_type,
    j.source_paths, j.target_level,
    j.total_files, j.done_files, j.error_files, j.skipped_files,
    COALESCE((SELECT COUNT(*) FROM file_queue
              WHERE job_id = j.id AND status = 'in_progress'), 0) AS in_progress
FROM ingest_jobs j
WHERE j.id = ?1
"#;

const JOB_SELECT_ALL_SQL: &str = r#"
SELECT
    j.id, j.created_at, j.updated_at, j.status, j.job_type,
    j.source_paths, j.target_level,
    j.total_files, j.done_files, j.error_files, j.skipped_files,
    COALESCE((SELECT COUNT(*) FROM file_queue
              WHERE job_id = j.id AND status = 'in_progress'), 0) AS in_progress
FROM ingest_jobs j
"#;

fn job_row_to_summary(r: &Row<'_>) -> rusqlite::Result<JobSummary> {
    let paths_json: String = r.get(5)?;
    let source_paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
    Ok(JobSummary {
        id: r.get(0)?,
        created_at: r.get(1)?,
        updated_at: r.get(2)?,
        status: r.get(3)?,
        job_type: r.get(4)?,
        source_paths,
        target_level: r.get::<_, i64>(6).map(|v| v as u8)?,
        total_files: r.get(7)?,
        done_files: r.get(8)?,
        error_files: r.get(9)?,
        skipped_files: r.get(10)?,
        in_progress_files: r.get(11)?,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_queue() -> (JobQueue, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let q = JobQueue::open_or_create(dir.path()).unwrap();
        (q, dir)
    }

    fn sample_files(job_id: &str, n: usize) -> Vec<FileEntry> {
        (0..n)
            .map(|i| FileEntry {
                file_path: format!("/docs/file_{i}.pdf"),
                doc_id: None,
                target_level: 3,
            })
            .collect()
    }

    #[test]
    fn create_and_list_job() {
        let (q, _dir) = tmp_queue();
        let id = q.create_job("ingest", &["/docs".to_owned()], 3, None).unwrap();
        let jobs = q.list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, id);
        assert_eq!(jobs[0].status, "pending");
    }

    #[test]
    fn add_files_deduplicates() {
        let (q, _dir) = tmp_queue();
        let jid = q.create_job("ingest", &[], 3, None).unwrap();
        let files = sample_files(&jid, 3);
        let n1 = q.add_files(&jid, &files).unwrap();
        let n2 = q.add_files(&jid, &files).unwrap();
        assert_eq!(n1, 3);
        assert_eq!(n2, 0); // duplicates ignored
        let job = q.get_job(&jid).unwrap().unwrap();
        assert_eq!(job.total_files, 3);
    }

    #[test]
    fn claim_mark_done_roundtrip() {
        let (q, _dir) = tmp_queue();
        let jid = q.create_job("ingest", &[], 3, None).unwrap();
        q.add_files(&jid, &sample_files(&jid, 5)).unwrap();

        let batch = q.claim_batch(&jid, 3).unwrap();
        assert_eq!(batch.len(), 3);
        assert!(batch.iter().all(|f| f.job_id == jid));

        let ids: Vec<i64> = batch.iter().map(|f| f.row_id).collect();
        q.mark_done(&jid, &ids).unwrap();

        let job = q.get_job(&jid).unwrap().unwrap();
        assert_eq!(job.done_files, 3);

        // 2 still pending
        assert_eq!(q.pending_count(&jid).unwrap(), 2);
    }

    #[test]
    fn error_retries_then_fails() {
        let (q, _dir) = tmp_queue();
        let jid = q.create_job("ingest", &[], 3, None).unwrap();
        q.add_files(&jid, &sample_files(&jid, 1)).unwrap();

        let max_retries = 2;
        let file = q.claim_batch(&jid, 1).unwrap().into_iter().next().unwrap();
        q.mark_error(&jid, file.row_id, "boom", max_retries).unwrap();
        // retry_count=1, still pending
        let file2 = q.claim_batch(&jid, 1).unwrap().into_iter().next().unwrap();
        assert_eq!(file2.retry_count, 1);
        q.mark_error(&jid, file2.row_id, "boom", max_retries).unwrap();
        // retry_count=2, still pending (< max)
        let file3 = q.claim_batch(&jid, 1).unwrap().into_iter().next().unwrap();
        assert_eq!(file3.retry_count, 2);
        q.mark_error(&jid, file3.row_id, "final", max_retries).unwrap();
        // retry_count=3, > max → error
        let job = q.get_job(&jid).unwrap().unwrap();
        assert_eq!(job.error_files, 1);
        assert_eq!(q.pending_count(&jid).unwrap(), 0);
    }

    #[test]
    fn reclaim_in_progress_on_restart() {
        let (q, _dir) = tmp_queue();
        let jid = q.create_job("ingest", &[], 3, None).unwrap();
        q.add_files(&jid, &sample_files(&jid, 4)).unwrap();
        let _ = q.claim_batch(&jid, 4).unwrap(); // all go in_progress

        // Simulate restart: reclaim_in_progress resets them all to pending
        let reclaimed = q.reclaim_in_progress(&jid).unwrap();
        assert_eq!(reclaimed, 4);
        assert_eq!(q.pending_count(&jid).unwrap(), 4);
    }

    #[test]
    fn delete_job_removes_file_queue() {
        let (q, _dir) = tmp_queue();
        let jid = q.create_job("ingest", &[], 3, None).unwrap();
        q.add_files(&jid, &sample_files(&jid, 2)).unwrap();
        q.delete_job(&jid).unwrap();
        assert!(q.get_job(&jid).unwrap().is_none());
    }
}
