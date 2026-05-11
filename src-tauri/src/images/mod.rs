//! P13 — Images vertical (Photos / images).
//!
//! Mirrors `crate::drives` in shape: a trait (`ImagesBackend`) +
//! a Tier-1 impl (`LocalImages`) + Tauri command thin wrappers.
//!
//! ## Tiering
//!
//! Per `docs/P13_Bilder_integration.md`:
//!
//! * **Tier 0** — feature disabled / hidden.  Not modelled in code; the
//!   tab simply isn't shown when there's nothing indexed.
//! * **Tier 1** — `LocalImages`, the default for every fresh install.
//!   Filters the existing LanceDB index down to image rows.  Zero
//!   external deps.  Implemented in [`local`].
//! * **Tier 2** — `CrispLensImages`, an HTTP client against a sibling
//!   CrispLens server.  Lands in slice **B1**; the module isn't in this
//!   tree yet but the trait method set is already locked so we don't
//!   churn the UI when it shows up.
//!
//! For slice A1 only `LocalImages::list` is wired all the way through.
//! Every other trait method on `LocalImages` returns an error so the UI
//! gets a clean failure rather than a Tauri panic if it accidentally
//! reaches for a future-slice capability.

pub mod exif;
pub mod local;
pub mod phash;
pub mod tauri_commands;
pub mod thumbnail;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;

pub use types::{
    DuplicateGroup, HealthStatus, Image, ImageRef, ImagesPage, ListFilters,
    NearDuplicateGroup, NearDuplicateItem,
};

/// Canonical lower-cased list of extensions the Images grid considers
/// "image rows".  Matches the spec in
/// `docs/P13_Bilder_integration.md` — keep this list and the spec in
/// sync.  GIF/AVIF/SVG/ICO are deliberately excluded: they're either
/// non-photographic (SVG, ICO), animation-centric (GIF, APNG) or
/// codec-fringe (AVIF in the EXIF pipeline) — they can be added in a
/// follow-up slice once the rest of the vertical is shaped.
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "heic", "heif", "tiff", "bmp",
];

/// Returns `true` if `ext` (lower-cased internally) is one of the
/// extensions surfaced in the Images tab.  Case-insensitive on
/// purpose — LanceDB stores `ext` already lower-cased but callers in
/// the UI sometimes upper-case for display.
pub fn is_image_ext(ext: &str) -> bool {
    let lower = ext.to_lowercase();
    IMAGE_EXTS.iter().any(|&e| e == lower)
}

/// Source of image data + face data + semantic search.  Tier 1
/// (`LocalImages`) and Tier 2 (`CrispLensImages`, future) implement
/// this so the UI can swap backends transparently.
///
/// Methods are `async` because the Tier-2 impl will be a thin HTTP
/// client; `async-trait` is already a workspace dep used by the
/// extractor pipeline.
#[async_trait]
pub trait ImagesBackend: Send + Sync {
    /// One-shot health probe.  Used by the auto-degradation monitor
    /// in slice B4 to decide whether to fall back to Tier 1.
    async fn health(&self) -> Result<HealthStatus>;

    /// List image rows matching `filters`.  Pagination via opaque
    /// cursor; `None` cursor = first page.
    async fn list(
        &self,
        page_size: i32,
        cursor: Option<String>,
        filters: ListFilters,
    ) -> Result<ImagesPage>;

    /// Group image rows by SHA-256 `source_hash` and return only
    /// groups with two or more members — the byte-identical-file
    /// duplicate view (slice A3).  Order is by group size descending
    /// so the UI surfaces the most-duplicated files first.
    ///
    /// Caller-supplied `filters` apply *before* grouping (e.g. scope
    /// to one folder, override IMAGE_EXTS).  No pagination on the
    /// outer wire shape — at the table sizes we expect (Tier 1 single
    /// user, mostly), the response fits comfortably in one Tauri RPC;
    /// if that ever changes we can switch to GROUP-BY pushdown
    /// without revving the API.
    async fn duplicates(&self, filters: ListFilters) -> Result<Vec<DuplicateGroup>>;

    /// Group image rows whose perceptual hashes are within
    /// `threshold` Hamming distance (slice A4).  Catches visual
    /// duplicates that the SHA-256 view misses: a JPEG and its
    /// resized copy share a pHash within the default threshold of 8
    /// but have different bytes (and thus different `source_hash`).
    ///
    /// Implementation in Tier 1 is on-demand: the local backend
    /// resolves each row's local path and decodes + hashes the file,
    /// then runs an O(N²) clustering pass.  Skips rows whose
    /// `location_uri` doesn't resolve to a local path (Tier 2 / drive
    /// rows) and rows whose hash compute fails (HEIC, missing files).
    async fn near_duplicates(
        &self,
        threshold: u32,
        filters: ListFilters,
    ) -> Result<Vec<NearDuplicateGroup>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_exts_match_spec() {
        // Exactly the set in docs/P13_Bilder_integration.md — guard
        // against drift via this assertion.
        let expected: std::collections::BTreeSet<&str> = [
            "jpg", "jpeg", "png", "webp", "heic", "heif", "tiff", "bmp",
        ]
        .into_iter()
        .collect();
        let actual: std::collections::BTreeSet<&str> = IMAGE_EXTS.iter().copied().collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn is_image_ext_case_insensitive() {
        assert!(is_image_ext("jpg"));
        assert!(is_image_ext("JPG"));
        assert!(is_image_ext("Jpeg"));
        assert!(is_image_ext("HEIC"));
        assert!(is_image_ext("tiff"));
        assert!(!is_image_ext("pdf"));
        assert!(!is_image_ext("gif"));   // deliberately excluded for v1
        assert!(!is_image_ext("svg"));   // deliberately excluded for v1
        assert!(!is_image_ext(""));
    }
}
