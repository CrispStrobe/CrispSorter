/// Arrow schema for the LanceDB table and helper types for document chunks.
///
/// One row = one chunk (chunk_index >= 0).
/// A whole-document metadata row uses chunk_index = -1.
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Build the Arrow schema for the `documents` LanceDB table.
/// The embedding dimension is runtime-configurable (depends on the chosen model).
pub fn build_schema(embedding_dims: usize) -> Arc<Schema> {
    let embedding_field = Field::new(
        "embedding",
        DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float32, true)),
            embedding_dims as i32,
        ),
        true,
    );

    Arc::new(Schema::new(vec![
        // ── Identity ─────────────────────────────────────────────────────
        Field::new("id", DataType::Utf8, false), // SHA-256(doc_id + chunk_index)
        Field::new("doc_id", DataType::Utf8, false), // SHA-256 of file content
        Field::new("location_uri", DataType::Utf8, false), // crisp+* URI
        Field::new("owner_id", DataType::Utf8, false), // user UUID (for multi-user filter)
        // ── Document metadata ─────────────────────────────────────────────
        Field::new("filename", DataType::Utf8, true),
        Field::new("title", DataType::Utf8, true),
        Field::new("author", DataType::Utf8, true),
        Field::new("year", DataType::Int32, true),
        Field::new("ext", DataType::Utf8, true),
        Field::new("language", DataType::Utf8, true), // "de" | "en" | "de+en"
        Field::new("page_count", DataType::Int32, true),
        // ── Text content ──────────────────────────────────────────────────
        Field::new("headings_text", DataType::Utf8, true), // all headings joined (FTS boosted)
        Field::new("full_text", DataType::Utf8, true),     // stripped plain text (FTS + embedding)
        Field::new("full_text_md", DataType::Utf8, true),  // Markdown with structure (display)
        // ── Embedding ─────────────────────────────────────────────────────
        embedding_field,
        Field::new("embedding_sparse", DataType::Utf8, true), // JSON {indices:[], values:[]}
        Field::new("embedding_model", DataType::Utf8, true),  // model ID string
        // ── Chunking ──────────────────────────────────────────────────────
        Field::new("chunk_index", DataType::Int32, false), // -1 = whole-doc metadata row
        Field::new("chunk_total", DataType::Int32, false),
        Field::new("chunk_start_char", DataType::Int32, true),
        Field::new("chunk_end_char", DataType::Int32, true),
        // ── Provenance ───────────────────────────────────────────────────
        Field::new(
            "indexed_at",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("source_hash", DataType::Utf8, false), // hash of original file bytes
        Field::new(
            "tags",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        // ── Forward-compatibility escape hatch ────────────────────────────
        // Store anything not yet in the schema here as JSON.
        // Future location types, batch IDs, session IDs, Internxt-zip archive
        // paths, CrispSorter session links, etc. all live here without schema
        // migrations. Once a field stabilises, promote it to a first-class column.
        Field::new("metadata_json", DataType::Utf8, true),
        // ── P9 step 3 — promoted from metadata_json ───────────────────────
        // Scalar-indexed for fast folder-prefix filtering in query_documents.
        // New column appended at the end so existing tables are migrated
        // non-destructively via ALTER TABLE ADD COLUMN (all-null backfill).
        Field::new("parent_dir", DataType::Utf8, true),
        // ── P9 step 7 — promoted from metadata_json ───────────────────────
        // volume_id for offline-volume filtering; same migration strategy.
        Field::new("volume_id", DataType::Utf8, true),
    ]))
}

// ── Document chunk types used across the index module ─────────────────────

/// Full representation of a document chunk as it flows through the ingest pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    // Identity
    pub id: String,
    pub doc_id: String,
    pub location_uri: String,
    pub owner_id: String,

    // Document metadata
    pub filename: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub ext: Option<String>,
    pub language: Option<String>,
    pub page_count: Option<i32>,

    // Text content
    pub headings_text: Option<String>,
    pub full_text: Option<String>,
    pub full_text_md: Option<String>,

    // Embedding (filled by embedder step)
    pub embedding: Option<Vec<f32>>,
    pub embedding_sparse: Option<String>, // JSON
    pub embedding_model: Option<String>,

    // Chunking
    pub chunk_index: i32,
    pub chunk_total: i32,
    pub chunk_start_char: Option<i32>,
    pub chunk_end_char: Option<i32>,

    // Provenance
    pub indexed_at: i64, // Unix ms
    pub source_hash: String,
    pub tags: Vec<String>,

    // Escape hatch
    pub metadata_json: Option<String>,

    // P9 step 3 — promoted from metadata_json; scalar-indexed for fast folder prefix filter
    pub parent_dir: Option<String>,
    // P9 step 7 — promoted from metadata_json; scalar-indexed for volume-availability filter
    pub volume_id: Option<String>,
}

