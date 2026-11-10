//! Additive cursor protocol for bounded, repeatable metadata synchronization.
use serde::{Deserialize, Serialize};

use super::SearchHit;

/// Current metadata sync continuation encoding.
pub const SYNC_CURSOR_VERSION: u32 = 2;

/// Continuation within one immutable Lance snapshot. Clients treat its JSON
/// encoding as an opaque query parameter and preserve it until local apply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncCursor {
    /// Continuation encoding version.
    pub version: u32,
    /// Immutable table version used by every page in this cycle.
    pub snapshot_version: u64,
    /// Upper bound of deletion history observed with this table snapshot.
    pub tombstone_horizon: i64,
    /// Inclusive lower timestamp from the last completed cycle.
    pub since_ms: i64,
    /// Timestamp of the last returned document.
    pub indexed_at: i64,
    /// First stable tie-break component.
    pub owner_id: String,
    /// Second stable tie-break component.
    pub doc_id: String,
}

impl SyncCursor {
    /// Check bounded identity fields and supported version before building SQL.
    pub fn is_valid(&self) -> bool {
        self.version == SYNC_CURSOR_VERSION
            && self.snapshot_version > 0
            && self.since_ms >= 0
            && self.indexed_at >= self.since_ms
            && self.tombstone_horizon >= self.indexed_at
            && self.owner_id.len() <= 512
            && !self.doc_id.is_empty()
            && self.doc_id.len() <= 512
    }
}

/// A durable deletion of one owner's document at a server write timestamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncDeletion {
    /// Collection owner whose copy was removed.
    pub owner_id: String,
    /// Content identity removed by the server.
    pub doc_id: String,
    /// Monotonic server mutation timestamp.
    pub indexed_at: i64,
}

/// A bounded page of document metadata. A completed timestamp is inclusive:
/// the next synchronization replays its boundary millisecond so writes that
/// arrived after this snapshot with the same timestamp cannot be skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPage {
    /// Negotiated cursor and deletion-feed version (zero for legacy servers).
    #[serde(default)]
    pub sync_version: u32,
    /// Document metadata in timestamp, owner, and document order.
    pub rows: Vec<SearchHit>,
    /// Deletions interleaved with rows by the same bounded page cursor.
    #[serde(default)]
    pub deletions: Vec<SyncDeletion>,
    /// Largest returned timestamp, retained inclusively after completion.
    pub max_indexed_at: i64,
    /// Whether another page in this exact snapshot exists.
    pub has_more: bool,
    /// Opaque continuation; mandatory whenever `has_more` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The pinned source table version, absent only for older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_version: Option<u64>,
}
