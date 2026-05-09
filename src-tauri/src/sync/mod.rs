//! P11 Pillar 6 — SyncManager: local ↔ remote sync outbox.
//!
//! In Hybrid mode, writes that would normally go directly to the remote
//! server are queued here first.  A background worker drains the outbox
//! when the server is reachable, with exponential back-off on failure.
//!
//! The outbox lives in `{data_dir}/sync_outbox.db` (WAL SQLite).
//!
//! # Operations
//!
//! | op       | payload                                      |
//! |----------|----------------------------------------------|
//! | `ingest` | `{ chunks: Vec<IngestChunk> }` (JSON)        |
//! | `delete` | `{ doc_id: String }`                         |
//! | `move`   | `{ doc_id: String, new_uri: String }`        |
//!
//! # Sync state
//!
//! A single row in `sync_state` tracks `last_pull_ts` and `last_push_ts`
//! so the UI chip can show "synced 2 min ago" or "3 pending".

pub mod tauri_commands;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Public types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id:         i64,
    pub op:         String,
    pub payload:    String,
    pub retries:    i32,
    pub last_err:   Option<String>,
    pub queued_at:  i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub pending_count:  usize,
    pub last_push_ts:   Option<i64>,
    pub last_pull_ts:   Option<i64>,
    pub remote_online:  bool,
}

// ── SyncManager ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SyncManager {
    conn: Arc<Mutex<Connection>>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sync_outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    op          TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    retries     INTEGER NOT NULL DEFAULT 0,
    last_err    TEXT,
    queued_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

