//! Tier-1 `ImagesBackend` impl — filters the existing LanceDB index
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
    phash::{hamming_distance, phash_file},
    tauri_commands::location_uri_to_local_path,
    types::{
        phash_to_hex, DuplicateGroup, HealthStatus, Image, ImagesPage, ListFilters,
        NearDuplicateGroup, NearDuplicateItem,
    },
    ImagesBackend, IMAGE_EXTS,
};

/// Tier-1 backend.  Construct via [`LocalImages::new`] from the
/// `Arc<LocalIndex>` held in `AppState::index`.
pub struct LocalImages {
    index: Arc<LocalIndex>,
}

impl LocalImages {
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
impl ImagesBackend for LocalImages {
    async fn health(&self) -> Result<HealthStatus> {
        // Tier 1 health = "the local index is reachable".  We don't
        // probe LanceDB here — `LocalImages` only exists when
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
            .context("LocalImages::list -> query_documents")?;

        let items: Vec<Image> = result
            .rows
            .into_iter()
            .map(search_result_to_image)
            .collect();

        Ok(ImagesPage {
            items,
            total: result.total_estimate as i64,
            next_cursor: result.next_cursor.map(|c| c.0),
            page_size: limit as i32,
        })
    }

    async fn duplicates(
        &self,
        filters: ListFilters,
    ) -> Result<Vec<DuplicateGroup>> {
        // Walk all pages of `list(filters)` (1 000 rows per page —
        // the cap we apply server-side) and bucket by source_hash.
        // For Tier-1 fixtures (low-tens-of-k images) this is fine
        // memory-wise; if the user grows past that we push GROUP BY
        // into LanceDB SQL.  See the trait doc for the upgrade plan.
        const PAGE: i32 = 1000;

        let mut by_hash: std::collections::HashMap<String, Vec<Image>> =
            std::collections::HashMap::new();
        let mut cursor: Option<String> = None;

        loop {
            let page = self
                .list(PAGE, cursor.clone(), filters.clone())
                .await
                .context("LocalImages::duplicates -> list(page)")?;

            for img in page.items {
                // Skip rows with empty source_hash defensively — they
                // can appear in pre-source_hash rows during a schema
                // migration window.  Treat them as un-grouped rather
                // than as a single big "" cluster.
                if img.source_hash.is_empty() {
                    continue;
                }
                by_hash
                    .entry(img.source_hash.clone())
                    .or_default()
                    .push(img);
            }

            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        let mut groups: Vec<DuplicateGroup> = by_hash
            .into_iter()
            .filter_map(|(hash, items)| {
                if items.len() >= 2 {
                    Some(DuplicateGroup { source_hash: hash, items })
                } else {
                    None
                }
            })
            .collect();
        // Largest groups first — the UI shows the "9 copies of this
        // file" cluster before the "2 copies" ones.  Tie-break by
        // hash for deterministic ordering across runs.
        groups.sort_by(|a, b| {
            b.items
                .len()
                .cmp(&a.items.len())
                .then_with(|| a.source_hash.cmp(&b.source_hash))
        });
        Ok(groups)
    }

    async fn near_duplicates(
        &self,
        threshold: u32,
        filters: ListFilters,
    ) -> Result<Vec<NearDuplicateGroup>> {
        // Walk all pages of the filtered image set, then hash each row
        // off the runtime via spawn_blocking — image decode + DCT are
        // CPU-bound and we don't want to stall the async executor.
        const PAGE: i32 = 1000;
        let mut images: Vec<Image> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .list(PAGE, cursor.clone(), filters.clone())
                .await
                .context("LocalImages::near_duplicates -> list(page)")?;
            images.extend(page.items);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        // One blocking task per image — Tokio caps the blocking pool
        // at 512 by default; 99% of users won't have that many image
        // rows.  For larger indexes, a future slice can introduce a
        // proper FuturesUnordered with a manual concurrency cap.
        let join_set: Vec<_> = images
            .into_iter()
            .map(|img| {
                tokio::task::spawn_blocking(move || {
                    let phash = try_hash_row(&img);
                    (img, phash)
                })
            })
            .collect();

        let mut hashed: Vec<(Image, i64)> = Vec::new();
        for handle in join_set {
            let (img, phash) = handle
                .await
                .context("LocalImages::near_duplicates -> spawn_blocking join")?;
            if let Some(h) = phash {
                hashed.push((img, h));
            }
        }

        Ok(cluster_by_phash(hashed, threshold))
    }
}

fn search_result_to_image(r: crate::index::schema::SearchResult) -> Image {
    Image {
        doc_id: r.doc_id,
        location_uri: r.location_uri,
        filename: r.filename,
        ext: r.ext,
        size: extract_fs_size(r.metadata_json.as_deref()),
        indexed_at: r.indexed_at,
        source_hash: r.source_hash,
    }
}

/// Hash one image row.  Returns `None` for rows whose `location_uri`
/// doesn't resolve to a local path (Tier 2 / drive sources) and for
/// rows whose hash compute fails (HEIC, missing files, decode errors)
/// — we drop them silently rather than failing the whole near-dup
/// pass.  Lives at module scope because both `near_duplicates` and
/// the test fixture exercise the same code path.
fn try_hash_row(img: &Image) -> Option<i64> {
    let path = location_uri_to_local_path(&img.location_uri)?;
    phash_file(&path).ok()
}

/// Cluster `(image, phash)` rows by Hamming distance ≤ `threshold`
/// using a single-link union-find style sweep.  O(N²) which is fine
/// at Tier-1 scale; an LSH index is the obvious upgrade if we ever
/// need to handle millions of images.
fn cluster_by_phash(
    rows: Vec<(Image, i64)>,
    threshold: u32,
) -> Vec<NearDuplicateGroup> {
    let n = rows.len();
    if n < 2 {
        return Vec::new();
    }

    // parent[i] = index of the row representing the cluster i belongs to.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut root = i;
        while parent[root] != root {
            root = parent[root];
        }
        // Path compression.
        let mut cur = i;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if hamming_distance(rows[i].1, rows[j].1) <= threshold {
                let ri = find(&mut parent, i);
                let rj = find(&mut parent, j);
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }

    // Bucket rows by their cluster representative.
    let mut buckets: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        buckets.entry(root).or_default().push(i);
    }

