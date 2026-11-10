//! Exercise the production full-text index without the application shell,
//! vector database, model downloads, or platform-specific Tauri dependencies.
//! Only the app-wide transport types are reduced to fields used by these two
//! production modules. Query parsing, indexing, positions, and scoring are real.
pub mod schema {
    use serde::Deserialize;
    #[derive(Clone, Default)]
    pub struct SearchFilters {
        pub owner_id: Option<String>,
        pub mounted_volume_ids: Option<Vec<String>>,
        pub language: Option<String>,
        pub year_min: Option<i32>,
        pub year_max: Option<i32>,
        pub tags: Vec<String>,
        pub prefer_translated_lang: Option<String>,
        pub ext: Vec<String>,
        pub source_hash_prefix: Option<String>,
        pub parent_dir_prefix: Option<String>,
        pub audio_duration_min_seconds: Option<f64>,
        pub audio_duration_max_seconds: Option<f64>,
        pub image_camera_make: Option<String>,
        pub image_camera_model: Option<String>,
        pub omni_search: bool,
        pub colbert_rerank: bool,
        pub url_domain: Option<String>,
        pub tag: Option<String>,
        pub fuzzy: bool,
        pub synonyms: bool,
        pub doc_id_scope: Vec<String>,
        pub doc_status: Option<String>,
    }
    #[derive(Clone)]
    pub struct SearchResult;
    #[derive(Clone, Deserialize)]
    pub struct DocumentChunk {
        pub doc_id: String,
        pub owner_id: String,
        pub language: Option<String>,
        pub title: Option<String>,
        pub headings_text: Option<String>,
        pub full_text: Option<String>,
        pub text_translated: Option<String>,
    }
}
#[path = "../../src-tauri/src/index/fts_index.rs"]
pub mod fts_index;
#[path = "../../src-tauri/src/index/fts_query.rs"]
pub mod fts_query;
#[path = "../../src-tauri/src/index/result_cache.rs"]
pub mod result_cache;
#[path = "../../src-tauri/src/index/synonyms.rs"]
pub mod synonyms;
pub mod index {
    pub use crate::{fts_index, fts_query, result_cache, schema, synonyms};
}
