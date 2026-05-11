//! Tauri command surface for the Images vertical.
//!
//! Slice A1 shipped `images_list` + `images_default_extensions`.
//! Slice A2 adds `images_thumbnail` (PNG bytes for grid tiles) and
//! `images_exif` (curated EXIF subset for the preview pane), plus a
//! private `doc_id_to_local_path` resolver shared by both.
//!
//! `images_list` intentionally returns an empty page — never an error
//! — when the index isn't ready yet.  That mirrors the convention
//! `index_query_documents` set in P9 (see comment around its `(false,
//! _) | (true, None)` branch): the Images tab polls on mount, before
//! `init_index` finishes during a cold start, and erroring there
//! would surface as a wave of "Images list failed" log lines for
//! what is actually a clean empty state.
//!
//! The thumbnail + exif commands offload the (potentially slow) image
//! decode and the (always small) EXIF parse to a Tokio blocking pool
//! so they don't stall the runtime.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;

use super::{
    exif::{read_exif, ExifSummary},
    local::LocalImages,
    thumbnail::{generate_thumbnail, ThumbnailError, DEFAULT_THUMBNAIL_SIZE},
    types::{ImagesPage, ListFilters},
    ImagesBackend, IMAGE_EXTS,
};
use crate::index::local_index::LocalIndex;
use crate::AppState;

#[tauri::command]
pub async fn images_list(
    state: State<'_, AppState>,
    page_size: Option<i32>,
    cursor: Option<String>,
    filters: Option<ListFilters>,
) -> Result<ImagesPage, String> {
    let lock = state.index.lock().await;

    // P9 convention: index disabled OR not yet initialised → empty
    // page rather than an error.  See `index_query_documents` for the
    // matching pattern.
    let local_index = match (lock.config.enabled, lock.local.as_ref()) {
        (false, _) | (true, None) => {
            return Ok(ImagesPage {
                items: vec![],
                total: 0,
                next_cursor: None,
                page_size: page_size.unwrap_or(200),
            });
        }
        (true, Some(l)) => l.clone(),
    };
    drop(lock);

    let backend = LocalImages::new(local_index);
    backend
        .list(
            page_size.unwrap_or(200),
            cursor,
            filters.unwrap_or_default(),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Returns the canonical Tier-1 image extension list so the frontend
/// can render filter chips (and add `?` badges for unfamiliar
/// extensions) without duplicating the spec list in TypeScript.
#[tauri::command]
pub fn images_default_extensions() -> Vec<String> {
    IMAGE_EXTS.iter().map(|e| (*e).to_string()).collect()
}

// ── A2: thumbnail + EXIF surface ─────────────────────────────────────────

#[tauri::command]
pub async fn images_thumbnail(
    state: State<'_, AppState>,
    doc_id: String,
    size: Option<u32>,
) -> Result<Vec<u8>, String> {
    let local_index = require_local_index(&state).await?;
    let path = doc_id_to_local_path(&local_index, &doc_id)
        .await
        .map_err(|e| e.to_string())?;
    let target = size.unwrap_or(DEFAULT_THUMBNAIL_SIZE);

    // Image decode is CPU-bound — push to the blocking pool so the
    // single-threaded async runtime keeps servicing other Tauri RPCs
    // while a 24-MP file decodes.
    tauri::async_runtime::spawn_blocking(move || generate_thumbnail(&path, target))
        .await
        .map_err(|e| format!("thumbnail join: {e}"))?
        .map_err(|e: ThumbnailError| e.to_string())
}

#[tauri::command]
pub async fn images_exif(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<ExifSummary, String> {
    let local_index = require_local_index(&state).await?;
    let path = doc_id_to_local_path(&local_index, &doc_id)
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || read_exif(&path))
        .await
        .map_err(|e| format!("exif join: {e}"))?
        .map_err(|e| e.to_string())
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Pull `IndexState.local` out of `AppState`.  Returns `Err` instead
/// of an empty default — the thumbnail + EXIF commands have no
/// meaningful "no data" payload, so a typed error is the right shape
/// for the UI to fall back on the placeholder tile.
async fn require_local_index(
    state: &State<'_, AppState>,
) -> Result<Arc<LocalIndex>, String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("local index is disabled".into());
    }
    lock.local
        .as_ref()
        .cloned()
        .ok_or_else(|| "local index not initialised yet".into())
}

/// Look up `doc_id` in the local index and resolve the row's
/// `location_uri` to a real filesystem path.  Handles the schemes
/// CrispSorter actually writes (`crisp+local://`, `file://`, plain
/// absolute path) — explicitly does NOT try to materialise
/// `crisp+drive://` rows here, since drive download is a separate
/// pipeline (see `drive_*` commands).  The UI surfaces the failure
/// with a "remote source — open in CrispLens" CTA in slice B5.
pub(crate) async fn doc_id_to_local_path(
    local: &LocalIndex,
    doc_id: &str,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let rows = local
        .fetch_search_results_by_ids(&[doc_id.to_string()])
        .await
        .with_context(|| format!("fetch_search_results_by_ids({doc_id})"))?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("doc_id not found: {doc_id}"))?;

    let path = location_uri_to_local_path(&row.location_uri)
        .ok_or_else(|| anyhow::anyhow!(
            "location_uri is not a local path: {}",
            row.location_uri
        ))?;
    Ok(path)
}