    let mut groups: Vec<NearDuplicateGroup> = buckets
        .into_iter()
        .filter_map(|(_, idxs)| {
            if idxs.len() < 2 {
                return None;
            }
            // Representative = the row with the numerically smallest
            // hash (stable, hash-only, no time-of-arrival dependency).
            let rep_phash = idxs
                .iter()
                .map(|&i| rows[i].1)
                .min()
                .expect("non-empty");
            let mut items: Vec<NearDuplicateItem> = idxs
                .into_iter()
                .map(|i| {
                    let (img, phash) = &rows[i];
                    NearDuplicateItem {
                        image: img.clone(),
                        phash_hex: phash_to_hex(*phash),
                        distance_from_rep: hamming_distance(rep_phash, *phash),
                    }
                })
                .collect();
            // Closest-to-representative first inside each group.
            // Tie-break on the hex hash for stable ordering even when
            // multiple members are equidistant from the representative.
            items.sort_by(|a, b| {
                a.distance_from_rep
                    .cmp(&b.distance_from_rep)
                    .then_with(|| a.phash_hex.cmp(&b.phash_hex))
            });
            Some(NearDuplicateGroup {
                representative_phash_hex: phash_to_hex(rep_phash),
                items,
            })
        })
        .collect();

    // Largest groups first; tie-break by representative phash hex
    // for determinism across runs.
    groups.sort_by(|a, b| {
        b.items
            .len()
            .cmp(&a.items.len())
            .then_with(|| a.representative_phash_hex.cmp(&b.representative_phash_hex))
    });
    groups
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

    // ── Integration tests against a real LanceDB on a tempdir ─────────────

    use crate::index::schema::DocumentChunk;
    use crate::index::ingest::chunk_row_id;
    use tempfile::TempDir;

