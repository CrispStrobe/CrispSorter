//! Durable scheduler receipts. Payloads contain source metadata and the OCR
//! configuration only; credentials and service connections stay in app state.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::path::Path;

pub(super) struct QueueStore {
    connection: Connection,
    // Held for this scheduler's lifetime. Another process must not reclaim a
    // live worker's receipt as though the first process had crashed.
    _lease: Connection,
}

pub(super) struct StoredEntry {
    pub key: String,
    pub payload: String,
    pub status: String,
    pub error: Option<String>,
}

pub(super) struct RecoveredQueue {
    pub entries: Vec<StoredEntry>,
    pub done: usize,
    pub last_error: Option<String>,
    pub settings: Option<String>,
}

impl QueueStore {
    /// Inspect persisted policy without acquiring the scheduler lease, creating
    /// storage, migrating schemas or recovering another process's active work.
    pub fn read_settings(data_dir: &Path) -> Result<Option<String>> {
        let path = data_dir.join("background-ingest.sqlite");
        if !path.try_exists().context("Check saved OCR settings")? { return Ok(None); }
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Open saved OCR settings read-only")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.query_row("SELECT value FROM background_queue_meta WHERE key = 'settings'", [], |row| row.get(0))
            .optional().context("Read saved OCR settings")
    }

    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let lease = Connection::open(data_dir.join("background-ingest-lock.sqlite"))?;
        lease.busy_timeout(std::time::Duration::from_millis(100))?;
        lease.execute_batch("BEGIN IMMEDIATE")
            .context("Another background scheduler is using this data directory")?;
        let connection = Connection::open(data_dir.join("background-ingest.sqlite"))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS background_queue (
            source_key TEXT PRIMARY KEY,
            payload TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('pending','in_progress','error')),
            followup_payload TEXT,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS background_queue_status ON background_queue(status);
        CREATE TABLE IF NOT EXISTS background_queue_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        Ok(Self { connection, _lease: lease })
    }

    /// Only call when this process has no live workers. The newest request for
    /// an interrupted source takes precedence over its older in-flight request.
    pub fn recover(&mut self) -> Result<RecoveredQueue> {
        self.connection.execute("UPDATE background_queue SET
            payload = COALESCE(followup_payload, payload), followup_payload = NULL,
            status = 'pending' WHERE status = 'in_progress'", [])?;
        let entries = self.connection.prepare(
            "SELECT source_key,payload,status,error FROM background_queue ORDER BY rowid")?
            .query_map([], |row| Ok(StoredEntry { key: row.get(0)?, payload: row.get(1)?, status: row.get(2)?, error: row.get(3)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let get = |key: &str| -> Result<Option<String>> {
            Ok(self.connection.query_row("SELECT value FROM background_queue_meta WHERE key = ?1", [key], |row| row.get(0)).optional()?)
        };
        let done = get("done")?.map(|value| value.parse::<usize>()).transpose()
            .context("Invalid saved background progress")?.unwrap_or(0);
        Ok(RecoveredQueue { entries, done, last_error: get("last_error")?, settings: get("settings")? })
    }

    pub fn enqueue(&mut self, items: &[(String, String)]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for (key, payload) in items {
            transaction.execute("INSERT INTO background_queue(source_key,payload,status)
                VALUES (?1,?2,'pending') ON CONFLICT(source_key) DO UPDATE SET
                followup_payload = CASE WHEN background_queue.status = 'in_progress' THEN excluded.payload ELSE NULL END,
                payload = CASE WHEN background_queue.status = 'in_progress' THEN background_queue.payload ELSE excluded.payload END,
                status = CASE WHEN background_queue.status = 'in_progress' THEN 'in_progress' ELSE 'pending' END,
                error = NULL", params![key, payload])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn claim(&self, key: &str) -> Result<()> {
        let changed = self.connection.execute("UPDATE background_queue SET status = 'in_progress'
            WHERE source_key = ?1 AND status = 'pending'", [key])?;
        anyhow::ensure!(changed == 1, "Background source is no longer pending; reload the queue before continuing");
        Ok(())
    }

    /// Return a coalesced follow-up request, if any. The successful receipt and
    /// counter advance atomically; an unsuccessful receipt stays retryable.
    pub fn complete(&mut self, key: &str, error: Option<&str>) -> Result<Option<String>> {
        let transaction = self.connection.transaction()?;
        let current: Option<(String, Option<String>)> = transaction.query_row(
            "SELECT status,followup_payload FROM background_queue WHERE source_key = ?1", [key],
            |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let (status, next) = current.ok_or_else(|| anyhow::anyhow!("Background source receipt was not found"))?;
        anyhow::ensure!(status == "in_progress", "Background source receipt is not in progress");
        if let Some(ref payload) = next {
            transaction.execute("UPDATE background_queue SET payload = ?1, status = 'pending', followup_payload = NULL, error = NULL WHERE source_key = ?2", params![payload, key])?;
        } else if let Some(error) = error {
            transaction.execute("UPDATE background_queue SET status = 'error', error = ?1 WHERE source_key = ?2", params![error, key])?;
        } else {
            transaction.execute("DELETE FROM background_queue WHERE source_key = ?1", [key])?;
        }
        if let Some(error) = error {
            transaction.execute("INSERT INTO background_queue_meta(key,value) VALUES ('last_error',?1)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value", [error])?;
        } else {
            transaction.execute("INSERT INTO background_queue_meta(key,value) VALUES ('done','1')
                ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1", [])?;
        }
        transaction.commit()?;
        Ok(next)
    }

    pub fn retry_errors(&self) -> Result<()> {
        self.connection.execute("UPDATE background_queue SET status='pending', error=NULL WHERE status='error'", [])?;
        Ok(())
    }

    pub fn save_settings(&self, payload: &str) -> Result<()> {
        self.save_meta("settings", payload)
    }

    pub fn save_status(&self, status: &str) -> Result<()> {
        self.save_meta("status", status)
    }

    fn save_meta(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute("INSERT INTO background_queue_meta(key,value) VALUES (?1,?2)
            ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM background_queue", [])?;
        transaction.execute("DELETE FROM background_queue_meta WHERE key IN ('done','last_error','status')", [])?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_read_does_not_create_storage() {
        let directory = tempfile::tempdir().unwrap();
        let absent = directory.path().join("unused-data-directory");
        assert!(QueueStore::read_settings(&absent).unwrap().is_none());
        assert!(!absent.exists());
        assert!(std::fs::read_dir(directory.path()).unwrap().next().is_none());
    }

    #[test]
    fn policy_read_preserves_live_claims_followups_and_scheduler_lease() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = QueueStore::open(directory.path()).unwrap();
        let settings = r#"{"ocr_enabled":true,"ocr_tier":"tier4"}"#;
        store.save_settings(settings).unwrap();
        store.enqueue(&[("active".into(), "original request".into())]).unwrap();
        store.claim("active").unwrap();
        store.enqueue(&[("active".into(), "follow-up request".into())]).unwrap();

        assert_eq!(QueueStore::read_settings(directory.path()).unwrap().as_deref(), Some(settings));
        let (status, payload, followup): (String, String, Option<String>) = store.connection.query_row(
            "SELECT status,payload,followup_payload FROM background_queue WHERE source_key='active'", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!(status, "in_progress");
        assert_eq!(payload, "original request");
        assert_eq!(followup.as_deref(), Some("follow-up request"));
        assert!(QueueStore::open(directory.path()).is_err(), "read-only inspection released the live lease");
    }

    #[test]
    fn reopening_recovers_inflight_and_latest_followup_without_replaying_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = QueueStore::open(directory.path()).unwrap();
        store.save_settings(r#"{"ocr_enabled":true,"ocr_tier":"tier2"}"#).unwrap();
        store.enqueue(&[("done".into(), "completed request".into()), ("active".into(), "older request".into()), ("failed".into(), "failed request".into())]).unwrap();
        store.claim("done").unwrap();
        store.complete("done", None).unwrap();
        store.claim("failed").unwrap();
        store.complete("failed", Some("extractor unavailable")).unwrap();
        store.claim("active").unwrap();
        store.enqueue(&[("active".into(), "newer request".into())]).unwrap();
        drop(store);

        let mut reopened = QueueStore::open(directory.path()).unwrap();
        let recovery = reopened.recover().unwrap();
        assert_eq!(recovery.done, 1);
        assert_eq!(recovery.entries.len(), 2);
        assert_eq!(recovery.entries[0].payload, "newer request");
        assert_eq!(recovery.entries[0].status, "pending");
        assert_eq!(recovery.entries[1].status, "error");
        assert_eq!(recovery.entries[1].error.as_deref(), Some("extractor unavailable"));
        assert_eq!(recovery.settings.as_deref(), Some(r#"{"ocr_enabled":true,"ocr_tier":"tier2"}"#));
        reopened.retry_errors().unwrap();
        assert!(reopened.recover().unwrap().entries.iter().all(|row| row.status == "pending"));
    }

    #[test]
    fn enqueue_and_acknowledgement_rollback_on_storage_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = QueueStore::open(directory.path()).unwrap();
        store.connection.execute_batch("CREATE TRIGGER reject_source BEFORE INSERT ON background_queue WHEN NEW.source_key = 'bad' BEGIN SELECT RAISE(ABORT, 'disk write failure'); END;").unwrap();
        assert!(store.enqueue(&[("good".into(), "one".into()), ("bad".into(), "two".into())]).is_err());
        assert!(store.recover().unwrap().entries.is_empty());
        store.connection.execute_batch("DROP TRIGGER reject_source;").unwrap();
        store.enqueue(&[("good".into(), "one".into())]).unwrap();
        store.claim("good").unwrap();
        store.connection.execute_batch("CREATE TRIGGER reject_ack BEFORE INSERT ON background_queue_meta WHEN NEW.key = 'done' BEGIN SELECT RAISE(ABORT, 'ack failure'); END;").unwrap();
        assert!(store.complete("good", None).is_err());
        let recovered = store.recover().unwrap();
        assert_eq!(recovered.done, 0);
        assert_eq!(recovered.entries.len(), 1);
        assert_eq!(recovered.entries[0].status, "pending");
    }

    #[test]
    fn same_source_is_coalesced_and_only_one_worker_can_claim_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = QueueStore::open(directory.path()).unwrap();
        store.enqueue(&[("source".into(), "first".into()), ("source".into(), "second".into())]).unwrap();
        assert_eq!(store.recover().unwrap().entries.len(), 1);
        store.claim("source").unwrap();
        assert!(store.claim("source").is_err());
        store.enqueue(&[("source".into(), "third".into()), ("source".into(), "last".into())]).unwrap();
        assert_eq!(store.complete("source", None).unwrap().as_deref(), Some("last"));
        store.claim("source").unwrap();
        store.complete("source", None).unwrap();
        assert!(store.complete("source", None).is_err());
        assert_eq!(store.recover().unwrap().done, 2);
        store.clear().unwrap();
        assert_eq!(store.recover().unwrap().done, 0);
    }

    #[test]
    fn a_second_scheduler_cannot_reclaim_a_live_process() {
        let directory = tempfile::tempdir().unwrap();
        let first = QueueStore::open(directory.path()).unwrap();
        assert!(QueueStore::open(directory.path()).is_err());
        drop(first);
        assert!(QueueStore::open(directory.path()).is_ok());
    }
}
