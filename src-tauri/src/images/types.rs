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
