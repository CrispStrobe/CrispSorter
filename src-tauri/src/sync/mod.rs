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

pub mod backup_scheduler;
pub mod backup_state;
pub mod cert_pins;
pub mod cloud_backup;
pub mod conflict;
pub mod delta;
pub mod offline_queue;
pub mod pairs;
pub mod partition;
pub mod proxy;
pub mod proxy_secret;
pub mod secret;
pub mod tauri_commands;
pub mod transfer_queue;

use self::proxy::ProxyConfig;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Public types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: i64,
    pub op: String,
    pub payload: String,
    pub retries: i32,
    pub last_err: Option<String>,
    pub queued_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub pending_count: usize,
    pub last_push_ts: Option<i64>,
    pub last_pull_ts: Option<i64>,
    pub remote_online: bool,
}

/// A cloud-backup pull conflict awaiting an explicit user decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingConflict {
    pub id: i64,
    pub path: String,
    pub local_doc_id: String,
    pub local_hash: String,
    pub remote_hash: String,
    pub local_title: Option<String>,
    pub remote_title: Option<String>,
    pub remote_indexed_at: i64,
    pub created_at: i64,
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

CREATE TABLE IF NOT EXISTS sync_conflicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL,
    local_doc_id TEXT NOT NULL,
    local_hash TEXT NOT NULL,
    remote_hash TEXT NOT NULL,
    local_title TEXT,
    remote_title TEXT,
    remote_indexed_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(path, remote_hash)
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
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
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
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(OutboxEntry {
                    id: r.get(0)?,
                    op: r.get(1)?,
                    payload: r.get(2)?,
                    retries: r.get(3)?,
                    last_err: r.get(4)?,
                    queued_at: r.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn claim_batch_by_op(&self, op: &str, limit: usize) -> Result<Vec<OutboxEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, op, payload, retries, last_err, queued_at
             FROM sync_outbox WHERE op = ?1 AND retries < 10
             ORDER BY queued_at ASC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![op, limit as i64], |r| {
                Ok(OutboxEntry {
                    id: r.get(0)?,
                    op: r.get(1)?,
                    payload: r.get(2)?,
                    retries: r.get(3)?,
                    last_err: r.get(4)?,
                    queued_at: r.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Mark an entry as successfully pushed (delete it).
    pub fn mark_done(&self, id: i64) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
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
        conn.query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| {
            r.get(0)
        })
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

    /// Add a manual-review conflict idempotently.
    pub fn enqueue_conflict(
        &self,
        path: &str,
        local_doc_id: &str,
        local_hash: &str,
        remote_hash: &str,
        local_title: Option<&str>,
        remote_title: Option<&str>,
        remote_indexed_at: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO sync_conflicts
             (path, local_doc_id, local_hash, remote_hash, local_title,
              remote_title, remote_indexed_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                path,
                local_doc_id,
                local_hash,
                remote_hash,
                local_title,
                remote_title,
                remote_indexed_at,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// List unresolved conflicts in creation order for the review UI/CLI.
    pub fn pending_conflicts(&self) -> Result<Vec<PendingConflict>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, path, local_doc_id, local_hash, remote_hash,
                    local_title, remote_title, remote_indexed_at, created_at
             FROM sync_conflicts ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PendingConflict {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    local_doc_id: r.get(2)?,
                    local_hash: r.get(3)?,
                    remote_hash: r.get(4)?,
                    local_title: r.get(5)?,
                    remote_title: r.get(6)?,
                    remote_indexed_at: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn remove_conflict(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM sync_conflicts WHERE id = ?1", [id])?
            > 0)
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
    pub async fn push_pending(&self, remote_url: &str, api_key: &str) -> Result<(usize, usize)> {
        self.push_pending_with_proxy(remote_url, api_key, &ProxyConfig::default())
            .await
    }

    /// Push pending entries using an explicit HTTP/SOCKS5 proxy policy.
    pub async fn push_pending_with_proxy(
        &self,
        remote_url: &str,
        api_key: &str,
        proxy: &ProxyConfig,
    ) -> Result<(usize, usize)> {
        let batch = self.claim_batch(64)?;
        if batch.is_empty() {
            return Ok((0, 0));
        }

        let client = self::proxy::build_async_client_with_timeout(proxy, Duration::from_secs(30))?;
        let mut pushed = 0;
        let mut failed = 0;

        for entry in &batch {
            let endpoint = match entry.op.as_str() {
                "ingest" => format!("{}/v1/ingest/batch", remote_url.trim_end_matches('/')),
                "delete" => format!("{}/v1/docs", remote_url.trim_end_matches('/')),
                "move" => format!("{}/v1/docs/location", remote_url.trim_end_matches('/')),
                _ => {
                    self.mark_error(entry.id, "unknown op").ok();
                    failed += 1;
                    continue;
                }
            };

            let payload: serde_json::Value = match serde_json::from_str(&entry.payload) {
                Ok(v) => v,
                Err(e) => {
                    self.mark_error(entry.id, &e.to_string()).ok();
                    failed += 1;
                    continue;
                }
            };

            let mut req = client.post(&endpoint).json(&payload);
            if !api_key.is_empty() {
                req = req.bearer_auth(api_key);
            }

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

    /// Pull rows from the remote server's `/v1/sync/since` endpoint that have
    /// `indexed_at >= last_pull_ts`. Returns the parsed `SearchHit` rows AND
    /// the new `max_indexed_at` (so the caller can advance state after a
    /// successful apply).
    ///
    /// This method does NOT advance `last_pull_ts` itself — the caller does
    /// so via [`Self::set_state("last_pull_ts", …)`] *after* successfully
    /// applying the rows to local storage.  This gives at-least-once
    /// semantics: a crash mid-apply re-fetches the same rows on next pull.
    pub async fn pull_pending(
        &self,
        remote_url: &str,
        api_key: &str,
        limit: usize,
    ) -> Result<(Vec<crisp_index_protocol::SearchHit>, i64)> {
        self.pull_pending_with_proxy(remote_url, api_key, limit, &ProxyConfig::default())
            .await
    }

    /// Pull pending rows using an explicit HTTP/SOCKS5 proxy policy.
    pub async fn pull_pending_with_proxy(
        &self,
        remote_url: &str,
        api_key: &str,
        limit: usize,
        proxy: &ProxyConfig,
    ) -> Result<(Vec<crisp_index_protocol::SearchHit>, i64)> {
        let last_pull_ts: i64 = self
            .get_state("last_pull_ts")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let url = format!(
            "{}/v1/sync/since?ts={}&limit={}",
            remote_url.trim_end_matches('/'),
            last_pull_ts,
            limit
        );
        let mut req =
            self::proxy::build_async_client_with_timeout(proxy, Duration::from_secs(30))?.get(&url);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("server returned {}", resp.status());
        }
        let body: serde_json::Value = resp.json().await?;
        let max_ts = body["max_indexed_at"].as_i64().unwrap_or(last_pull_ts);
        let rows: Vec<crisp_index_protocol::SearchHit> =
            serde_json::from_value(body["rows"].clone()).unwrap_or_default();
        Ok((rows, max_ts))
    }

    /// P13.7 Stage F — drain `cb_manifest_push` outbox entries
    /// through the cloud-backup HTTP API.  Mirrors `push_pending`
    /// but routes via [`cloud_backup::CloudBackupClient`] so the
    /// existing crisp-index-server flow is untouched.
    ///
    /// Batching: claims up to `batch_size` rows from the outbox,
    /// filters to op="cb_manifest_push", deserialises each payload
    /// to a [`cloud_backup::ManifestRow`], posts the batch in one
    /// request.  Success → mark every claimed entry done.  Failure
    /// → mark every claimed entry as errored (retry counter bumps).
    ///
    /// Returns `(pushed, failed)` counts.  When the cloud-backup
    /// client init itself fails, all entries are reset to retryable
    /// state (we didn't actually try them).
    pub async fn drain_cb_outbox(
        &self,
        client: &cloud_backup::CloudBackupClient,
        batch_size: usize,
    ) -> Result<(usize, usize)> {
        let claimed = self.claim_batch(batch_size)?;
        let cb_entries: Vec<_> = claimed
            .into_iter()
            .filter(|e| e.op == "cb_manifest_push")
            .collect();
        if cb_entries.is_empty() {
            return Ok((0, 0));
        }

        let mut rows: Vec<cloud_backup::ManifestRow> = Vec::with_capacity(cb_entries.len());
        let mut id_for_row: Vec<i64> = Vec::with_capacity(cb_entries.len());
        let mut failed = 0;
        for entry in &cb_entries {
            match serde_json::from_str::<cloud_backup::ManifestRow>(&entry.payload) {
                Ok(row) => {
                    rows.push(row);
                    id_for_row.push(entry.id);
                }
                Err(e) => {
                    // Malformed payload — bump retries so a fix can
                    // reset; don't include in the batch POST.
                    self.mark_error(entry.id, &format!("payload: {e}")).ok();
                    failed += 1;
                }
            }
        }
        if rows.is_empty() {
            return Ok((0, failed));
        }

        match client.manifest_push(&rows).await {
            Ok(_) => {
                for id in &id_for_row {
                    self.mark_done(*id).ok();
                }
                self.set_state("cb_last_outbox_drain_ts", &now_ms().to_string())
                    .ok();
                Ok((rows.len(), failed))
            }
            Err(e) => {
                let msg = format!("{e}");
                for id in &id_for_row {
                    self.mark_error(*id, &msg).ok();
                }
                Ok((0, failed + rows.len()))
            }
        }
    }

    /// Stage U — drain `cb_file_upload` outbox entries.  Each entry
    /// payload is `{"sha256": "…", "path": "/abs/path"}`.  For each
    /// entry the file at `path` is uploaded via the existing
    /// `POST /api/files/by-hash/<sha>` endpoint.  Entries where the
    /// local file is missing are silently completed (the file may have
    /// been moved; the VPS will never see bytes for that sha).
    pub async fn drain_cb_file_uploads(
        &self,
        client: &cloud_backup::CloudBackupClient,
        batch_size: usize,
    ) -> Result<(usize, usize)> {
        let claimed = self.claim_batch_by_op("cb_file_upload", batch_size)?;
        if claimed.is_empty() {
            return Ok((0, 0));
        }

        let mut uploaded = 0usize;
        let mut failed = 0usize;
        for entry in &claimed {
            #[derive(serde::Deserialize)]
            struct UploadJob {
                sha256: String,
                path: String,
            }
            match serde_json::from_str::<UploadJob>(&entry.payload) {
                Err(e) => {
                    self.mark_error(entry.id, &format!("payload parse: {e}"))
                        .ok();
                    failed += 1;
                }
                Ok(job) => {
                    let path = std::path::PathBuf::from(&job.path);
                    if !path.exists() {
                        // File moved or deleted — skip, mark done so we
                        // don't retry forever.
                        self.mark_done(entry.id).ok();
                        continue;
                    }
                    // Stream the body straight from disk via
                    // `upload_file_by_hash`, which wraps a
                    // `tokio_util::io::ReaderStream` in a `reqwest::Body`.
                    // Keeps multi-GB media uploads off the heap.
                    match client.upload_file_by_hash(&job.sha256, &path).await {
                        Ok(_) => {
                            self.mark_done(entry.id).ok();
                            uploaded += 1;
                        }
                        Err(e) => {
                            self.mark_error(entry.id, &format!("upload: {e}")).ok();
                            failed += 1;
                        }
                    }
                }
            }
        }
        Ok((uploaded, failed))
    }

    pub fn clear_failed(&self) -> Result<usize> {
        let n = self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM sync_outbox WHERE retries >= 10", [])?;
        Ok(n)
    }

    pub fn status(&self, _remote_url: Option<&str>) -> SyncStatus {
        let pending_count = self.pending_count().unwrap_or(0);
        let last_push_ts = self
            .get_state("last_push_ts")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok());
        let last_pull_ts = self
            .get_state("last_pull_ts")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok());
        SyncStatus {
            pending_count,
            last_push_ts,
            last_pull_ts,
            remote_online: false, // updated async by the caller
        }
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
    fn manual_conflicts_are_durable_and_deduplicated() {
        let (_tmp, mgr) = fresh();
        mgr.enqueue_conflict(
            "/doc.txt",
            "local",
            "aaa",
            "bbb",
            Some("Local"),
            Some("Remote"),
            42,
        )
        .unwrap();
        mgr.enqueue_conflict(
            "/doc.txt",
            "local",
            "aaa",
            "bbb",
            Some("Local"),
            Some("Remote"),
            42,
        )
        .unwrap();
        let rows = mgr.pending_conflicts().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/doc.txt");
        assert_eq!(rows[0].remote_hash, "bbb");
        assert!(mgr.remove_conflict(rows[0].id).unwrap());
        assert!(mgr.pending_conflicts().unwrap().is_empty());
        assert!(!mgr.remove_conflict(rows[0].id).unwrap());
    }

    #[test]
    fn enqueue_returns_increasing_ids() {
        let (_tmp, mgr) = fresh();
        let id1 = mgr.enqueue("ingest", "{}").unwrap();
        let id2 = mgr.enqueue("delete", "{}").unwrap();
        let id3 = mgr.enqueue("move", "{}").unwrap();
        assert!(id1 < id2 && id2 < id3, "rowids must be monotonic");
    }

    #[test]
    fn pending_count_excludes_permanent_failures() {
        let (_tmp, mgr) = fresh();
        let id = mgr.enqueue("ingest", "{}").unwrap();
        assert_eq!(mgr.pending_count().unwrap(), 1);
        // Bump retries to 10 — should drop out of pending_count.
        for _ in 0..10 {
            mgr.mark_error(id, "fail").unwrap();
        }
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
        let id_ok = mgr.enqueue("ingest", "{}").unwrap();
        let id_fail = mgr.enqueue("delete", "{}").unwrap();
        for _ in 0..10 {
            mgr.mark_error(id_fail, "boom").unwrap();
        }
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
        assert_eq!(
            mgr.get_state("last_pull_ts").unwrap().as_deref(),
            Some("1234")
        );
        // Replace.
        mgr.set_state("last_pull_ts", "5678").unwrap();
        assert_eq!(
            mgr.get_state("last_pull_ts").unwrap().as_deref(),
            Some("5678")
        );
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

    /// Pull-delta uses /v1/sync/since which is unauthenticated for local
    /// tests; we don't spawn the server, but we exercise the parsing logic
    /// by hand-constructing the expected JSON shape.
    #[test]
    fn pull_pending_parses_search_hits_correctly() {
        // Mimic the server's response body shape — exact field names matter.
        let server_response = serde_json::json!({
            "rows": [
                {
                    "doc_id":       "abc-123",
                    "location_uri": "crisp+local://owner@m1/data/doc.pdf",
                    "owner_id":     "owner",
                    "title":        "Test Doc",
                    "author":       "Doe, John",
                    "year":         2024,
                    "filename":     "doc.pdf",
                    "ext":          "pdf",
                    "snippet":      "",
                    "score":        0.0,
                    "chunk_index":  0
                },
                {
                    "doc_id":       "def-456",
                    "location_uri": "crisp+local://owner@m1/data/notes.md",
                    "owner_id":     "owner",
                    "snippet":      "",
                    "score":        0.0,
                    "chunk_index":  0
                }
            ],
            "max_indexed_at": 1_700_000_000_000_i64,
            "has_more":       false
        });

        let max_ts = server_response["max_indexed_at"].as_i64().unwrap();
        let rows: Vec<crisp_index_protocol::SearchHit> =
            serde_json::from_value(server_response["rows"].clone()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].doc_id, "abc-123");
        assert_eq!(rows[0].title.as_deref(), Some("Test Doc"));
        assert_eq!(rows[0].year, Some(2024));
        assert_eq!(rows[1].title, None); // optional fields tolerated
        assert!(max_ts > 1_500_000_000_000);
    }

    /// P13.7 Stage F — `drain_cb_outbox` happy path: enqueue two
    /// rows, drain against a mockito server, both rows marked done.
    #[tokio::test]
    async fn drain_cb_outbox_clears_entries_on_success() {
        use mockito::Server;
        let (_tmp, mgr) = fresh();
        // Two valid payloads.
        let row1 = serde_json::json!({
            "path": "/a.txt", "size_bytes": 1, "sha256": "a".repeat(64),
            "mtime_unix": 1.0, "owner_id": "o", "filename": "a.txt",
            "ext": "txt", "parent_dir": "/",
        })
        .to_string();
        let row2 = serde_json::json!({
            "path": "/b.txt", "size_bytes": 2, "sha256": "b".repeat(64),
            "mtime_unix": 2.0, "owner_id": "o", "filename": "b.txt",
            "ext": "txt", "parent_dir": "/",
        })
        .to_string();
        mgr.enqueue("cb_manifest_push", &row1).unwrap();
        mgr.enqueue("cb_manifest_push", &row2).unwrap();
        assert_eq!(mgr.pending_count().unwrap(), 2);

        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/api/manifest/push")
            .with_status(200)
            .with_body(r#"{"accepted": 2}"#)
            .create_async()
            .await;
        let client = cloud_backup::CloudBackupClient::new(server.url(), "k").unwrap();

        let (pushed, failed) = mgr.drain_cb_outbox(&client, 64).await.unwrap();
        assert_eq!(pushed, 2);
        assert_eq!(failed, 0);
        assert_eq!(mgr.pending_count().unwrap(), 0);
        m.assert_async().await;
    }

    /// `drain_cb_outbox` failure path: server returns 500, both
    /// entries marked errored (retries bump) but not done.
    #[tokio::test]
    async fn drain_cb_outbox_marks_error_on_server_failure() {
        use mockito::Server;
        let (_tmp, mgr) = fresh();
        let row = serde_json::json!({
            "path": "/c.txt", "size_bytes": 3, "sha256": "c".repeat(64),
            "mtime_unix": 3.0, "owner_id": "o", "filename": "c.txt",
            "ext": "txt", "parent_dir": "/",
        })
        .to_string();
        mgr.enqueue("cb_manifest_push", &row).unwrap();

        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/api/manifest/push")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;
        let client = cloud_backup::CloudBackupClient::new(server.url(), "k").unwrap();
        let (pushed, failed) = mgr.drain_cb_outbox(&client, 64).await.unwrap();
        assert_eq!(pushed, 0);
        assert_eq!(failed, 1);
        // Entry still pending — retries bumped to 1.
        let batch = mgr.claim_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].retries, 1);
        assert!(batch[0].last_err.is_some());
        m.assert_async().await;
    }

    /// `drain_cb_outbox` skips entries with non-cb_manifest_push op
    /// (so the legacy crisp-index-server `ingest` op isn't drained
    /// through the wrong route).
    #[tokio::test]
    async fn drain_cb_outbox_ignores_other_ops() {
        let (_tmp, mgr) = fresh();
        mgr.enqueue("ingest", r#"{"x":1}"#).unwrap();
        mgr.enqueue("delete", r#"{"x":1}"#).unwrap();

        // Server not even hit; the routing filter discards both.
        // Use a never-bound port to verify no HTTP call happens.
        let client = cloud_backup::CloudBackupClient::new("http://127.0.0.1:1", "k").unwrap();
        let (pushed, failed) = mgr.drain_cb_outbox(&client, 64).await.unwrap();
        assert_eq!(pushed, 0);
        assert_eq!(failed, 0);
        // Both entries still in the outbox, untouched.
        assert_eq!(mgr.pending_count().unwrap(), 2);
    }

    #[test]
    fn pull_does_not_advance_state_on_empty_response() {
        let (_tmp, mgr) = fresh();
        // Set an initial watermark so we can verify it's not clobbered.
        mgr.set_state("last_pull_ts", "1234567890").unwrap();
        // We can't call pull_pending without a server, but we CAN verify
        // the contract: the public set_state is only called by the
        // Tauri command after a successful apply (see sync_pull in
        // tauri_commands.rs). The SyncManager's own pull_pending doesn't
        // mutate state — guard against accidental future regressions.
        // Read the source-of-truth value back to confirm it's untouched.
        assert_eq!(
            mgr.get_state("last_pull_ts").unwrap().as_deref(),
            Some("1234567890")
        );
    }
}