/// Lightweight search result returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub doc_id: String,
    pub location_uri: String,
    pub owner_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub filename: Option<String>,
    pub ext: Option<String>,
    pub language: Option<String>,
    /// Snippet of matched text with the hit context (up to 400 chars).
    pub snippet: String,
    /// Relevance score (higher = better). Units vary by search mode.
    pub score: f32,
    pub chunk_index: i32,
    /// Forward-compatibility blob from the row's `metadata_json` column.
    /// Frontend reads `level`, `fs_size`, `fs_mtime`, etc. from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
    /// Set on catalog-channel hits (P6 Phase 4c) to the source `.caf`
    /// path. `None` for ordinary documents-table hits. Frontend uses
    /// this to render a `[catalog: <name>]` badge alongside the regular
    /// metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_source: Option<String>,
    /// PLAN P7.6 follow-up — the source volume's stable id (macOS
    /// `diskutil` UUID, Linux blkid UUID, Windows volume serial),
    /// read from the `volume_id` column (P9 step 7). `None` for rows
    /// ingested before P7.6 landed, for path-less ingests, or for any
    /// row whose volume helper failed at ingest time. The
    /// `index_search` caller drops hits whose `volume_id` is `Some`
    /// and not in the currently-mounted set, unless
    /// `include_unmounted` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_id: Option<String>,
    /// Unix milliseconds when this row was indexed. Used by
    /// `SortColumn::IndexedAt` (newest first by default).
    /// `#[serde(default)]` so existing JSON without this field deserialises
    /// as 0 rather than failing.
    #[serde(default)]
    pub indexed_at: i64,
}

/// Pre-filter parameters applied before ANN / BM25 scoring.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    pub owner_id: Option<String>,
    pub language: Option<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub tags: Vec<String>,
}

impl SearchFilters {
    /// Build a LanceDB SQL `WHERE` clause fragment from the active filters.
    /// Returns `None` if no filters are set (no WHERE clause needed).
    pub fn to_lance_sql(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref oid) = self.owner_id {
            parts.push(format!("owner_id = '{}'", oid.replace('\'', "''")));
        }
        if let Some(ref lang) = self.language {
            parts.push(format!("language = '{}'", lang.replace('\'', "''")));
        }
        if let Some(ymin) = self.year_min {
            parts.push(format!("year >= {}", ymin));
        }
        if let Some(ymax) = self.year_max {
            parts.push(format!("year <= {}", ymax));
        }
        if !parts.is_empty() {
            Some(parts.join(" AND "))
        } else {
            None
        }
    }
}

// ── Document query API (PLAN P9 Übersicht scaling) ───────────────────────
//
// `SearchFilters` above is the *retrieval-side* filter (applied around
// FTS / ANN scoring). The catalog overview pane needs a richer
// browse-side filter that's index-friendly: parent-folder prefix,
// extension multi-select, name substring, level, completeness flags,
// optional doc_id allowlist (for "Show in Catalog" from a search hit).
// This block contains the API contract; the implementation lives in
// `LocalIndex::query_documents`.

/// Browse-side filter for `index_query_documents`.
///
/// All fields are optional / additive — a `Default::default()` filter
/// matches every documents-table row (modulo the implicit
/// `chunk_index <= 0` predicate that selects L1 metadata rows + L3
/// representative rows, exactly like `list_documents`).
///
/// Every field is `#[serde(default)]` so the frontend can omit any
/// field it doesn't want to constrain. Without this, the frontend has
/// to send a complete payload with empty arrays / nulls everywhere or
/// the Tauri command fails with `missing field 'ext'` etc. -- which
/// is exactly the bug we surfaced as "Übersicht stuck on Lade…".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DocumentFilter {
    /// Match rows whose `parent_dir` column starts with this prefix.
    /// Empty / `None` = any folder.
    pub parent_dir_prefix: Option<String>,
    /// Lowercased extensions (without leading dot) to keep, e.g.
    /// `["pdf", "docx"]`. Empty = any extension.
    pub ext: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    /// ISO 639-1 code, e.g. "de" or "en". Matches `language = ?`.
    pub language: Option<String>,
    /// Filter by analysis level. `None` = all levels.
    /// L1 = filesystem only (`chunk_index = -1`)
    /// L2 = embedded metadata (also `chunk_index = -1`, with non-empty
    ///      `metadata_json` carrying L2 fields)
    /// L3 = full text + embedding (`chunk_index = 0` row exists)
    pub level: Option<u8>,
    /// Case-insensitive substring search on filename / title.
    pub name_substring: Option<String>,
    /// Restrict to a fixed doc_id allowlist (used by the
    /// "Show in Catalog" pipeline that pipes search-hit doc_ids
    /// into the Übersicht pane).
    pub doc_ids: Option<Vec<String>>,
    /// Multi-user filter — usually pulled from auth state in lib.rs.
    pub owner_id: Option<String>,
    /// Volume awareness (P7.6) — drop hits whose `metadata_json.volume_id`
    /// isn't in this allowlist. `None` = volume filter disabled.
    pub volume_ids: Option<Vec<String>>,
}