impl SyncManager {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("sync_outbox.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening sync outbox at {}", db_path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Enqueue an operation for deferred push to the remote server.
    pub fn enqueue(&self, op: &str, payload: &str) -> Result<i64> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sync_outbox (op, payload, queued_at) VALUES (?1, ?2, ?3)",
            params![op, payload, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Number of entries still pending.
    pub fn pending_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_outbox WHERE retries < 10",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Claim the next batch of entries for pushing.
    pub fn claim_batch(&self, limit: usize) -> Result<Vec<OutboxEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, op, payload, retries, last_err, queued_at
             FROM sync_outbox WHERE retries < 10
             ORDER BY queued_at ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(OutboxEntry {
                id:        r.get(0)?,
                op:        r.get(1)?,
                payload:   r.get(2)?,
                retries:   r.get(3)?,
                last_err:  r.get(4)?,
                queued_at: r.get(5)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
        Ok(rows)
    }

    /// Mark an entry as successfully pushed (delete it).
    pub fn mark_done(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap()
            .execute("DELETE FROM sync_outbox WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Record a push failure (increment retries, store error).
    pub fn mark_error(&self, id: i64, err: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE sync_outbox SET retries = retries + 1, last_err = ?1 WHERE id = ?2",
            params![err, id],
        )?;
        Ok(())
    }

    pub fn get_state(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(|e| e.into())
    }

    pub fn set_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO sync_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Check if the remote server is reachable (quick GET /health).
    pub async fn is_remote_online(remote_url: &str) -> bool {
        let url = format!("{}/health", remote_url.trim_end_matches('/'));
        reqwest::Client::new()
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Push all pending entries to the remote server.
    /// Returns (pushed, failed) counts.
    pub async fn push_pending(
        &self,
        remote_url: &str,
        api_key: &str,
    ) -> Result<(usize, usize)> {
        let batch = self.claim_batch(64)?;
        if batch.is_empty() { return Ok((0, 0)); }

        let client = reqwest::Client::new();
        let mut pushed = 0;
        let mut failed = 0;

        for entry in &batch {
            let endpoint = match entry.op.as_str() {
                "ingest" => format!("{}/v1/ingest/batch", remote_url.trim_end_matches('/')),
                "delete" => format!("{}/v1/docs", remote_url.trim_end_matches('/')),
                "move"   => format!("{}/v1/docs/location", remote_url.trim_end_matches('/')),
                _        => { self.mark_error(entry.id, "unknown op").ok(); failed += 1; continue; }
            };

            let payload: serde_json::Value = match serde_json::from_str(&entry.payload) {
                Ok(v) => v,
                Err(e) => { self.mark_error(entry.id, &e.to_string()).ok(); failed += 1; continue; }
            };

            let mut req = client.post(&endpoint).json(&payload);
            if !api_key.is_empty() { req = req.bearer_auth(api_key); }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    self.mark_done(entry.id).ok();
                    pushed += 1;
                }
                Ok(resp) => {
                    let msg = format!("HTTP {}", resp.status());
                    self.mark_error(entry.id, &msg).ok();
                    failed += 1;
                }
                Err(e) => {
                    self.mark_error(entry.id, &e.to_string()).ok();
                    failed += 1;
                }
            }
        }

        if pushed > 0 {
            self.set_state("last_push_ts", &now_ms().to_string()).ok();
        }
        Ok((pushed, failed))
    }

    pub fn clear_failed(&self) -> Result<usize> {
        let n = self.conn.lock().unwrap()
            .execute("DELETE FROM sync_outbox WHERE retries >= 10", [])?;
        Ok(n)
    }

    pub fn status(&self, remote_url: Option<&str>) -> SyncStatus {
        let pending_count = self.pending_count().unwrap_or(0);
        let last_push_ts = self.get_state("last_push_ts").ok().flatten()
            .and_then(|s| s.parse::<i64>().ok());
        let last_pull_ts = self.get_state("last_pull_ts").ok().flatten()
            .and_then(|s| s.parse::<i64>().ok());
        SyncStatus {
            pending_count, last_push_ts, last_pull_ts,
            remote_online: false, // updated async by the caller
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, SyncManager) {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = SyncManager::open(tmp.path()).unwrap();
        (tmp, mgr)
    }

    #[test]
    fn open_creates_db_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        // Two opens of the same dir must succeed and share state.
        let m1 = SyncManager::open(tmp.path()).unwrap();
        m1.enqueue("ingest", r#"{"x":1}"#).unwrap();
        drop(m1);
        let m2 = SyncManager::open(tmp.path()).unwrap();
        assert_eq!(m2.pending_count().unwrap(), 1);
    }

    #[test]
    fn enqueue_returns_increasing_ids() {
        let (_tmp, mgr) = fresh();
        let id1 = mgr.enqueue("ingest", "{}").unwrap();
        let id2 = mgr.enqueue("delete", "{}").unwrap();
        let id3 = mgr.enqueue("move",   "{}").unwrap();
        assert!(id1 < id2 && id2 < id3, "rowids must be monotonic");
    }

    #[test]
    fn pending_count_excludes_permanent_failures() {
        let (_tmp, mgr) = fresh();
        let id = mgr.enqueue("ingest", "{}").unwrap();
        assert_eq!(mgr.pending_count().unwrap(), 1);
        // Bump retries to 10 — should drop out of pending_count.
        for _ in 0..10 { mgr.mark_error(id, "fail").unwrap(); }
        assert_eq!(mgr.pending_count().unwrap(), 0);
    }

    #[test]
    fn claim_batch_respects_limit_and_order() {
        let (_tmp, mgr) = fresh();
        for i in 0..5 {
            mgr.enqueue("ingest", &format!(r#"{{"i":{i}}}"#)).unwrap();
        }
        let batch = mgr.claim_batch(3).unwrap();
        assert_eq!(batch.len(), 3);
        // Oldest first (FIFO).
        assert!(batch[0].id < batch[1].id);
        assert!(batch[1].id < batch[2].id);
        assert_eq!(batch[0].op, "ingest");
        assert!(batch[0].payload.contains("\"i\":0"));
    }

    #[test]
    fn mark_done_removes_entry() {
        let (_tmp, mgr) = fresh();
        let id = mgr.enqueue("ingest", "{}").unwrap();
        mgr.mark_done(id).unwrap();
        assert_eq!(mgr.pending_count().unwrap(), 0);
        assert_eq!(mgr.claim_batch(10).unwrap().len(), 0);
    }

    #[test]
    fn mark_error_increments_retries_and_records_msg() {
        let (_tmp, mgr) = fresh();
        let id = mgr.enqueue("delete", r#"{"doc_id":"x"}"#).unwrap();
        mgr.mark_error(id, "HTTP 503").unwrap();
        let entries = mgr.claim_batch(10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].retries, 1);
        assert_eq!(entries[0].last_err.as_deref(), Some("HTTP 503"));
        // Second failure increments again.
        mgr.mark_error(id, "HTTP 502").unwrap();
        let entries = mgr.claim_batch(10).unwrap();
        assert_eq!(entries[0].retries, 2);
        assert_eq!(entries[0].last_err.as_deref(), Some("HTTP 502"));
    }

    #[test]
    fn clear_failed_removes_only_max_retried() {
        let (_tmp, mgr) = fresh();
        let id_ok    = mgr.enqueue("ingest", "{}").unwrap();
        let id_fail  = mgr.enqueue("delete", "{}").unwrap();
        for _ in 0..10 { mgr.mark_error(id_fail, "boom").unwrap(); }
        let cleared = mgr.clear_failed().unwrap();
        assert_eq!(cleared, 1);
        // The healthy entry is still there.
        let remaining = mgr.claim_batch(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, id_ok);
    }

    #[test]
    fn state_kv_round_trip() {
        let (_tmp, mgr) = fresh();
        assert_eq!(mgr.get_state("nope").unwrap(), None);
        mgr.set_state("last_pull_ts", "1234").unwrap();
        assert_eq!(mgr.get_state("last_pull_ts").unwrap().as_deref(), Some("1234"));
        // Replace.
        mgr.set_state("last_pull_ts", "5678").unwrap();
        assert_eq!(mgr.get_state("last_pull_ts").unwrap().as_deref(), Some("5678"));
    }

    #[test]
    fn status_reports_pending_count_correctly() {
        let (_tmp, mgr) = fresh();
        mgr.enqueue("ingest", "{}").unwrap();
        mgr.enqueue("ingest", "{}").unwrap();
        let s = mgr.status(None);
        assert_eq!(s.pending_count, 2);
        assert!(!s.remote_online); // no async ping done in status() itself
        assert_eq!(s.last_pull_ts, None);
    }

    #[test]
    fn outbox_entry_payload_preserved() {
        let (_tmp, mgr) = fresh();
        let payload = r#"{"chunks":[{"doc_id":"a"}]}"#;
        mgr.enqueue("ingest", payload).unwrap();
        let entries = mgr.claim_batch(1).unwrap();
        assert_eq!(entries[0].payload, payload);
    }
}