    /// Mint a single L1-style row (`chunk_index = -1`, no embedding,
    /// no full text) for a hypothetical file at `parent/filename.ext`.
    /// Matches the shape `IngestPipeline::ingest_l1` writes.
    fn l1_chunk(
        doc_id: &str,
        parent_dir: &str,
        filename: &str,
        ext: &str,
        size: i64,
        indexed_at: i64,
    ) -> DocumentChunk {
        let location_uri = format!("crisp+local://test/{parent_dir}/{filename}");
        let meta = serde_json::json!({
            "level":      1,
            "fs_size":    size,
            "parent_dir": parent_dir,
        });
        DocumentChunk {
            id:                chunk_row_id(doc_id, -1),
            doc_id:            doc_id.to_owned(),
            location_uri,
            owner_id:          "test-owner".to_owned(),
            filename:          Some(filename.to_owned()),
            title:             None,
            author:            None,
            year:              None,
            ext:               Some(ext.to_owned()),
            language:          None,
            page_count:        None,
            headings_text:    None,
            full_text:         None,
            full_text_md:      None,
            embedding:         None,
            embedding_sparse:  None,
            embedding_model:   None,
            chunk_index:       -1,
            chunk_total:       0,
            chunk_start_char:  None,
            chunk_end_char:    None,
            indexed_at,
            source_hash:       format!("hash-{doc_id}"),
            tags:              vec![],
            metadata_json:     Some(meta.to_string()),
            parent_dir:        Some(parent_dir.to_owned()),
            volume_id:         None,
            text_translated:   None,
            text_translated_lang: None,
            // Fixture-only DocumentChunks for image tests — no audio
            // metadata present.
            audio_duration_seconds: None,
            audio_codec: None,
            audio_sample_rate_hz: None,
            audio_channels: None,
            audio_bitrate_kbps: None,
        }
    }

    /// Build a fresh LocalIndex on a tempdir and seed it with a mixed
    /// fixture: 5 image rows across the canonical extension set + 3
    /// non-image rows (pdf / docx / md).  Returns both the backend and
    /// the holding TempDir so the caller's `_tmp` lifetime keeps the
    /// dir alive across the test.
    async fn fixture() -> (LocalImages, TempDir) {
        let tmp = TempDir::new().expect("tempdir");
        let local = std::sync::Arc::new(
            LocalIndex::open_or_create(tmp.path(), 1024)
                .await
                .expect("open_or_create"),
        );

        let chunks = vec![
            l1_chunk("img-jpg-1",  "/photos/2024", "sunset.jpg",   "jpg",  111_111, 1_700_000_001_000),
            l1_chunk("img-jpeg-1", "/photos/2024", "lake.jpeg",    "jpeg", 222_222, 1_700_000_002_000),
            l1_chunk("img-png-1",  "/photos/2025", "graph.png",    "png",  333_333, 1_700_000_003_000),
            l1_chunk("img-heic-1", "/photos/2025", "iphone.heic",  "heic", 444_444, 1_700_000_004_000),
            l1_chunk("img-bmp-1",  "/photos/scans", "x.bmp",       "bmp",  555_555, 1_700_000_005_000),
            l1_chunk("doc-pdf-1",  "/papers",      "thesis.pdf",   "pdf",  666_666, 1_700_000_006_000),
            l1_chunk("doc-docx-1", "/papers",      "memo.docx",    "docx", 777_777, 1_700_000_007_000),
            l1_chunk("doc-md-1",   "/notes",       "readme.md",    "md",   888_888, 1_700_000_008_000),
        ];
        local.ingest_batch(&chunks).await.expect("ingest_batch");

        (LocalImages::new(local), tmp)
    }

    #[tokio::test]
    async fn list_returns_only_image_rows_by_default() {
        let (backend, _tmp) = fixture().await;
        let page = backend
            .list(50, None, ListFilters::default())
            .await
            .expect("list");
        // 5 image rows seeded; 3 non-image rows seeded.
        assert_eq!(page.total, 5, "total should count image rows only");
        assert_eq!(page.items.len(), 5);
        for img in &page.items {
            let ext = img.ext.as_deref().unwrap_or("");
            assert!(
                IMAGE_EXTS.contains(&ext),
                "non-image row leaked through filter: {ext}"
            );
        }
        // Every seeded image row had a non-zero fs_size; verify the
        // metadata_json reader is plumbed through to the wire type.
        assert!(page.items.iter().all(|i| i.size.is_some_and(|s| s > 0)));
    }

