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
        // ── P13.5 Phase 8 batch — index-time translation ─────────────────
        // `text_translated` carries the full-doc translation produced
        // by the extractor's MT pass (when `ExtractOptions::translate_to`
        // was supplied).  Replicated across every chunk row of the
        // same doc, matching the existing `full_text_md` convention.
        // Added on existing tables via the
        // `AddTextTranslatedColumns` migration in `index/migrations.rs`.
        Field::new("text_translated", DataType::Utf8, true),
        // ISO 639-1 target language of `text_translated`.  Lets the
        // search side filter / facet on the available translation
        // language without scanning text.
        Field::new("text_translated_lang", DataType::Utf8, true),
        // ── P13.6 Step 3c / 7 — audio L2 metadata ─────────────────────────
        // Populated by bg_ingest from ExtractedDocument.audio (symphonia
        // probe, no decode pass).  Replicated across every chunk row of
        // the same doc, matching the convention used for full_text_md /
        // text_translated.  All nullable because (a) non-audio rows
        // simply leave them NULL, (b) symphonia doesn't always expose
        // every datapoint (VBR mp3 with no n_frames, truncated m4a, …).
        // Added on existing tables via the `AddAudioMetadataColumns`
        // migration in `index/migrations.rs` (v101).
        Field::new("audio_duration_seconds", DataType::Float64, true),
        Field::new("audio_codec", DataType::Utf8, true),
        Field::new("audio_sample_rate_hz", DataType::Int32, true),
        Field::new("audio_channels", DataType::Int32, true),
        Field::new("audio_bitrate_kbps", DataType::Int32, true),
        // ── P13.6 Step 9 — image L2 (EXIF) ────────────────────────────
        // Curated subset of the kamadak-exif tags surfaced by the
        // P13 Bilder preview pane.  5 columns rather than the full
        // ExifSummary so search-time scalar filters work directly
        // ("show me photos shot on a Canon EOS R6" / "after 2020").
        // The full ExifSummary stays accessible via the P13 Bilder
        // tab; the index columns are the minimum needed for search.
        // Populated by the OCR extractor when feature `paddle-ocr`
        // / `ocrs` / tesseract returns successfully, or NULL when
        // EXIF can't be parsed.  Added on existing tables via the
        // `AddImageMetadataColumns` migration in
        // `index/migrations.rs` (v102).
        Field::new("image_camera_make", DataType::Utf8, true),
        Field::new("image_camera_model", DataType::Utf8, true),
        Field::new("image_lens_model", DataType::Utf8, true),
        Field::new("image_taken_at_unix", DataType::Int64, true),
        Field::new("image_iso", DataType::Int32, true),
        // Stage AD — ColBERT multi-vector retrieval (v105 migration).
        // multivec_packed: raw little-endian f32 bytes, n_tokens × dim × 4.
        // multivec_n_tokens: number of ColBERT token vectors packed.
        // Both NULL for models without a ColBERT head (all non-BGE-M3 models).
        Field::new("multivec_packed", DataType::LargeBinary, true),
        Field::new("multivec_n_tokens", DataType::Int16, true),
        // v106 — Original source URL the document came from.  Populated
        // from YAML frontmatter (`url:`) by the markdown extractor,
        // from XMP `/URL` for browser-saved PDFs, and from `dc:source`
        // for EPUB / DOCX captures.  NULL for local-only files that
        // never had a source URL.  Added on existing tables by the
        // `AddUrlColumn` migration in `index/migrations.rs`.
        Field::new("url", DataType::Utf8, true),
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

    // P13.5 Phase 8 batch — translation of full_text produced by the
    // extractor's post-dispatch MT pass.  Replicated across every
    // chunk of a doc (same convention as full_text_md); `None` when
    // no translation was attempted or MT failed.
    pub text_translated: Option<String>,
    pub text_translated_lang: Option<String>,

    // P13.6 Step 7 — audio L2 metadata (symphonia probe).  Populated
    // by bg_ingest from ExtractedDocument.audio for audio/video
    // extensions; replicated across every chunk row matching the
    // text_translated convention.  All Option-shaped because (a)
    // non-audio docs leave them None, (b) symphonia doesn't always
    // expose every datapoint.  Lands in the audio_* LanceDB columns
    // via the AddAudioMetadataColumns migration (v101).
    pub audio_duration_seconds: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate_hz: Option<i32>,
    pub audio_channels: Option<i32>,
    pub audio_bitrate_kbps: Option<i32>,

    // P13.6 Step 9 — image L2 (EXIF).  Populated by the OCR
    // extractor for images; `None` for non-image rows.  Lands in
    // the image_* LanceDB columns added by migration v102.
    pub image_camera_make: Option<String>,
    pub image_camera_model: Option<String>,
    pub image_lens_model: Option<String>,
    pub image_taken_at_unix: Option<i64>,
    pub image_iso: Option<i32>,

    // Stage AD — ColBERT multi-vector retrieval (v105 migration).
    // Packed as little-endian f32 bytes (n_tokens × dim × 4).
    // None for models without a ColBERT head; skipped in JSON
    // serialization (transient ingest-pipeline field).
    #[serde(skip)]
    pub multivec_packed: Option<Vec<u8>>,
    #[serde(skip)]
    pub multivec_n_tokens: Option<i16>,

    // v106 — Original source URL the document came from (YAML
    // frontmatter `url:`, PDF /URL, EPUB dc:source, etc.).  NULL
    // for files with no provenance URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
    /// SHA-256 of the original file bytes. Promoted onto SearchResult
    /// in the P13/A3 image-duplicate work so the dup-grouping view in
    /// `crate::images::local::LocalImages::duplicates` can bucket by
    /// hash without a second roundtrip. Empty string for synthesised
    /// results that didn't come from the LanceDB row scanner (catalog
    /// channel, FTS-only candidates, the test mk_result helper).
    /// `#[serde(default)]` keeps existing JSON payloads valid.
    #[serde(default)]
    pub source_hash: String,

    /// P13.5 Phase 8 batch — translation of `full_text` written by
    /// the extractor's MT pass at index time.  `None` for rows
    /// ingested without `ExtractOptions::translate_to`, or before
    /// the `AddTextTranslatedColumns` migration ran (the migration
    /// backfills nulls so old rows just look untranslated, which is
    /// what they are).  `#[serde(default)]` keeps existing payloads
    /// valid against the new field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_translated: Option<String>,
    /// ISO 639-1 target language of `text_translated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_translated_lang: Option<String>,
    /// v106 — source URL provenance, read from the `url` column (markdown
    /// frontmatter `url:`, PDF `/URL`, EPUB `dc:source`, …).  `None` for rows
    /// ingested before v106 or without a source URL.  Surfaced so the
    /// frontend / unified `search` verb can render an "Open original" link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// v107 — structured tag list, read from the `tags` `List<Utf8>` column.
    /// Empty for rows with no tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Pre-filter parameters applied before ANN / BM25 scoring.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    pub owner_id: Option<String>,
    pub language: Option<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub tags: Vec<String>,
    /// P13.5 follow-up — when set, restrict results to rows whose
    /// `text_translated_lang` matches this ISO 639-1 code AND whose
    /// `text_translated` column is non-null.  Lets a cross-language
    /// search say "give me Bosnian / Korean / etc. documents that
    /// have been pre-translated to English at index time" without
    /// the caller having to construct the SQL by hand.
    ///
    /// Has no effect on rows ingested without
    /// `ExtractOptions::translate_to` (their `text_translated` is
    /// null → filtered out).  Mostly orthogonal to [`Self::language`],
    /// which targets the SOURCE language column; this targets the
    /// TARGET (post-translation) column.  A typical "English-only
    /// search corpus" query would set `prefer_translated_lang =
    /// Some("en")` without touching `language` so docs in any source
    /// language land in the result set as long as they have an
    /// English translation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_translated_lang: Option<String>,
    /// P13.7 Step 6 — restrict to rows whose `ext` column matches one
    /// of the given values (lowercased on insert).  Multi-select to
    /// support `--ext pdf,docx,mp3` style CLI flags.  Empty Vec ==
    /// no filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ext: Vec<String>,
    /// P13.7 Step 6 — SHA-256 prefix match against `source_hash`.
    /// Mirrors cloud-backup's `--hash PREFIX` flag.  None == no
    /// filter.  Hex-only (no auto-escape — caller is responsible
    /// for sanitisation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash_prefix: Option<String>,
    /// P13.7 Step 6 — folder-prefix match against `parent_dir`.
    /// Already used by the Übersicht's DocumentFilter; surfacing
    /// it on SearchFilters lets CLI / search-side callers reuse
    /// the scalar-indexed column without rebuilding the SQL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_dir_prefix: Option<String>,
    /// P13.7 Step 6 — audio duration range (seconds).  Closed
    /// interval; either bound is independently optional.  Filters
    /// against the `audio_duration_seconds` column added by
    /// migration v101.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_duration_min_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_duration_max_seconds: Option<f64>,
    /// P13.7 Step 6 — image EXIF facets.  Substring-match against
    /// the `image_camera_make` / `image_camera_model` columns added
    /// by migration v102.  Stored values are typically short ("Apple",
    /// "iPhone 15 Pro") so the user can pass either an exact value
    /// or a substring fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_camera_make: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_camera_model: Option<String>,
    /// Stage AE follow-up — when set, run ColBERT MaxSim re-ranking on
    /// the top-K candidates before any cross-encoder reranker fires.
    /// Requires a model with a ColBERT head (BGE-M3 GGUF today) and
    /// rows ingested at or after schema v105 — gracefully degrades to a
    /// no-op otherwise (the re-rank only fires for rows that carry
    /// `multivec_packed` data).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub colbert_rerank: bool,
    /// v106 — substring match against the `url` column.  A user-typed
    /// `--url-domain spiegel.de` becomes `url LIKE '%spiegel.de%'`,
    /// which catches `https://www.spiegel.de/...` AND any URL where
    /// the domain appears as a substring (handles subdomains
    /// transparently).  `None` == no filter; pre-v106 rows have NULL
    /// url and are simply excluded when the filter is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_domain: Option<String>,
    /// v107 — element-of match on the `tags` list (Arrow `List<Utf8>`).
    /// Translates to `array_has(tags, '<value>')` on Lance's
    /// DataFusion SQL.  Mirrors `HybridSearchFilters.tag` on the
    /// cb-api side so a user gets the same semantics whether they
    /// search local or federated.  Pre-v107 rows have NULL tags
    /// and drop out when this is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
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
        if let Some(ref tgt) = self.prefer_translated_lang {
            parts.push(format!(
                "text_translated_lang = '{}' AND text_translated IS NOT NULL",
                tgt.replace('\'', "''")
            ));
        }
        // P13.7 Step 6 — ext multi-select.  Builds `ext IN ('pdf',
        // 'docx', …)` rather than `ext = X OR ext = Y` so LanceDB's
        // scalar-index path on `ext` fires.
        if !self.ext.is_empty() {
            let quoted: Vec<String> = self
                .ext
                .iter()
                .map(|e| format!("'{}'", e.to_lowercase().replace('\'', "''")))
                .collect();
            parts.push(format!("ext IN ({})", quoted.join(", ")));
        }
        if let Some(ref h) = self.source_hash_prefix {
            // SHA-256 is hex only; escape just in case a caller
            // forgets the validation.  LIKE 'prefix%' uses LanceDB's
            // string-index when prefix is non-empty.
            parts.push(format!(
                "source_hash LIKE '{}%'",
                h.replace('\'', "''")
            ));
        }
        if let Some(ref pdir) = self.parent_dir_prefix {
            parts.push(format!(
                "parent_dir LIKE '{}%'",
                pdir.replace('\'', "''")
            ));
        }
        if let Some(d_min) = self.audio_duration_min_seconds {
            parts.push(format!("audio_duration_seconds >= {}", d_min));
        }
        if let Some(d_max) = self.audio_duration_max_seconds {
            parts.push(format!("audio_duration_seconds <= {}", d_max));
        }
        if let Some(ref make) = self.image_camera_make {
            parts.push(format!(
                "image_camera_make LIKE '%{}%'",
                make.replace('\'', "''")
            ));
        }
        if let Some(ref model) = self.image_camera_model {
            parts.push(format!(
                "image_camera_model LIKE '%{}%'",
                model.replace('\'', "''")
            ));
        }
        if let Some(ref dom) = self.url_domain {
            // Substring match against url.  Pre-v106 rows have NULL
            // url and won't match (intended — the filter narrows to
            // rows with provenance).
            parts.push(format!(
                "url LIKE '%{}%'",
                dom.replace('\'', "''")
            ));
        }
        if let Some(ref t) = self.tag {
            // Element-of match on the Lance List<Utf8> tags column
            // via DataFusion's `array_has`.  Pre-v107 rows have NULL
            // tags and don't match.
            parts.push(format!(
                "array_has(tags, '{}')",
                t.replace('\'', "''")
            ));
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
    fn filters_sql_prefer_translated_lang_emits_correct_predicate() {
        // P13.5 follow-up — the new field translates into a scalar
        // filter that restricts rows to those whose target-language
        // column matches AND whose translated-text column is
        // populated.  Drift here would either bring in untranslated
        // rows (text_translated NULL) or wrong-language rows.
        let f = SearchFilters {
            prefer_translated_lang: Some("en".to_owned()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().expect("filter must produce SQL");
        assert!(
            sql.contains("text_translated_lang = 'en'"),
            "must include lang predicate: {sql}",
        );
        assert!(
            sql.contains("text_translated IS NOT NULL"),
            "must include non-null guard so rows without a translation drop out: {sql}",
        );
    }

    #[test]
    fn filters_sql_escapes_single_quotes_in_prefer_translated_lang() {
        // Defensive — the lang code is an ISO 639-1 in practice but
        // the same escaping path the other filters use should apply
        // (Bobby-Tables style protection in case a misbehaving
        // caller passes a quoted string).
        let f = SearchFilters {
            prefer_translated_lang: Some("e'n".to_owned()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("text_translated_lang = 'e''n'"), "got: {sql}");
    }

    #[test]
    fn filters_sql_combines_translated_lang_with_source_lang() {
        // The Bosnian-PDF example: `language = 'bs' AND
        // text_translated_lang = 'en' AND text_translated IS NOT NULL`
        // says "give me Bosnian sources that have been pre-translated
        // to English".  Both columns coexist on the same row.
        let f = SearchFilters {
            language: Some("bs".to_owned()),
            prefer_translated_lang: Some("en".to_owned()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("language = 'bs'"), "got: {sql}");
        assert!(sql.contains("text_translated_lang = 'en'"), "got: {sql}");
        assert!(sql.contains("text_translated IS NOT NULL"), "got: {sql}");
    }

    // ── P13.7 Step 6 — CLI search-filter SQL coverage ──────────────────

    #[test]
    fn filters_sql_ext_multi_select_emits_in_predicate() {
        let f = SearchFilters {
            ext: vec!["pdf".to_string(), "docx".to_string(), "mp3".to_string()],
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("ext IN ('pdf', 'docx', 'mp3')"), "sql = {sql}");
    }

    #[test]
    fn filters_sql_ext_lowercases_input() {
        // Drift guard: if a caller passes uppercase, we still emit
        // lowercase to match the stored column (lowercase-on-insert).
        let f = SearchFilters {
            ext: vec!["PDF".to_string()],
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("ext IN ('pdf')"), "sql = {sql}");
    }

    #[test]
    fn filters_sql_source_hash_prefix_emits_like() {
        let f = SearchFilters {
            source_hash_prefix: Some("a1b2c3".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("source_hash LIKE 'a1b2c3%'"), "sql = {sql}");
    }

    #[test]
    fn filters_sql_parent_dir_prefix_emits_like() {
        let f = SearchFilters {
            parent_dir_prefix: Some("/Users/foo/docs".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("parent_dir LIKE '/Users/foo/docs%'"),
            "sql = {sql}"
        );
    }

    #[test]
    fn filters_sql_audio_duration_range_emits_bounds() {
        let f = SearchFilters {
            audio_duration_min_seconds: Some(60.0),
            audio_duration_max_seconds: Some(1800.0),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("audio_duration_seconds >= 60"), "sql = {sql}");
        assert!(sql.contains("audio_duration_seconds <= 1800"), "sql = {sql}");
    }

    #[test]
    fn filters_sql_image_camera_filters_emit_like() {
        let f = SearchFilters {
            image_camera_make: Some("Apple".to_string()),
            image_camera_model: Some("iPhone 15 Pro".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("image_camera_make LIKE '%Apple%'"),
            "sql = {sql}"
        );
        assert!(
            sql.contains("image_camera_model LIKE '%iPhone 15 Pro%'"),
            "sql = {sql}"
        );
    }

    #[test]
    fn filters_sql_escapes_single_quotes_in_camera_filters() {
        // SQL-injection guard: a caller passing `' OR 1=1 --` must
        // get the literal quote escaped, not interpreted.  Pins the
        // doubled-quote convention LanceDB / DataFusion follow.
        let f = SearchFilters {
            image_camera_model: Some("O'Brien camera".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("image_camera_model LIKE '%O''Brien camera%'"),
            "sql = {sql}"
        );
    }

    // ── v106 — url_domain filter ──────────────────────────────────

    #[test]
    fn filters_sql_url_domain_emits_substring_like() {
        let f = SearchFilters {
            url_domain: Some("spiegel.de".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("url LIKE '%spiegel.de%'"),
            "sql = {sql}"
        );
    }

    #[test]
    fn filters_sql_url_domain_escapes_single_quotes() {
        let f = SearchFilters {
            url_domain: Some("o'brien.example".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("url LIKE '%o''brien.example%'"),
            "sql = {sql}"
        );
    }

    #[test]
    fn filters_sql_url_domain_omitted_when_none() {
        let f = SearchFilters::default();
        assert!(f.to_lance_sql().is_none());
    }

    #[test]
    fn filters_sql_tag_emits_array_has() {
        let f = SearchFilters {
            tag: Some("pocket-import".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("array_has(tags, 'pocket-import')"),
            "sql = {sql}"
        );
    }

    #[test]
    fn filters_sql_tag_escapes_single_quotes() {
        let f = SearchFilters {
            tag: Some("o'malley's blog".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(
            sql.contains("array_has(tags, 'o''malley''s blog')"),
            "sql = {sql}"
        );
    }

    #[test]
    fn filters_sql_tag_combines_with_url_domain() {
        let f = SearchFilters {
            tag: Some("research".to_string()),
            url_domain: Some("arxiv.org".to_string()),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("url LIKE '%arxiv.org%'"), "sql = {sql}");
        assert!(sql.contains("array_has(tags, 'research')"), "sql = {sql}");
    }

    #[test]
    fn filters_sql_url_domain_combines_with_other_filters() {
        let f = SearchFilters {
            url_domain: Some("github.com".to_string()),
            ext: vec!["md".to_string()],
            year_min: Some(2024),
            ..Default::default()
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("url LIKE '%github.com%'"), "sql = {sql}");
        assert!(sql.contains("ext IN ('md')"), "sql = {sql}");
        assert!(sql.contains("year >= 2024"), "sql = {sql}");
        // Single AND-joined predicate string
        assert!(sql.matches(" AND ").count() >= 2, "sql = {sql}");
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
