//! Tier-1 `BilderBackend` impl — filters the existing LanceDB index
//! down to image rows.  Zero new dependencies; everything below
//! delegates to `crate::index::local_index::LocalIndex`.
//!
//! Pagination flows straight through: the opaque cursor we hand back to
//! the UI is the same string that LanceDB's `PageCursor` round-trips,
//! so when slice A2 swaps the in-process sort for a keyset cursor we
//! don't have to rev the wire format.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value as Json;

use crate::index::local_index::LocalIndex;
use crate::index::schema::{
    DocumentFilter, PageCursor, PageSpec, SortColumn, SortDir, SortSpec,
};

use super::{
    types::{HealthStatus, Image, ImagesPage, ListFilters},
    BilderBackend, IMAGE_EXTS,
};

/// Tier-1 backend.  Construct via [`LocalBilder::new`] from the
/// `Arc<LocalIndex>` held in `AppState::index`.
pub struct LocalBilder {
    index: Arc<LocalIndex>,
}

impl LocalBilder {
    pub fn new(index: Arc<LocalIndex>) -> Self {
        Self { index }
    }
}

/// Pull `fs_size` out of a SearchResult's `metadata_json` blob.
/// Mirrors the reader at `index/tauri_commands.rs:1453`.
fn extract_fs_size(metadata_json: Option<&str>) -> Option<i64> {
    let raw = metadata_json?;
    let v: Json = serde_json::from_str(raw).ok()?;
    v.get("fs_size").and_then(|x| x.as_i64()).filter(|s| *s >= 0)
}

#[async_trait]
impl BilderBackend for LocalBilder {
    async fn health(&self) -> Result<HealthStatus> {
        // Tier 1 health = "the local index is reachable".  We don't
        // probe LanceDB here — `LocalBilder` only exists when
        // `IndexState::local` is `Some`, so the open-handle invariant
        // is already enforced by the construction site.
        Ok(HealthStatus::Ok {
            version: env!("CARGO_PKG_VERSION").to_string(),
            face_engine: None,
        })
    }

    async fn list(
        &self,
        page_size: i32,
        cursor: Option<String>,
        filters: ListFilters,
    ) -> Result<ImagesPage> {
        // Resolve the ext list: caller-supplied override beats the
        // canonical Tier-1 set.  We always lower-case for comparison
        // because `DocumentFilter::ext` matches `ext IN (...)` against
        // already-lower-cased rows in LanceDB.
        let ext: Vec<String> = match filters.ext {
            Some(list) if !list.is_empty() => {
                list.into_iter().map(|e| e.to_lowercase()).collect()
            }
            _ => IMAGE_EXTS.iter().map(|e| (*e).to_string()).collect(),
        };

        // Clamp page_size into a reasonable window so a buggy / hostile
        // caller can't ask for the entire table.  Floor at 1 to avoid
        // an empty fetch loop on the UI side.
        let limit = page_size.clamp(1, 1000) as u32;

        let document_filter = DocumentFilter {
            parent_dir_prefix: filters.parent_dir_prefix,
            ext,
            owner_id: filters.owner_id,
            volume_ids: filters.volume_ids,
            ..Default::default()
        };

        // Newest-first by indexed_at — same default the Übersicht uses.
        let sort = SortSpec {
            column: SortColumn::IndexedAt,
            direction: SortDir::Desc,
        };
        let page = PageSpec {
            limit,
            cursor: cursor.map(PageCursor),
        };

        let result = self
            .index
            .query_documents(&document_filter, sort, page)
            .await
            .context("LocalBilder::list -> query_documents")?;

        let items: Vec<Image> = result
            .rows
            .into_iter()
            .map(|r| Image {
                doc_id: r.doc_id,
                location_uri: r.location_uri,
                filename: r.filename,
                ext: r.ext,
                size: extract_fs_size(r.metadata_json.as_deref()),
                indexed_at: r.indexed_at,
            })
            .collect();

        Ok(ImagesPage {
            items,
            total: result.total_estimate as i64,
            next_cursor: result.next_cursor.map(|c| c.0),
            page_size: limit as i32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_fs_size_reads_canonical_field() {
        let json = r#"{"fs_size":12345,"fs_mtime":1700000000}"#;
        assert_eq!(extract_fs_size(Some(json)), Some(12345));
    }

    #[test]
    fn extract_fs_size_rejects_negative() {
        // The canonical writer clamps at 0 (see ingest.rs); a negative
        // value here means a corrupt row -- treat it as missing.
        let json = r#"{"fs_size":-1}"#;
        assert_eq!(extract_fs_size(Some(json)), None);
    }

    #[test]
    fn extract_fs_size_handles_absent_or_malformed() {
        assert_eq!(extract_fs_size(None), None);
        assert_eq!(extract_fs_size(Some("")), None);
        assert_eq!(extract_fs_size(Some("not json")), None);
        assert_eq!(extract_fs_size(Some(r#"{"other":1}"#)), None);
        // String rather than number — common metadata-json drift.
        assert_eq!(extract_fs_size(Some(r#"{"fs_size":"123"}"#)), None);
    }
}