    #[tokio::test]
    async fn list_respects_caller_supplied_ext_override() {
        let (backend, _tmp) = fixture().await;
        let only_jpg = backend
            .list(
                50,
                None,
                ListFilters {
                    ext: Some(vec!["jpg".to_owned()]),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert_eq!(only_jpg.total, 1);
        assert_eq!(only_jpg.items.len(), 1);
        assert_eq!(only_jpg.items[0].ext.as_deref(), Some("jpg"));
        assert_eq!(only_jpg.items[0].doc_id, "img-jpg-1");
    }

    #[tokio::test]
    async fn list_uppercase_ext_override_lowercases_for_match() {
        // Real-world callers (filter chips, manual flags) sometimes
        // hand us upper-case extensions.  LanceDB stores `ext` already
        // lower-cased, so we must lower-case the override before
        // building `ext IN (...)` -- otherwise the SQL match misses.
        let (backend, _tmp) = fixture().await;
        let result = backend
            .list(
                50,
                None,
                ListFilters {
                    ext: Some(vec!["JPG".to_owned(), "PNG".to_owned()]),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert_eq!(result.total, 2, "JPG + PNG should both match");
        let exts: std::collections::BTreeSet<String> = result
            .items
            .iter()
            .filter_map(|i| i.ext.clone())
            .collect();
        assert_eq!(
            exts,
            ["jpg", "png"]
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>()
        );
    }

    #[tokio::test]
    async fn list_paginates_via_opaque_cursor() {
        let (backend, _tmp) = fixture().await;
        // Page size 2 means page 1 = 2 items, page 2 = 2, page 3 = 1.
        let p1 = backend.list(2, None, ListFilters::default()).await.unwrap();
        assert_eq!(p1.items.len(), 2);
        assert_eq!(p1.total, 5);
        let cursor1 = p1.next_cursor.expect("page 1 should have a cursor");

        let p2 = backend
            .list(2, Some(cursor1), ListFilters::default())
            .await
            .unwrap();
        assert_eq!(p2.items.len(), 2);
        let cursor2 = p2.next_cursor.expect("page 2 should have a cursor");

        let p3 = backend
            .list(2, Some(cursor2), ListFilters::default())
            .await
            .unwrap();
        assert_eq!(p3.items.len(), 1, "last page is the remainder");
        // P9 step 5 returns no cursor on the final page.
        assert!(p3.next_cursor.is_none());

        // Sanity: the three pages cover the whole result set with no
        // duplicates.  Order is by `indexed_at DESC`, the LocalImages
        // default — newest first.
        let mut all: Vec<String> = p1
            .items
            .iter()
            .chain(p2.items.iter())
            .chain(p3.items.iter())
            .map(|i| i.doc_id.clone())
            .collect();
        let unique = all.iter().cloned().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), all.len(), "pages overlapped: {all:?}");
        all.sort();
        assert_eq!(all.len(), 5);
    }

    #[tokio::test]
    async fn list_scopes_to_parent_dir_prefix() {
        let (backend, _tmp) = fixture().await;
        // 2 image rows live under /photos/2024 (jpg + jpeg).
        let scoped = backend
            .list(
                50,
                None,
                ListFilters {
                    parent_dir_prefix: Some("/photos/2024".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(scoped.total, 2);
        for img in &scoped.items {
            assert!(
                img.location_uri.contains("/photos/2024/"),
                "row escaped folder scope: {}",
                img.location_uri
            );
        }
    }

    #[tokio::test]
    async fn list_returns_empty_page_when_no_rows_match() {
        let (backend, _tmp) = fixture().await;
        let page = backend
            .list(
                50,
                None,
                ListFilters {
                    ext: Some(vec!["tiff".to_owned()]), // not in fixture
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_orders_newest_first_by_indexed_at() {
        let (backend, _tmp) = fixture().await;
        let page = backend
            .list(50, None, ListFilters::default())
            .await
            .unwrap();
        // The fixture's image rows have indexed_at 1..5 (millisecond
        // increments); LocalImages defaults to IndexedAt DESC, so the
        // first row out should be the bmp (indexed_at = 5).
        assert_eq!(page.items.first().unwrap().doc_id, "img-bmp-1");
        assert_eq!(page.items.last().unwrap().doc_id,  "img-jpg-1");
    }

    #[tokio::test]
    async fn list_surfaces_source_hash_on_each_image() {
        // Regression: A1 dropped source_hash on the floor; A3 needs it
        // for the dup-grouping view, so every Image returned by `list`
        // must carry the SHA-256.
        let (backend, _tmp) = fixture().await;
        let page = backend.list(50, None, ListFilters::default()).await.unwrap();
        for img in &page.items {
            assert_eq!(img.source_hash, format!("hash-{}", img.doc_id),
                "source_hash missing or mangled on {}", img.doc_id);
        }
    }

    // ── A3: duplicate-grouping tests ─────────────────────────────────────

    /// Build a fixture LocalIndex that deliberately seeds duplicate
    /// `source_hash` values across image rows so we can exercise the
    /// grouping path:
    ///   - "shared-hash-A" appears 3 times (sunset.jpg, sunset_copy.jpg, sunset_again.jpg)
    ///   - "shared-hash-B" appears 2 times (lake.jpeg, lake_copy.jpeg)
    ///   - "uniq-1" / "uniq-2" each appear once (singletons → not a dup)
    ///   - one PDF with "shared-hash-A" too — must NOT pollute image dups
    ///   - one row with empty source_hash to exercise the skip-empty path
    async fn dup_fixture() -> (LocalImages, TempDir) {
        let tmp = TempDir::new().unwrap();
        let local = std::sync::Arc::new(
            LocalIndex::open_or_create(tmp.path(), 1024).await.unwrap(),
        );
        let mut chunk = |doc: &str, name: &str, ext: &str, hash: &str, ts: i64| {
            let mut c = l1_chunk(doc, "/photos", name, ext, 1234, ts);
            c.source_hash = hash.to_owned();
            c
        };
        let mut chunks = vec![
            chunk("img-1", "sunset.jpg",        "jpg", "shared-hash-A", 1_000_000_001_000),
            chunk("img-2", "sunset_copy.jpg",   "jpg", "shared-hash-A", 1_000_000_002_000),
            chunk("img-3", "sunset_again.jpg",  "jpg", "shared-hash-A", 1_000_000_003_000),
            chunk("img-4", "lake.jpeg",         "jpeg","shared-hash-B", 1_000_000_004_000),
            chunk("img-5", "lake_copy.jpeg",    "jpeg","shared-hash-B", 1_000_000_005_000),
            chunk("img-6", "alone.png",         "png", "uniq-1",        1_000_000_006_000),
            chunk("img-7", "lonely.bmp",        "bmp", "uniq-2",        1_000_000_007_000),
            // Different ext, same hash as the image dup-A — proves that
            // the IMAGE_EXTS filter narrows BEFORE we group.  If the
            // PDF leaked into the dup view we'd see 4 in group A.
            chunk("doc-1", "sunset_print.pdf",  "pdf", "shared-hash-A", 1_000_000_008_000),
        ];
        // One row with an empty source_hash — the production code may
        // see these during a schema migration window.  We skip them
        // rather than bucketing the world into one big "" cluster.
        let mut empty_hash_row = l1_chunk("img-empty", "/photos", "no_hash.jpg", "jpg", 4321, 1_000_000_009_000);
        empty_hash_row.source_hash = String::new();
        chunks.push(empty_hash_row);
        local.ingest_batch(&chunks).await.unwrap();
        (LocalImages::new(local), tmp)
    }

    #[tokio::test]
    async fn duplicates_groups_image_rows_by_source_hash() {
        let (backend, _tmp) = dup_fixture().await;
        let groups = backend.duplicates(ListFilters::default()).await.unwrap();
        // 2 dup groups: A (3 images) and B (2 images).  The PDF row
        // sharing hash-A must NOT bump the group to 4.
        assert_eq!(groups.len(), 2, "got groups: {groups:#?}");
        let by_hash: std::collections::HashMap<&str, &Vec<Image>> =
            groups.iter().map(|g| (g.source_hash.as_str(), &g.items)).collect();
        assert_eq!(by_hash.get("shared-hash-A").unwrap().len(), 3);
        assert_eq!(by_hash.get("shared-hash-B").unwrap().len(), 2);
        // Every clustered row is an image extension.
        for items in by_hash.values() {
            for img in *items {
                let ext = img.ext.as_deref().unwrap_or("");
                assert!(IMAGE_EXTS.contains(&ext), "non-image leaked: {ext}");
            }
        }
    }

    #[tokio::test]
    async fn duplicates_excludes_singletons() {
        let (backend, _tmp) = dup_fixture().await;
        let groups = backend.duplicates(ListFilters::default()).await.unwrap();
        // The two singleton hashes (uniq-1, uniq-2) must not appear.
        assert!(groups.iter().all(|g| g.source_hash != "uniq-1"));
        assert!(groups.iter().all(|g| g.source_hash != "uniq-2"));
    }

    #[tokio::test]
    async fn duplicates_orders_by_group_size_descending() {
        let (backend, _tmp) = dup_fixture().await;
        let groups = backend.duplicates(ListFilters::default()).await.unwrap();
        // Largest group first: hash-A (3) before hash-B (2).
        assert_eq!(groups.first().unwrap().source_hash, "shared-hash-A");
        assert!(groups.first().unwrap().items.len() >= groups.last().unwrap().items.len());
    }

    #[tokio::test]
    async fn duplicates_skips_rows_with_empty_source_hash() {
        // The fixture seeds one row with source_hash = "" — it must
        // not appear in any group, in particular not in a stray
        // single-element "" group.
        let (backend, _tmp) = dup_fixture().await;
        let groups = backend.duplicates(ListFilters::default()).await.unwrap();
        for g in &groups {
            assert!(!g.source_hash.is_empty(), "empty-hash group leaked: {g:?}");
            for img in &g.items {
                assert!(!img.source_hash.is_empty(), "empty-hash row leaked: {img:?}");
                assert_ne!(img.doc_id, "img-empty");
            }
        }
    }

    #[tokio::test]
    async fn duplicates_respects_caller_filters() {
        // Ext override = png → no PNG duplicates in fixture, so
        // expect zero groups.  Proves filters apply *before* grouping.
        let (backend, _tmp) = dup_fixture().await;
        let groups = backend
            .duplicates(ListFilters {
                ext: Some(vec!["png".to_owned()]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(groups.is_empty(), "got unexpected png dup groups: {groups:?}");
    }

    // ── A4: pHash near-duplicate clustering tests ────────────────────────

    /// `cluster_by_phash` is the union-find core; tests it directly
    /// with synthetic (image, phash) pairs so we can pin the
    /// algorithm's behaviour without running the image decoder.
    #[test]
    fn cluster_by_phash_returns_empty_below_two_inputs() {
        let img = |id: &str| Image {
            doc_id: id.to_owned(),
            location_uri: format!("file:///{id}"),
            filename: None,
            ext: None,
            size: None,
            indexed_at: 0,
            source_hash: String::new(),
        };
        assert!(cluster_by_phash(vec![], 8).is_empty());
        assert!(cluster_by_phash(vec![(img("a"), 0)], 8).is_empty());
    }

    #[test]
    fn cluster_by_phash_groups_within_threshold() {
        let img = |id: &str| Image {
            doc_id: id.to_owned(),
            location_uri: format!("file:///{id}"),
            filename: None,
            ext: None,
            size: None,
            indexed_at: 0,
            source_hash: String::new(),
        };
        // Pairwise distances:
        //   distance(0, 1) = 1, distance(0, 3) = 2, distance(1, 3) = 1
        //   distance(0, 0xFFFF_FFFF) = 32 — far enough at threshold 2.
        let groups = cluster_by_phash(
            vec![
                (img("a"), 0),
                (img("b"), 1),
                (img("c"), 3),
                (img("d"), 0xFFFF_FFFF),
            ],
            2,
        );
        // One cluster of 3 (a/b/c), the far one is a singleton (excluded).
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.items.len(), 3);
        let ids: std::collections::BTreeSet<&str> =
            g.items.iter().map(|i| i.image.doc_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"].into_iter().collect());
        // Representative pHash = numerically smallest in the cluster (0).
        assert_eq!(g.representative_phash_hex, "0000000000000000");
        // distance_from_rep = popcount(rep ^ phash).  Recover phash
        // from the hex form to verify.
        let rep = super::super::types::phash_from_hex(&g.representative_phash_hex).unwrap();
        for it in &g.items {
            let h = super::super::types::phash_from_hex(&it.phash_hex).unwrap();
            assert_eq!(it.distance_from_rep, (rep ^ h).count_ones());
        }
    }

    #[test]
    fn cluster_by_phash_orders_groups_by_size_descending() {
        let img = |id: &str| Image {
            doc_id: id.to_owned(),
            location_uri: format!("file:///{id}"),
            filename: None,
            ext: None,
            size: None,
            indexed_at: 0,
            source_hash: String::new(),
        };
        // Two clusters: small (2) at hash 0 and big (3) at far hash.
        let groups = cluster_by_phash(
            vec![
                (img("s1"), 0),
                (img("s2"), 1),
                (img("b1"), 0xFFFF),
                (img("b2"), 0xFFFE),
                (img("b3"), 0xFFFD),
            ],
            2,
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].items.len(), 3, "biggest cluster first");
        assert_eq!(groups[1].items.len(), 2);
    }

    /// End-to-end near-duplicate test: synthesise actual image files
    /// on disk, ingest the L1 chunks pointing at those paths, then
    /// run `near_duplicates`.  Verifies the full pipeline (URI →
    /// path → decode → hash → cluster → wire shape).
    ///
    /// **Fixture choice matters here.**  pHash is degenerate on
    /// uniform images and on patterns whose period is much smaller
    /// than the 8×8 downsample grid (both end up "all-uniform" in
    /// hash space).  Use a content-rich gradient for the cluster
    /// fixture and a diagonal-split for the outlier — both have real
    /// intra-image variation so the DCT-pHash can discriminate.
    #[tokio::test]
    async fn near_duplicates_clusters_real_image_files() {
        use image::{ImageBuffer, Rgb};

        let tmp = TempDir::new().unwrap();
        // Three resizes of the same gradient — DCT-pHash should agree
        // within the default threshold across the size variants.
        let gradient = |side: u32, name: &str| -> std::path::PathBuf {
            let img = ImageBuffer::from_fn(side, side, |x, y| {
                let r = (x as f32 / side as f32 * 255.0) as u8;
                let g = (y as f32 / side as f32 * 255.0) as u8;
                Rgb([r, g, 64u8])
            });
            let p = tmp.path().join(name);
            img.save(&p).unwrap();
            p
        };
        let a = gradient(256, "grad_a.png");
        let b = gradient(128, "grad_b.png");
        let c = gradient(96,  "grad_c.png");
        // Diagonal-split outlier — top-left dark, bottom-right bright.
        // Genuinely different visual signature (the "very_different_"
        // unit test pins that this distinguishes from uniform).
        let split_path = tmp.path().join("split.png");
        let split = ImageBuffer::from_fn(128u32, 128u32, |x, y| {
            if x + y < 128 { Rgb([5u8, 5u8, 5u8]) } else { Rgb([250u8, 250u8, 250u8]) }
        });
        split.save(&split_path).unwrap();

        // Ingest L1 chunks pointing at those real files.  Use the
        // bare-absolute-path scheme (no `crisp+local://` prefix) since
        // location_uri_to_local_path handles both.
        let local = std::sync::Arc::new(
            LocalIndex::open_or_create(tmp.path(), 1024).await.unwrap(),
        );
        let row = |doc: &str, p: &std::path::Path, ts: i64| {
            let mut c = l1_chunk(doc, "/synth", p.file_name().unwrap().to_str().unwrap(), "png", 0, ts);
            c.location_uri = p.to_string_lossy().into_owned();
            c
        };
        local.ingest_batch(&[
            row("near-a", &a, 1),
            row("near-b", &b, 2),
            row("near-c", &c, 3),
            row("far",    &split_path, 4),
        ]).await.unwrap();

        let backend = LocalImages::new(local);
        let groups = backend
            .near_duplicates(8, ListFilters::default())
            .await
            .unwrap();

        assert_eq!(groups.len(), 1, "expected one near-dup cluster, got: {groups:#?}");
        let g = &groups[0];
        assert_eq!(g.items.len(), 3, "expected the 3 gradient rows clustered, got: {g:#?}");
        let ids: std::collections::BTreeSet<&str> =
            g.items.iter().map(|i| i.image.doc_id.as_str()).collect();
        assert_eq!(ids, ["near-a", "near-b", "near-c"].into_iter().collect());
        // Diagonal-split row must NOT be in the cluster.
        for it in &g.items {
            assert_ne!(it.image.doc_id, "far");
        }
    }
}
