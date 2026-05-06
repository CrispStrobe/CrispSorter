use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crisp_index_protocol::{IngestBatch, IngestChunk, TaskStatusResponse};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tokio::time::MissedTickBehavior;

use crate::embedder::ServerEmbedder;
use crate::index::SearchIndex;

const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const LEASE_DURATION: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const CLAIM_ERROR_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct QueueConfig {
    pub lease_secs: u64,
    pub retry_base_ms: u64,
    pub max_attempts: u32,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            lease_secs: LEASE_DURATION.as_secs(),
            retry_base_ms: 1_000,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

#[derive(Clone)]
pub struct TaskQueue {
    db_path:       PathBuf,
    conn:          Arc<Mutex<Connection>>,
    lease_ms:      i64,
    retry_base_ms: i64,
    max_attempts:  u32,
}

#[derive(Debug)]
struct QueuedTask {
    id:            String,
    payload:       IngestBatch,
    total_chunks:  usize,
    attempt_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LegacyQueueFile {
    tasks: Vec<LegacyTaskRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyTaskRecord {
    id: String,
    payload_json: String,
    state: String,
    total_chunks: usize,
    completed_chunks: usize,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    #[serde(default)]
    attempt_count: u32,
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease_expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_heartbeat_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt_started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_retry_at: Option<i64>,
}

impl TaskQueue {
    pub fn open_or_create(data_dir: &Path, config: QueueConfig) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("crisp_jobs.db");
        let json_path = data_dir.join("ingest_tasks.json");

        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening queue database {}", db_path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("configuring sqlite busy timeout")?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enabling sqlite WAL mode")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("setting sqlite synchronous mode")?;
        conn.execute_batch(SCHEMA)
            .context("creating queue schema")?;
        migrate_schema_v2(&conn).context("schema v2 migration")?;

        let queue = Self {
            db_path,
            conn: Arc::new(Mutex::new(conn)),
            lease_ms: i64::try_from(Duration::from_secs(config.lease_secs).as_millis())
                .context("queue lease duration exceeds i64 millis")?,
            retry_base_ms: i64::try_from(config.retry_base_ms)
                .context("queue retry_base_ms exceeds i64")?,
            max_attempts: config.max_attempts.max(1),
        };

        queue.migrate_legacy_json_if_needed(&json_path)?;
        queue.reclaim_expired_tasks()?;
        Ok(queue)
    }

    pub fn enqueue_batch(&self, batch: &IngestBatch) -> Result<(String, usize)> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let total_chunks = i64::try_from(batch.chunks.len()).context("chunk count exceeds i64")?;

        // Pack pre-computed embeddings into compact little-endian binary so
        // payload_json stays small (text + metadata only).  If the batch
        // uses server-side embedding (all embedding vectors are empty) the
        // blob stays null, saving both the packing work and the column space.
        let embed_dims = batch.chunks.first().map_or(0, |c| c.embedding.len());
        let (embeddings_blob, embed_dims_stored): (Option<Vec<u8>>, i64) = if embed_dims > 0 {
            let mut blob = Vec::with_capacity(batch.chunks.len() * embed_dims * 4);
            for chunk in &batch.chunks {
                for &f in &chunk.embedding {
                    blob.extend_from_slice(&f.to_le_bytes());
                }
            }
            (Some(blob), i64::try_from(embed_dims).context("embed_dims exceeds i64")?)
        } else {
            (None, 0)
        };

        // Strip embeddings from the JSON payload — they are in the blob.
        let compact_batch = if embed_dims > 0 {
            IngestBatch {
                chunks: batch.chunks.iter().map(|c| IngestChunk {
                    embedding: Vec::new(),
                    ..c.clone()
                }).collect(),
            }
        } else {
            batch.clone()
        };
        let payload_json = serde_json::to_string(&compact_batch)
            .context("serializing compact ingest batch")?;

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting enqueue transaction")?;
        tx.execute(
            r#"
            INSERT INTO ingest_tasks (
                id, payload_json, state, total_chunks, completed_chunks, error,
                created_at, updated_at, attempt_count, max_attempts, worker_id,
                lease_expires_at, last_heartbeat_at, last_attempt_started_at, next_retry_at,
                embeddings_blob, embed_dims
            )
            VALUES (?1, ?2, 'queued', ?3, 0, NULL, ?4, ?4, 0, ?5, NULL, NULL, NULL, NULL, NULL, ?6, ?7)
            "#,
            params![task_id, payload_json, total_chunks, now, i64::from(self.max_attempts),
                    embeddings_blob, embed_dims_stored],
        )
        .context("inserting queued task")?;
        let queue_depth = queue_depth_tx(&tx)?;
        tx.commit().context("committing enqueue transaction")?;
        Ok((task_id, queue_depth))
    }

    pub fn get_task_status(&self, task_id: &str) -> Result<Option<TaskStatusResponse>> {
        self.reclaim_expired_tasks()?;
        let conn = self.conn.lock().unwrap();
        let queue_depth = queue_depth_conn(&conn)?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id,
                    state,
                    total_chunks,
                    completed_chunks,
                    attempt_count,
                    max_attempts,
                    last_heartbeat_at,
                    lease_expires_at,
                    error
                FROM ingest_tasks
                WHERE id = ?1
                "#,
            )
            .context("preparing task status query")?;
        let status: Option<(String, String, i64, i64, i64, i64, Option<i64>, Option<i64>, Option<String>)> = stmt
            .query_row(params![task_id], |row: &Row<'_>| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            })
            .optional()
            .context("reading task status")?;
        Ok(status
            .map(|(
                task_id,
                state,
                total_chunks,
                completed_chunks,
                attempt_count,
                max_attempts,
                last_heartbeat_at,
                lease_expires_at,
                error,
            )| -> Result<TaskStatusResponse> {
                let total_chunks = usize_from_i64(total_chunks, "total_chunks")?;
                let completed_chunks = usize_from_i64(completed_chunks, "completed_chunks")?;
                Ok(TaskStatusResponse {
                    task_id,
                    state,
                    total_chunks,
                    completed_chunks,
                    queue_depth,
                    attempt_count: Some(u32_from_i64(attempt_count, "attempt_count")?),
                    max_attempts: Some(u32_from_i64(max_attempts, "max_attempts")?),
                    last_heartbeat_at,
                    lease_expires_at,
                    error,
                })
            })
            .transpose()?)
    }

    fn claim_next_task(&self, worker_id: &str) -> Result<Option<QueuedTask>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("starting claim transaction")?;
        reclaim_expired_tasks_tx(&tx, self.lease_ms, self.retry_base_ms)?;

        let now = now_ms();
        let claimed: Option<(String, String, i64, i64, Option<Vec<u8>>, i64)> = tx
            .query_row(
                r#"
                WITH picked AS (
                    SELECT id
                    FROM ingest_tasks
                    WHERE state = 'queued'
                      AND (next_retry_at IS NULL OR next_retry_at <= ?1)
                    ORDER BY created_at ASC
                    LIMIT 1
                )
                UPDATE ingest_tasks
                SET state = 'processing',
                    attempt_count = attempt_count + 1,
                    worker_id = ?2,
                    lease_expires_at = ?3,
                    last_heartbeat_at = ?1,
                    last_attempt_started_at = ?1,
                    completed_chunks = 0,
                    error = NULL,
                    next_retry_at = NULL,
                    updated_at = ?1
                WHERE id IN (SELECT id FROM picked)
                RETURNING id, payload_json, total_chunks, attempt_count, embeddings_blob, embed_dims
                "#,
                params![now, worker_id, now + self.lease_ms],
                |row: &Row<'_>| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()
            .context("claiming next queued task")?;

        let Some((task_id, payload_json, total_chunks_i64, attempt_count_i64, embeddings_blob, embed_dims_i64)) = claimed else {
            tx.commit().context("committing empty claim transaction")?;
            return Ok(None);
        };
        tx.commit().context("committing claim transaction")?;

        let mut payload: IngestBatch = serde_json::from_str(&payload_json)
            .with_context(|| format!("deserializing queued payload for task {task_id}"))?;

        if let Some(blob) = embeddings_blob.as_deref() {
            let dims = usize::try_from(embed_dims_i64).unwrap_or(0);
            unpack_embeddings(&mut payload, blob, dims);
        }

        Ok(Some(QueuedTask {
            id: task_id,
            payload,
            total_chunks: usize_from_i64(total_chunks_i64, "total_chunks")?,
            attempt_count: u32_from_i64(attempt_count_i64, "attempt_count")?,
        }))
    }

    fn renew_lease(&self, task_id: &str, worker_id: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE ingest_tasks
            SET lease_expires_at = ?1,
                last_heartbeat_at = ?2,
                updated_at = ?2
            WHERE id = ?3
              AND state = 'processing'
              AND worker_id = ?4
            "#,
            params![now + self.lease_ms, now, task_id, worker_id],
        )
        .with_context(|| format!("renewing lease for task {task_id}"))?;
        Ok(())
    }

    fn mark_done(&self, task_id: &str, worker_id: &str, total_chunks: usize) -> Result<()> {
        let now = now_ms();
        let total_chunks = i64::try_from(total_chunks).context("total_chunks exceeds i64")?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE ingest_tasks
            SET state = 'done',
                completed_chunks = ?1,
                error = NULL,
                worker_id = NULL,
                lease_expires_at = NULL,
                last_heartbeat_at = NULL,
                next_retry_at = NULL,
                updated_at = ?2
            WHERE id = ?3
              AND worker_id = ?4
            "#,
            params![total_chunks, now, task_id, worker_id],
        )
        .with_context(|| format!("marking task {task_id} done"))?;
        Ok(())
    }

    fn mark_failed_or_requeue(&self, task_id: &str, worker_id: &str, error: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("starting failure transaction")?;
        let record: Option<(i64, i64)> = tx
            .query_row(
                r#"
                SELECT attempt_count, max_attempts
                FROM ingest_tasks
                WHERE id = ?1
                  AND worker_id = ?2
                "#,
                params![task_id, worker_id],
                |row: &Row<'_>| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .with_context(|| format!("loading task {task_id} failure state"))?;

        let Some((attempt_count, max_attempts)) = record else {
            tx.commit().context("committing empty failure transaction")?;
            return Ok(());
        };

        let now = now_ms();
        if attempt_count >= max_attempts {
            tx.execute(
                r#"
                UPDATE ingest_tasks
                SET state = 'failed',
                    error = ?1,
                    worker_id = NULL,
                    lease_expires_at = NULL,
                    last_heartbeat_at = NULL,
                    next_retry_at = NULL,
                    completed_chunks = 0,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![error, now, task_id],
            )
            .with_context(|| format!("marking task {task_id} failed"))?;
        } else {
            tx.execute(
                r#"
                UPDATE ingest_tasks
                SET state = 'queued',
                    error = NULL,
                    worker_id = NULL,
                    lease_expires_at = NULL,
                    last_heartbeat_at = NULL,
                    next_retry_at = ?1,
                    completed_chunks = 0,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![now + retry_delay_ms(self.retry_base_ms, attempt_count as u32), now, task_id],
            )
            .with_context(|| format!("requeueing task {task_id}"))?;
        }

        tx.commit().context("committing failure transaction")?;
        Ok(())
    }

    fn reclaim_expired_tasks(&self) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("starting reclaim transaction")?;
        reclaim_expired_tasks_tx(&tx, self.lease_ms, self.retry_base_ms)?;
        tx.commit().context("committing reclaim transaction")?;
        Ok(())
    }

    fn migrate_legacy_json_if_needed(&self, json_path: &Path) -> Result<()> {
        if !json_path.exists() {
            return Ok(());
        }

        let mut conn = self.conn.lock().unwrap();
        let existing_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ingest_tasks", [], |row: &Row<'_>| row.get(0))
            .context("counting existing sqlite queue rows")?;
        if existing_count > 0 {
            return Ok(());
        }

        let raw = std::fs::read_to_string(json_path)
            .with_context(|| format!("reading legacy queue file {}", json_path.display()))?;
        let legacy: LegacyQueueFile =
            serde_json::from_str(&raw).context("parsing legacy queue JSON")?;

        let tx = conn.transaction().context("starting legacy queue migration")?;
        let now = now_ms();
        for task in legacy.tasks {
            let (state, worker_id, lease_expires_at, last_heartbeat_at, next_retry_at, error) =
                normalize_legacy_task(&task, now);
            tx.execute(
                r#"
                INSERT INTO ingest_tasks (
                    id, payload_json, state, total_chunks, completed_chunks, error,
                    created_at, updated_at, attempt_count, max_attempts, worker_id,
                    lease_expires_at, last_heartbeat_at, last_attempt_started_at, next_retry_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
                params![
                    task.id,
                    task.payload_json,
                    state,
                    i64::try_from(task.total_chunks).context("legacy total_chunks exceeds i64")?,
                    i64::try_from(task.completed_chunks)
                        .context("legacy completed_chunks exceeds i64")?,
                    error,
                    task.created_at,
                    task.updated_at,
                    i64::from(task.attempt_count),
                    i64::from(task.max_attempts.max(self.max_attempts)),
                    worker_id,
                    lease_expires_at,
                    last_heartbeat_at,
                    task.last_attempt_started_at,
                    next_retry_at,
                ],
            )
            .with_context(|| format!("migrating legacy task {}", task.id))?;
        }
        tx.commit().context("committing legacy queue migration")?;

        let migrated_path = json_path.with_extension("json.migrated");
        std::fs::rename(json_path, &migrated_path).with_context(|| {
            format!(
                "renaming legacy queue file {} -> {}",
                json_path.display(),
                migrated_path.display()
            )
        })?;
        tracing::info!(
            "migrated legacy JSON queue into {}",
            self.db_path.display()
        );
        Ok(())
    }

    pub fn spawn_worker(&self, index: Arc<SearchIndex>, embedder: Option<ServerEmbedder>) {
        let queue = self.clone();
        let worker_id = format!("worker-{}", uuid::Uuid::new_v4());
        tokio::spawn(async move {
            loop {
                match queue.claim_next_task(&worker_id) {
                    Ok(Some(mut task)) => {
                        tracing::info!(
                            "queue claimed task {} attempt {} on {}",
                            task.id,
                            task.attempt_count,
                            worker_id
                        );
                        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
                        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
                        heartbeat.tick().await;
                        let mut work = Box::pin(async {
                            if let Some(ref emb) = embedder {
                                emb.hydrate_missing_embeddings(&mut task.payload).await?;
                            }
                            index.ingest_batch(&task.payload).await
                        });
                        let result = loop {
                            tokio::select! {
                                res = &mut work => break res,
                                _ = heartbeat.tick() => {
                                    if let Err(e) = queue.renew_lease(&task.id, &worker_id) {
                                        tracing::error!(
                                            "queue heartbeat failed for {} on {}: {e:#}",
                                            task.id,
                                            worker_id
                                        );
                                    }
                                }
                            }
                        };

                        match result {
                            Ok(()) => {
                                if let Err(e) = queue.mark_done(&task.id, &worker_id, task.total_chunks) {
                                    tracing::error!("queue mark_done failed for {}: {e:#}", task.id);
                                }
                            }
                            Err(e) => {
                                let msg = format!("{e:#}");
                                tracing::error!("queue task {} failed: {msg}", task.id);
                                if let Err(mark_err) =
                                    queue.mark_failed_or_requeue(&task.id, &worker_id, &msg)
                                {
                                    tracing::error!(
                                        "queue mark_failed_or_requeue failed for {}: {mark_err:#}",
                                        task.id
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => tokio::time::sleep(IDLE_POLL_INTERVAL).await,
                    Err(e) => {
                        tracing::error!("queue worker claim failed: {e:#}");
                        tokio::time::sleep(CLAIM_ERROR_BACKOFF).await;
                    }
                }
            }
        });
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ingest_tasks (
    id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    state TEXT NOT NULL,
    total_chunks INTEGER NOT NULL,
    completed_chunks INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    worker_id TEXT,
    lease_expires_at INTEGER,
    last_heartbeat_at INTEGER,
    last_attempt_started_at INTEGER,
    next_retry_at INTEGER,
    -- compact binary embedding storage (added in schema v2):
    -- embeddings_blob: little-endian f32 bytes packed sequentially per chunk
    -- embed_dims: dims per chunk (0 when server-side embedding fills them)
    embeddings_blob BLOB,
    embed_dims INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS ingest_tasks_claim_idx
    ON ingest_tasks(state, next_retry_at, created_at);

CREATE INDEX IF NOT EXISTS ingest_tasks_lease_idx
    ON ingest_tasks(state, lease_expires_at);
"#;

/// Migrate existing databases that predate the embeddings_blob columns.
/// `ALTER TABLE ADD COLUMN` is idempotent in practice because we check
/// column existence first.
fn migrate_schema_v2(conn: &Connection) -> Result<()> {
    let has_blob: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('ingest_tasks') WHERE name = 'embeddings_blob'",
            [],
            |r: &Row<'_>| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !has_blob {
        conn.execute_batch(
            "ALTER TABLE ingest_tasks ADD COLUMN embeddings_blob BLOB;
             ALTER TABLE ingest_tasks ADD COLUMN embed_dims INTEGER NOT NULL DEFAULT 0;",
        )
        .context("adding embeddings_blob / embed_dims columns (schema v2)")?;
    }
    Ok(())
}

fn reclaim_expired_tasks_tx(tx: &Transaction<'_>, lease_ms: i64, retry_base_ms: i64) -> Result<()> {
    let now = now_ms();
    let expired: Vec<(String, i64, i64)> = {
        let mut stmt = tx
            .prepare(
                r#"
                SELECT id, attempt_count, max_attempts
                FROM ingest_tasks
                WHERE state = 'processing'
                  AND lease_expires_at IS NOT NULL
                  AND lease_expires_at <= ?1
                "#,
            )
            .context("preparing expired lease query")?;
        let rows = stmt
            .query_map(params![now], |row: &Row<'_>| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .context("querying expired leases")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collecting expired leases")?
    };

    for (task_id, attempt_count, max_attempts) in expired {
        if attempt_count >= max_attempts {
            tx.execute(
                r#"
                UPDATE ingest_tasks
                SET state = 'failed',
                    error = ?1,
                    worker_id = NULL,
                    lease_expires_at = NULL,
                    last_heartbeat_at = NULL,
                    next_retry_at = NULL,
                    completed_chunks = 0,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![
                    format!("task lease expired after {attempt_count} attempts"),
                    now,
                    task_id,
                ],
            )
            .with_context(|| format!("failing expired task {task_id}"))?;
        } else {
            tx.execute(
                r#"
                UPDATE ingest_tasks
                SET state = 'queued',
                    error = NULL,
                    worker_id = NULL,
                    lease_expires_at = NULL,
                    last_heartbeat_at = NULL,
                    next_retry_at = ?1,
                    completed_chunks = 0,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![
                    now + retry_delay_ms(retry_base_ms.max(lease_ms.min(1_000)), attempt_count as u32),
                    now,
                    task_id,
                ],
            )
            .with_context(|| format!("requeueing expired task {task_id}"))?;
        }
    }
    Ok(())
}

fn queue_depth_conn(conn: &Connection) -> Result<usize> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ingest_tasks WHERE state IN ('queued', 'processing')",
            [],
            |row: &Row<'_>| row.get(0),
        )
        .context("counting queue depth")?;
    usize_from_i64(count, "queue_depth")
}

fn queue_depth_tx(tx: &Transaction<'_>) -> Result<usize> {
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM ingest_tasks WHERE state IN ('queued', 'processing')",
            [],
            |row: &Row<'_>| row.get(0),
        )
        .context("counting queue depth in transaction")?;
    usize_from_i64(count, "queue_depth")
}

fn normalize_legacy_task(
    task: &LegacyTaskRecord,
    now: i64,
) -> (String, Option<String>, Option<i64>, Option<i64>, Option<i64>, Option<String>) {
    match task.state.as_str() {
        "processing" => (
            "queued".to_owned(),
            None,
            None,
            None,
            Some(now),
            None,
        ),
        "failed" => (
            "failed".to_owned(),
            None,
            None,
            None,
            None,
            task.error.clone(),
        ),
        "done" => (
            "done".to_owned(),
            None,
            None,
            None,
            None,
            None,
        ),
        _ => (
            "queued".to_owned(),
            None,
            None,
            None,
            task.next_retry_at,
            None,
        ),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn default_max_attempts() -> u32 {
    DEFAULT_MAX_ATTEMPTS
}

fn retry_delay_ms(base_ms: i64, attempt_count: u32) -> i64 {
    let shift = attempt_count.saturating_sub(1).min(8);
    base_ms.saturating_mul(1_i64 << shift)
}

/// Restore per-chunk embeddings from a packed little-endian f32 blob.
/// Each chunk occupies exactly `dims * 4` bytes in `blob`.
fn unpack_embeddings(batch: &mut IngestBatch, blob: &[u8], dims: usize) {
    if dims == 0 || blob.is_empty() {
        return;
    }
    let bytes_per_chunk = dims * 4;
    for (i, chunk) in batch.chunks.iter_mut().enumerate() {
        let start = i * bytes_per_chunk;
        let end = start + bytes_per_chunk;
        if end > blob.len() {
            break;
        }
        chunk.embedding = blob[start..end]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
    }
}

fn usize_from_i64(value: i64, field: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| anyhow!("{field} out of range: {value}"))
}

fn u32_from_i64(value: i64, field: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| anyhow!("{field} out of range: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("crisp-queue-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn sample_batch() -> IngestBatch {
        IngestBatch { chunks: Vec::new() }
    }

    #[test]
    fn migrates_legacy_json_queue_into_sqlite() {
        let tmp = TestDir::new();
        let legacy_path = tmp.path().join("ingest_tasks.json");
        let legacy = serde_json::json!({
            "tasks": [{
                "id": "t1",
                "payload_json": serde_json::to_string(&sample_batch()).unwrap(),
                "state": "processing",
                "total_chunks": 0,
                "completed_chunks": 0,
                "error": null,
                "created_at": 1,
                "updated_at": 1
            }]
        });
        std::fs::write(&legacy_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let queue = TaskQueue::open_or_create(tmp.path(), QueueConfig::default()).unwrap();
        let status = queue.get_task_status("t1").unwrap().unwrap();
        assert_eq!(status.state, "queued");
        assert!(tmp.path().join("ingest_tasks.json.migrated").exists());
        assert!(tmp.path().join("crisp_jobs.db").exists());
    }

    #[test]
    fn expired_processing_rows_are_reclaimed_on_status_read() {
        let tmp = TestDir::new();
        let queue = TaskQueue::open_or_create(tmp.path(), QueueConfig::default()).unwrap();
        let (task_id, _) = queue.enqueue_batch(&sample_batch()).unwrap();
        {
            let conn = queue.conn.lock().unwrap();
            conn.execute(
                r#"
                UPDATE ingest_tasks
                SET state = 'processing',
                    attempt_count = 1,
                    max_attempts = 3,
                    worker_id = 'worker-a',
                    lease_expires_at = ?1
                WHERE id = ?2
                "#,
                params![now_ms() - 1, task_id],
            )
            .unwrap();
        }

        let status = queue.get_task_status(&task_id).unwrap().unwrap();
        assert_eq!(status.state, "queued");
    }

    #[test]
    fn status_exposes_attempt_and_lease_metadata() {
        let tmp = TestDir::new();
        let queue = TaskQueue::open_or_create(tmp.path(), QueueConfig::default()).unwrap();
        let (task_id, _) = queue.enqueue_batch(&sample_batch()).unwrap();

        let claimed = queue.claim_next_task("worker-a").unwrap().unwrap();
        assert_eq!(claimed.attempt_count, 1);

        let status = queue.get_task_status(&task_id).unwrap().unwrap();
        assert_eq!(status.state, "processing");
        assert_eq!(status.attempt_count, Some(1));
        assert_eq!(status.max_attempts, Some(QueueConfig::default().max_attempts));
        assert!(status.last_heartbeat_at.is_some());
        assert!(status.lease_expires_at.is_some());
    }

    #[test]
    fn failure_requeues_until_attempt_budget_is_exhausted() {
        let tmp = TestDir::new();
        let queue = TaskQueue::open_or_create(tmp.path(), QueueConfig::default()).unwrap();
        let (task_id, _) = queue.enqueue_batch(&sample_batch()).unwrap();

        let first = queue.claim_next_task("worker-a").unwrap().unwrap();
        assert_eq!(first.attempt_count, 1);
        queue
            .mark_failed_or_requeue(&task_id, "worker-a", "boom")
            .unwrap();
        {
            let conn = queue.conn.lock().unwrap();
            let (state, next_retry_at): (String, Option<i64>) = conn
                .query_row(
                    "SELECT state, next_retry_at FROM ingest_tasks WHERE id = ?1",
                    params![task_id],
                    |row: &Row<'_>| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(state, "queued");
            assert!(next_retry_at.is_some());
            conn.execute(
                "UPDATE ingest_tasks SET next_retry_at = ?1 WHERE id = ?2",
                params![now_ms() - 1, task_id],
            )
            .unwrap();
        }

        let second = queue.claim_next_task("worker-a").unwrap().unwrap();
        assert_eq!(second.attempt_count, 2);
        queue
            .mark_failed_or_requeue(&task_id, "worker-a", "boom again")
            .unwrap();
        {
            let conn = queue.conn.lock().unwrap();
            conn.execute(
                "UPDATE ingest_tasks SET next_retry_at = ?1 WHERE id = ?2",
                params![now_ms() - 1, task_id],
            )
            .unwrap();
        }

        let third = queue.claim_next_task("worker-a").unwrap().unwrap();
        assert_eq!(third.attempt_count, 3);
        queue
            .mark_failed_or_requeue(&task_id, "worker-a", "final boom")
            .unwrap();

        let status = queue.get_task_status(&task_id).unwrap().unwrap();
        assert_eq!(status.state, "failed");
        assert!(status.error.unwrap().contains("final boom"));
    }
}
