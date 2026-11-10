//! Cursor transport and checkpoint validation for metadata pull pages.
use anyhow::{ensure, Context, Result};
use crisp_index_protocol::{SyncCursor, SyncDeletion, SyncPage, SYNC_CURSOR_VERSION};
use serde::{Deserialize, Serialize};

pub const CHECKPOINT_KEY: &str = "pull_checkpoint_v1";

const DELETION_CONFLICT_PREFIX: &str = "sync-delete-v2:";
const METADATA_CONFLICT_PREFIX: &str = "sync-metadata-v2:";

/// Durable manual-review context. The local signature prevents a later review
/// click from deleting a document edited since the conflict was queued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDeletionConflict {
    pub remote_url: String,
    pub deletion: SyncDeletion,
    pub expected_local_signature: String,
}

impl RemoteDeletionConflict {
    pub fn encode(&self) -> Result<String> {
        Ok(format!(
            "{DELETION_CONFLICT_PREFIX}{}",
            serde_json::to_string(self)?
        ))
    }

    pub fn decode(value: &str) -> Result<Option<Self>> {
        value
            .strip_prefix(DELETION_CONFLICT_PREFIX)
            .map(|encoded| serde_json::from_str(encoded).context("Reading deletion conflict"))
            .transpose()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteMetadataConflict {
    pub remote_url: String,
    pub remote: crisp_index_protocol::SearchHit,
    pub expected_local_signature: String,
}

impl RemoteMetadataConflict {
    pub fn encode(&self) -> Result<String> {
        Ok(format!("{METADATA_CONFLICT_PREFIX}{}", serde_json::to_string(self)?))
    }

    pub fn decode(value: &str) -> Result<Option<Self>> {
        value.strip_prefix(METADATA_CONFLICT_PREFIX)
            .map(|encoded| serde_json::from_str(encoded).context("Reading metadata conflict"))
            .transpose()
    }
}

/// Deletion has no remote copy to retain. KeepBoth therefore keeps the local
/// document, and an independently named copy is outside this event's scope.
pub fn deletion_resolution(
    local_doc_id: &str,
    local_owner: &str,
    local_indexed_at: i64,
    deletion: &SyncDeletion,
    policy: super::conflict::ConflictPolicy,
) -> super::conflict::Resolution {
    use super::conflict::{ConflictPolicy, Resolution};
    if local_doc_id != deletion.doc_id || local_owner != deletion.owner_id {
        return Resolution::UseLocal;
    }
    match policy {
        ConflictPolicy::LocalWins | ConflictPolicy::KeepBoth => Resolution::UseLocal,
        ConflictPolicy::RemoteWins => Resolution::UseRemote,
        ConflictPolicy::Manual => Resolution::NeedsManualReview,
        ConflictPolicy::NewestWins => {
            if local_indexed_at >= deletion.indexed_at {
                Resolution::UseLocal
            } else {
                Resolution::UseRemote
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullCheckpoint {
    pub version: u32,
    pub remote_url: String,
    pub since_ms: i64,
    pub cursor: Option<String>,
}

impl PullCheckpoint {
    pub fn for_remote(
        saved: Option<&str>,
        legacy_timestamp: i64,
        remote_url: &str,
    ) -> Result<Self> {
        let remote_url = remote_url.trim_end_matches('/').to_owned();
        if let Some(saved) = saved {
            let mut checkpoint: Self =
                serde_json::from_str(saved).context("Reading sync checkpoint")?;
            ensure!(
                (checkpoint.version == 1 || checkpoint.version == SYNC_CURSOR_VERSION)
                    && checkpoint.since_ms >= 0,
                "Unsupported sync checkpoint"
            );
            if checkpoint.version == 1 {
                // A pre-deletion cursor cannot describe the new combined feed.
                // Replay its completed boundary; any partial page was not yet
                // published as a completed timestamp.
                checkpoint.version = SYNC_CURSOR_VERSION;
                checkpoint.cursor = None;
            }
            if checkpoint.remote_url == remote_url {
                if let Some(cursor) = &checkpoint.cursor {
                    let cursor: SyncCursor =
                        serde_json::from_str(cursor).context("Reading sync continuation")?;
                    ensure!(
                        cursor.is_valid() && cursor.since_ms == checkpoint.since_ms,
                        "Invalid sync continuation"
                    );
                }
                return Ok(checkpoint);
            }
            // A watermark from a different server cannot describe this corpus.
            return Ok(Self {
                version: SYNC_CURSOR_VERSION,
                remote_url,
                since_ms: 0,
                cursor: None,
            });
        }
        Ok(Self {
            version: SYNC_CURSOR_VERSION,
            remote_url,
            since_ms: legacy_timestamp.max(0),
            cursor: None,
        })
    }

    pub fn after_page(&self, page: &SyncPage, limit: usize) -> Result<Self> {
        ensure!(
            page.sync_version == SYNC_CURSOR_VERSION,
            "Server does not support the required deletion-aware sync protocol; upgrade the server"
        );
        ensure!(
            page.rows.len().saturating_add(page.deletions.len()) <= limit,
            "Server exceeded the requested sync page size"
        );
        ensure!(
            page.max_indexed_at >= self.since_ms,
            "Server moved the sync watermark backwards"
        );
        ensure!(page.snapshot_version.is_some_and(|version| version > 0),
            "Server omitted the sync snapshot identity");
        let previous = self.cursor.as_deref().map(serde_json::from_str::<SyncCursor>).transpose()?;
        if let Some(previous) = &previous {
            ensure!(
                page.snapshot_version == Some(previous.snapshot_version)
                    && page.max_indexed_at >= previous.indexed_at
                    && page.max_indexed_at <= previous.tombstone_horizon,
                "Server changed the snapshot or moved the continuation backwards"
            );
        }
        let mut identities = std::collections::HashSet::new();
        let mut positions = Vec::with_capacity(page.rows.len() + page.deletions.len());
        let mut last_live = None;
        for row in &page.rows {
            ensure!(
                !row.doc_id.is_empty() && row.doc_id.len() <= 512 && row.owner_id.len() <= 512
                    && row.indexed_at >= self.since_ms && row.indexed_at <= page.max_indexed_at
                    && identities.insert((&row.owner_id, &row.doc_id)),
                "Server returned an invalid or duplicate sync identity"
            );
            let position = (row.indexed_at, &row.owner_id, &row.doc_id);
            ensure!(last_live.is_none_or(|last| position > last), "Server returned unordered sync rows");
            last_live = Some(position);
            positions.push(position);
        }
        let mut last_deletion = None;
        for deletion in &page.deletions {
            ensure!(
                !deletion.doc_id.is_empty()
                    && deletion.doc_id.len() <= 512
                    && deletion.owner_id.len() <= 512
                    && deletion.indexed_at >= self.since_ms
                    && deletion.indexed_at <= page.max_indexed_at
                    && identities.insert((&deletion.owner_id, &deletion.doc_id)),
                "Server returned an invalid or duplicate deletion identity"
            );
            let position = (deletion.indexed_at, &deletion.owner_id, &deletion.doc_id);
            ensure!(last_deletion.is_none_or(|last| position > last), "Server returned unordered deletion rows");
            last_deletion = Some(position);
            positions.push(position);
        }
        if let Some(previous) = &previous {
            ensure!(positions.iter().all(|position| *position >
                (previous.indexed_at, &previous.owner_id, &previous.doc_id)),
                "Server repeated or regressed an applied sync change");
        }
        let last_position = positions.iter().max().copied();
        let expected_watermark = last_position.map(|position| position.0)
            .unwrap_or_else(|| previous.as_ref().map_or(self.since_ms, |cursor| cursor.indexed_at));
        ensure!(page.max_indexed_at == expected_watermark,
            "Sync watermark does not describe the last returned change");
        let mut next = self.clone();
        if page.has_more {
            ensure!(
                !page.rows.is_empty() || !page.deletions.is_empty(),
                "Server returned an empty continuation page"
            );
            let encoded = page.next_cursor.as_ref().context("Server requires pagination but does not provide a safe cursor; upgrade the server before continuing")?;
            let cursor: SyncCursor =
                serde_json::from_str(encoded).context("Reading server sync cursor")?;
            ensure!(
                cursor.is_valid()
                    && cursor.since_ms == self.since_ms
                    && page.snapshot_version == Some(cursor.snapshot_version)
                    && page.max_indexed_at == cursor.indexed_at,
                "Server returned an inconsistent sync cursor"
            );
            ensure!(
                last_position == Some((cursor.indexed_at, &cursor.owner_id, &cursor.doc_id)),
                "Sync cursor does not identify the last applied change"
            );
            if let Some(previous) = &previous {
                ensure!(
                    cursor.snapshot_version == previous.snapshot_version
                        && cursor.tombstone_horizon == previous.tombstone_horizon
                        && (cursor.indexed_at, &cursor.owner_id, &cursor.doc_id)
                            > (previous.indexed_at, &previous.owner_id, &previous.doc_id),
                    "Server did not advance the sync cursor"
                );
            }
            next.cursor = Some(encoded.clone());
        } else {
            ensure!(
                page.next_cursor.is_none(),
                "Server returned a cursor for a completed sync page"
            );
            // Inclusive replay covers writes in the boundary millisecond that
            // arrived after the completed immutable snapshot was opened.
            next.since_ms = page.max_indexed_at;
            next.cursor = None;
        }
        Ok(next)
    }
}

/// Fetch one bounded page. The caller persists the returned checkpoint only
/// after its local paired write succeeds.
pub async fn fetch_page(
    client: &reqwest::Client,
    checkpoint: &PullCheckpoint,
    api_key: &str,
    limit: usize,
) -> Result<(SyncPage, PullCheckpoint)> {
    ensure!(
        (1..=1000).contains(&limit),
        "Sync page limit must be between 1 and 1000"
    );
    let mut effective = checkpoint.clone();
    for attempt in 0..2 {
        let mut request = client
            .get(format!("{}/v1/sync/since", effective.remote_url))
            .query(&[
                ("ts", effective.since_ms.to_string()),
                ("limit", limit.to_string()),
                ("cursor_version", SYNC_CURSOR_VERSION.to_string()),
            ]);
        if let Some(cursor) = &effective.cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let expired = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("code")
                        .and_then(|code| code.as_str())
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("sync_snapshot_expired");
            if attempt == 0
                && status == reqwest::StatusCode::CONFLICT
                && expired
                && effective.cursor.is_some()
            {
                // Replay from the last completed boundary; never advance past
                // a snapshot removed by retention or an index replacement.
                effective.cursor = None;
                continue;
            }
            anyhow::bail!("Remote sync failed {status}: {body}");
        }
        let page: SyncPage = response.json().await.context("Reading remote sync page")?;
        let next = effective.after_page(&page, limit)?;
        return Ok((page, next));
    }
    unreachable!("the second attempt returns success or an error")
}

pub struct PendingPullPage {
    pub page: SyncPage,
    pub checkpoint_after_apply: PullCheckpoint,
    pub expected_checkpoint: Option<String>,
}

/// Publish the continuation and its completed timestamp in one durable state
/// transaction. A racing pull cannot overwrite a newer continuation.
pub fn acknowledge_page(
    connection: &mut rusqlite::Connection,
    pending: &PendingPullPage,
) -> Result<()> {
    use rusqlite::OptionalExtension;
    let next = serde_json::to_string(&pending.checkpoint_after_apply)?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current: Option<String> = transaction
        .query_row(
            "SELECT value FROM sync_state WHERE key=?1",
            [CHECKPOINT_KEY],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        current == pending.expected_checkpoint || current.as_deref() == Some(next.as_str()),
        "Another pull advanced the checkpoint; retry synchronization"
    );
    for (key, value) in [
        (CHECKPOINT_KEY, next),
        (
            "last_pull_ts",
            pending.checkpoint_after_apply.since_ms.to_string(),
        ),
    ] {
        transaction.execute("INSERT INTO sync_state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![key, value])?;
    }
    transaction.commit()?;
    Ok(())
}