/// Sort column for `index_query_documents`. Matches a real LanceDB
/// column when possible (cheap, scalar-index-friendly) and falls back
/// to a `metadata_json` field for properties that haven't been
/// promoted yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortColumn {
    Filename,
    Title,
    Author,
    Year,
    Language,
    /// Always present — when nothing else is specified, this is the
    /// stable default (newest first).
    IndexedAt,
    /// P9 step 3 — now a real column with a BTree scalar index.
    ParentDir,
}

impl Default for SortColumn {
    fn default() -> Self {
        SortColumn::IndexedAt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    #[serde(default)]
    pub column: SortColumn,
    #[serde(default)]
    pub direction: SortDir,
}

/// Pagination cursor — opaque to the frontend. Uses offset-based pagination;
/// the offset is encoded as a decimal string so it round-trips through the
/// Tauri serde boundary cleanly. DB-side ordering via `lance::Scanner` means
/// the 50k-row cap is gone and this is now O(1) at any offset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PageCursor(pub String);

impl PageCursor {
    pub fn from_offset(offset: u32) -> Self {
        PageCursor(offset.to_string())
    }
    pub fn offset(&self) -> u32 {
        self.0.parse().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSpec {
    /// Page size, capped at 1000 server-side. Frontend default 200.
    #[serde(default = "default_page_limit")]
    pub limit: u32,
    /// `None` = first page.
    #[serde(default)]
    pub cursor: Option<PageCursor>,
}

fn default_page_limit() -> u32 {
    200
}

/// Server response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPage {
    pub rows: Vec<SearchResult>,
    /// `None` when fewer than `limit` rows were returned (we hit the
    /// end of the result set). Otherwise pass this back unchanged in
    /// the next `PageSpec.cursor` to fetch the next page.
    pub next_cursor: Option<PageCursor>,
    /// Total rows matching `filter` regardless of `page`. Computed via
    /// `count_rows(filter_sql)` — a single scalar query against the
    /// same predicate, so it's cheap (no row materialisation).
    pub total_estimate: u64,
}

/// One node in the lazy-loaded folder tree (`index_folder_children`).
///
/// `doc_count` is the total number of documents in the entire subtree rooted at
/// `path` — not just direct children. This lets the UI render a badge like
/// "Papers (347)" without issuing a recursive count query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderChild {
    /// The folder's last path component, e.g. `"Papers"`.
    pub name: String,
    /// The full parent_dir value that should become the next
    /// `parentDirPrefix` when the user clicks this node.
    pub path: String,
    /// Total document rows whose `parent_dir` starts with `path`.
    pub doc_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_embedding_field() {
        let s = build_schema(1024);
        let field = s.field_with_name("embedding").unwrap();
        assert!(matches!(
            field.data_type(),
            DataType::FixedSizeList(_, 1024)
        ));
    }

    #[test]
    fn schema_has_escape_hatch() {
        let s = build_schema(384);
        assert!(s.field_with_name("metadata_json").is_ok());
    }

    #[test]
    fn filters_sql_none_when_empty() {
        let f = SearchFilters::default();
        assert!(f.to_lance_sql().is_none());
    }

    #[test]
    fn filters_sql_combined() {
        let f = SearchFilters {
            owner_id: Some("uuid-abc".to_owned()),
            year_min: Some(1950),
            year_max: Some(2000),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("owner_id"));
        assert!(sql.contains("year >= 1950"));
        assert!(sql.contains("year <= 2000"));
    }

    #[test]
    fn page_cursor_offset_round_trip() {
        for offset in [0u32, 1, 200, 999, 1_000_000] {
            assert_eq!(PageCursor::from_offset(offset).offset(), offset);
        }
    }

    #[test]
    fn page_cursor_offset_garbage_falls_back_to_zero() {
        // Forward-compat: a cursor minted by a future keyset-based
        // implementation should be treated as "first page" by an old
        // offset-based reader rather than crash.
        assert_eq!(PageCursor("not-a-number".to_owned()).offset(), 0);
    }
}
