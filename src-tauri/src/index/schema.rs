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
}