/// Mirror of the TypeScript `uriToPath` in `IndexIngest.svelte`.
/// Kept here (rather than re-exported from a shared module) because
/// it's the single user of this helper on the Rust side; if a third
/// caller appears we promote it to `crate::index::uri`.
pub(crate) fn location_uri_to_local_path(uri: &str) -> Option<PathBuf> {
    if let Some(rest) = uri.strip_prefix("crisp+local://") {
        // The TS helper grabs everything after the first `/` past the
        // host, e.g. `crisp+local://host/Users/foo/x.jpg` →
        // `/Users/foo/x.jpg`.  Mirror that exactly.
        let slash = rest.find('/')?;
        Some(PathBuf::from(&rest[slash..]))
    } else if let Some(rest) = uri.strip_prefix("file://") {
        Some(PathBuf::from(rest))
    } else if uri.starts_with('/') || uri.chars().nth(1) == Some(':') {
        // Bare absolute path: POSIX or `C:\...` Windows.
        Some(PathBuf::from(uri))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_uri_to_local_path_handles_crisp_local() {
        assert_eq!(
            location_uri_to_local_path("crisp+local://host/Users/x/photo.jpg"),
            Some(PathBuf::from("/Users/x/photo.jpg"))
        );
        // The current writer omits the host and uses "local" as a
        // placeholder, producing two slashes after the scheme — the
        // resolver must still strip cleanly.
        assert_eq!(
            location_uri_to_local_path("crisp+local://local//Users/x/photo.jpg"),
            Some(PathBuf::from("//Users/x/photo.jpg"))
        );
    }

    #[test]
    fn location_uri_to_local_path_handles_file_scheme() {
        assert_eq!(
            location_uri_to_local_path("file:///Users/x/photo.jpg"),
            Some(PathBuf::from("/Users/x/photo.jpg"))
        );
    }

    #[test]
    fn location_uri_to_local_path_handles_bare_absolute_paths() {
        assert_eq!(
            location_uri_to_local_path("/Users/x/photo.jpg"),
            Some(PathBuf::from("/Users/x/photo.jpg"))
        );
        // Windows drive letter syntax.
        assert_eq!(
            location_uri_to_local_path("C:/Users/x/photo.jpg"),
            Some(PathBuf::from("C:/Users/x/photo.jpg"))
        );
    }

    #[test]
    fn location_uri_to_local_path_rejects_remote_schemes() {
        assert!(location_uri_to_local_path("crisp+drive://abc/path").is_none());
        assert!(location_uri_to_local_path("crisp+cb-archive://1/abc").is_none());
        assert!(location_uri_to_local_path("https://example.com/x.jpg").is_none());
        assert!(location_uri_to_local_path("relative/path.jpg").is_none());
    }
}
