//! P29.4 — Conflict resolution policies.
//!
//! When a cloud-backup pull encounters a document that already exists
//! locally with different content, the conflict resolver determines what
//! to do based on a user-configured policy.

use serde::{Deserialize, Serialize};

// ── Types ────────────────────────────────────────────────────────────────

/// User-configurable conflict resolution policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Keep whichever version has the newer timestamp.
    NewestWins,
    /// Always keep the local version.
    LocalWins,
    /// Always keep the remote version (current default behaviour).
    RemoteWins,
    /// Keep both — the remote version gets a `_remote` suffix on its doc_id.
    KeepBoth,
    /// Queue the conflict for manual review.
    Manual,
}

impl Default for ConflictPolicy {
    fn default() -> Self {
        // Backward-compatible: matches the previous overwrite behaviour.
        Self::NewestWins
    }
}

/// Metadata about one side of a conflict.
#[derive(Debug, Clone)]
pub struct ConflictSide {
    pub doc_id: String,
    pub source_hash: String,
    pub updated_at: Option<i64>,
    pub title: Option<String>,
}

/// The resolution decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Use the local version; discard the remote.
    UseLocal,
    /// Use the remote version; overwrite local.
    UseRemote,
    /// Keep both.  `remote_doc_id` is the new doc_id for the remote copy.
    KeepBoth { remote_doc_id: String },
    /// Cannot resolve automatically; queue for manual review.
    NeedsManualReview,
}

// ── Resolver ─────────────────────────────────────────────────────────────

/// Resolve a conflict between a local and remote document.
///
/// If the content hashes are identical, there is no conflict regardless
/// of the policy — both sides have the same data.
pub fn resolve_conflict(
    local: &ConflictSide,
    remote: &ConflictSide,
    policy: ConflictPolicy,
) -> Resolution {
    // Short-circuit: identical content → no conflict.
    if local.source_hash == remote.source_hash {
        return Resolution::UseLocal;
    }

    match policy {
        ConflictPolicy::NewestWins => {
            match (local.updated_at, remote.updated_at) {
                (Some(l), Some(r)) => {
                    if l >= r {
                        Resolution::UseLocal
                    } else {
                        Resolution::UseRemote
                    }
                }
                // If timestamps are missing, fall back to keeping local.
                (Some(_), None) => Resolution::UseLocal,
                (None, Some(_)) => Resolution::UseRemote,
                (None, None) => Resolution::UseLocal,
            }
        }
        ConflictPolicy::LocalWins => Resolution::UseLocal,
        ConflictPolicy::RemoteWins => Resolution::UseRemote,
        ConflictPolicy::KeepBoth => {
            let remote_doc_id = format!("{}_remote", remote.doc_id);
            Resolution::KeepBoth { remote_doc_id }
        }
        ConflictPolicy::Manual => Resolution::NeedsManualReview,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn local_side(hash: &str, ts: Option<i64>) -> ConflictSide {
        ConflictSide {
            doc_id: "doc-local".into(),
            source_hash: hash.into(),
            updated_at: ts,
            title: Some("Local Doc".into()),
        }
    }

    fn remote_side(hash: &str, ts: Option<i64>) -> ConflictSide {
        ConflictSide {
            doc_id: "doc-remote".into(),
            source_hash: hash.into(),
            updated_at: ts,
            title: Some("Remote Doc".into()),
        }
    }

    #[test]
    fn identical_hashes_always_use_local() {
        for policy in [
            ConflictPolicy::NewestWins,
            ConflictPolicy::LocalWins,
            ConflictPolicy::RemoteWins,
            ConflictPolicy::KeepBoth,
            ConflictPolicy::Manual,
        ] {
            let r = resolve_conflict(
                &local_side("abc123", Some(100)),
                &remote_side("abc123", Some(200)),
                policy,
            );
            assert_eq!(
                r,
                Resolution::UseLocal,
                "policy {policy:?} should short-circuit on identical hashes"
            );
        }
    }

    #[test]
    fn newest_wins_picks_newer_timestamp() {
        let r = resolve_conflict(
            &local_side("aaa", Some(100)),
            &remote_side("bbb", Some(200)),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r, Resolution::UseRemote);

        let r = resolve_conflict(
            &local_side("aaa", Some(300)),
            &remote_side("bbb", Some(200)),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r, Resolution::UseLocal);
    }

    #[test]
    fn newest_wins_equal_timestamps_prefers_local() {
        let r = resolve_conflict(
            &local_side("aaa", Some(100)),
            &remote_side("bbb", Some(100)),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r, Resolution::UseLocal);
    }

    #[test]
    fn newest_wins_missing_timestamps() {
        // Local has timestamp, remote doesn't → local wins.
        let r = resolve_conflict(
            &local_side("aaa", Some(100)),
            &remote_side("bbb", None),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r, Resolution::UseLocal);

        // Remote has timestamp, local doesn't → remote wins.
        let r = resolve_conflict(
            &local_side("aaa", None),
            &remote_side("bbb", Some(100)),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r, Resolution::UseRemote);

        // Neither has timestamp → local wins.
        let r = resolve_conflict(
            &local_side("aaa", None),
            &remote_side("bbb", None),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r, Resolution::UseLocal);
    }

    #[test]
    fn local_wins_always() {
        let r = resolve_conflict(
            &local_side("aaa", Some(100)),
            &remote_side("bbb", Some(999)),
            ConflictPolicy::LocalWins,
        );
        assert_eq!(r, Resolution::UseLocal);
    }

    #[test]
    fn remote_wins_always() {
        let r = resolve_conflict(
            &local_side("aaa", Some(999)),
            &remote_side("bbb", Some(100)),
            ConflictPolicy::RemoteWins,
        );
        assert_eq!(r, Resolution::UseRemote);
    }

    #[test]
    fn keep_both_produces_renamed_doc_id() {
        let r = resolve_conflict(
            &local_side("aaa", Some(100)),
            &remote_side("bbb", Some(200)),
            ConflictPolicy::KeepBoth,
        );
        assert_eq!(
            r,
            Resolution::KeepBoth {
                remote_doc_id: "doc-remote_remote".into()
            }
        );
    }

    #[test]
    fn manual_policy_returns_needs_review() {
        let r = resolve_conflict(
            &local_side("aaa", Some(100)),
            &remote_side("bbb", Some(200)),
            ConflictPolicy::Manual,
        );
        assert_eq!(r, Resolution::NeedsManualReview);
    }

    #[test]
    fn policy_serde_round_trips() {
        for p in [
            ConflictPolicy::NewestWins,
            ConflictPolicy::LocalWins,
            ConflictPolicy::RemoteWins,
            ConflictPolicy::KeepBoth,
            ConflictPolicy::Manual,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: ConflictPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, p);
        }
    }

    #[test]
    fn default_policy_is_newest_wins() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::NewestWins);
    }
}
