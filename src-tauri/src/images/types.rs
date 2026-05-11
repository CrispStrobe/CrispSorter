//! P13 — wire types for the Images vertical (Tier 1, local only).
//!
//! These types live in this module for slice A1; per
//! `docs/P13_Bilder_integration.md` they migrate to a workspace
//! `crisplens-protocol` crate when slice B1 lands and we start sharing
//! shapes with CrispLens over HTTP.
//!
//! The Tier-1 `Image` shape intentionally stays narrow: filename, ext,
//! a doc_id pointer back to LanceDB, and the `location_uri` so the UI
//! can resolve to a real path later.  EXIF / dimensions / pHash columns
//! are added in slices A2 and A4.  Tier 2 fields (`face_count`, `tags`,
//! numeric `id`) come with B1 and live in the protocol crate.
//!
//! All fields are `Option<_>` where they can be missing, so existing
//! LanceDB rows that pre-date the image columns deserialise cleanly.

use serde::{Deserialize, Serialize};

/// One image row, pulled from LanceDB and shaped for the Images grid.
///
/// `doc_id` is the same UUID-ish identifier the rest of the app uses
/// (`SearchResult::doc_id`).  `location_uri` keeps its scheme prefix
/// (`file://`, `crisp+local://`, `crisp+drive://…`) — the UI strips
/// the scheme when it needs a display path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub doc_id: String,
    pub location_uri: String,
    pub filename: Option<String>,
    pub ext: Option<String>,
    /// Bytes from `metadata_json.fs_size` if present; `None` otherwise.
    /// Cheap to surface — we already parse `metadata_json` for level
    /// detection in the Übersicht pane.
    pub size: Option<i64>,
    /// Unix milliseconds when this row was indexed; same field as
    /// `SearchResult::indexed_at`.  Used for the default newest-first
    /// sort in the grid.
    pub indexed_at: i64,
    /// SHA-256 of the original file bytes — same value as
    /// `SearchResult::source_hash`.  A1 didn't surface this; A3's
    /// duplicate-grouping view needs it client-side too so the UI
    /// can label each group.  `#[serde(default)]` lets older JSON
    /// payloads (pre-A3) still deserialise.
    #[serde(default)]
    pub source_hash: String,
}

/// One cluster of image rows that share the same SHA-256
/// `source_hash` — i.e. byte-identical files in the index.  Returned
/// by `ImagesBackend::duplicates` for the A3 dup-view.
///
/// Groups always have `items.len() >= 2` (a singleton isn't a
/// duplicate); the backend filters smaller groups out before
/// returning.  Order is by `items.len()` descending so the UI
/// surfaces the most-duplicated files first.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub source_hash: String,
    pub items: Vec<Image>,
}

/// One cluster of image rows whose perceptual hashes are within the
/// caller-supplied Hamming threshold (default 8 — see
/// `phash::DEFAULT_NEAR_DUP_THRESHOLD`).  Returned by
/// `ImagesBackend::near_duplicates` for the A4 view.
///
/// Distinct from `DuplicateGroup` because near-dups are about visual
/// similarity, not byte identity: a JPEG and its phone-thumbnail copy
/// land here with `source_hash` differing but `phash` close.  Groups
/// always have `items.len() >= 2`; ordered by group size descending,
/// then by the smallest `phash` in the group for determinism.
///
/// `representative_phash_hex` is the canonical 16-char zero-padded
/// lower-case hex form (no `0x`).  The wire shape is a string rather
/// than an i64 because JSON numbers in JavaScript are 64-bit floats
/// and lose precision past 2^53; pHash hashes routinely have the
/// high bit set so the round-trip through Tauri's JSON IPC would
/// silently corrupt the value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NearDuplicateGroup {
    pub representative_phash_hex: String,
    pub items: Vec<NearDuplicateItem>,
}

/// One member of a near-dup cluster — same fields as `Image` plus the
/// computed `phash` (so the UI can surface the per-row pHash) and the
/// Hamming distance from the cluster representative.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NearDuplicateItem {
    pub image: Image,
    pub phash_hex: String,
    pub distance_from_rep: u32,
}

