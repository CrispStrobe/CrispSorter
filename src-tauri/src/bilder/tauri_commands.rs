//! Tauri command surface for the Bilder vertical.  Slice A1 ships a
//! single command (`bilder_list`) plus a metadata helper
//! (`bilder_default_extensions`) so the UI doesn't have to hard-code
//! the IMAGE_EXTS list.
//!
//! The command intentionally returns an empty page — never an error —
//! when the index isn't ready yet.  That mirrors the convention
//! `index_query_documents` set in P9 (see comment around its `(false,
//! _) | (true, None)` branch): the Bilder tab polls on mount, before
//! `init_index` finishes during a cold start, and erroring there would
//! surface as a wave of "Bilder list failed" log lines for what is
//! actually a clean empty state.

use tauri::State;

use super::{
    local::LocalBilder,
    types::{ImagesPage, ListFilters},
    BilderBackend, IMAGE_EXTS,
};
use crate::AppState;

#[tauri::command]
pub async fn bilder_list(
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

    let backend = LocalBilder::new(local_index);
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
pub fn bilder_default_extensions() -> Vec<String> {
    IMAGE_EXTS.iter().map(|e| (*e).to_string()).collect()
}
