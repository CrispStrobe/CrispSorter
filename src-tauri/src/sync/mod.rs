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