/// Format a packed 64-bit pHash (i64) as a 16-char zero-padded
/// lower-case hex string for the wire payload.  Sign-bit-safe:
/// reinterprets the i64 as u64 first so negative values render as
/// their unsigned bit pattern (which is what the JS / display side
/// actually wants — pHashes are bag-of-bits, not signed integers).
pub fn phash_to_hex(packed: i64) -> String {
    format!("{:016x}", packed as u64)
}

/// Inverse of `phash_to_hex`.  Returns `None` for malformed input.
pub fn phash_from_hex(s: &str) -> Option<i64> {
    if s.len() != 16 {
        return None;
    }
    u64::from_str_radix(s, 16).ok().map(|u| u as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phash_hex_round_trips_low_values() {
        assert_eq!(phash_to_hex(0), "0000000000000000");
        assert_eq!(phash_to_hex(1), "0000000000000001");
        assert_eq!(phash_to_hex(0x12_3456_789a_bcde), "00123456789abcde");
        assert_eq!(phash_from_hex("00123456789abcde"), Some(0x12_3456_789a_bcde));
        // Round-trip a sweep of representative values.
        for v in [0i64, 1, -1, i64::MIN, i64::MAX, 0x_dead_beef_0000_0000_u64 as i64] {
            assert_eq!(phash_from_hex(&phash_to_hex(v)), Some(v),
                "round-trip failed for {v:#x}");
        }
    }

    #[test]
    fn phash_hex_high_bit_round_trips_safely() {
        // The whole point of the hex wire shape: i64::MIN through
        // -1 round-trip exactly, even though JSON-via-JS would lose
        // precision past 2^53.  Pin every byte boundary so we'd
        // catch any future regression that accidentally re-introduces
        // a numeric wire shape.
        let edge_cases: &[i64] = &[
            i64::MIN,
            -1,
            -(1 << 53),
            (1 << 53) - 1,
            i64::MAX,
            0x_8000_0000_0000_0000_u64 as i64,
        ];
        for v in edge_cases {
            let hex = phash_to_hex(*v);
            assert_eq!(hex.len(), 16, "{v:#x} -> {hex}");
            assert_eq!(phash_from_hex(&hex), Some(*v));
        }
    }

    #[test]
    fn phash_from_hex_rejects_bad_inputs() {
        assert!(phash_from_hex("").is_none());
        assert!(phash_from_hex("short").is_none());
        assert!(phash_from_hex("toolongtoolongtoolong").is_none());
        assert!(phash_from_hex("ggggggggggggggg!").is_none()); // bad chars
    }
}

/// One page of `Image` rows.  Pagination is opaque-cursor based so we
/// can swap the underlying scan strategy in later slices without
/// breaking the wire format (mirrors `index::schema::DocumentPage`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesPage {
    pub items: Vec<Image>,
    /// Total rows matching the same filter regardless of page —
    /// computed by LanceDB's `count_rows`, so it's cheap.
    pub total: i64,
    /// `None` when fewer than `page_size` rows came back.  Otherwise
    /// pass back unchanged in the next request.
    pub next_cursor: Option<String>,
    pub page_size: i32,
}

/// Filter knob set passed by the UI.  `parent_dir_prefix` mirrors the
/// existing Übersicht folder filter so the Images tab can scope to the
/// same subtree.  `ext` overrides the default IMAGE_EXTS list — the
/// Tauri command falls back to the defaults when this is `None` so the
/// frontend doesn't have to know the canonical list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListFilters {
    pub parent_dir_prefix: Option<String>,
    pub ext: Option<Vec<String>>,
    pub owner_id: Option<String>,
    pub volume_ids: Option<Vec<String>>,
}

/// Forward-looking — `ImagesBackend::health` returns this.  Tier 1
/// always reports `Ok` since the local index is the source of truth;
/// Tier 2 will surface CrispLens's `/api/health` here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HealthStatus {
    Ok {
        version: String,
        face_engine: Option<String>,
    },
    Degraded {
        reason: String,
    },
}

/// Reference to an image row.  `Local` carries the LanceDB `doc_id`;
/// `Remote` is the CrispLens numeric `image_id` (used from B1 onwards).
/// Defined now so the trait signature is stable across slices.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum ImageRef {
    Local(String),
    Remote(i64),
}
