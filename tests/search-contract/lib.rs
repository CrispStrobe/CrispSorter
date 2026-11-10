//! Exercise the production full-text index without the application shell,
//! vector database, model downloads, or platform-specific Tauri dependencies.
//! Only the app-wide transport types are reduced to fields used by these two
//! production modules. Query parsing, indexing, positions, and scoring are real.
pub mod schema {
        use serde::Deserialize;
        #[derive(Default)]
        pub struct SearchFilters { pub owner_id: Option<String> }
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
#[path = "../../src-tauri/src/index/fts_query.rs"]
pub mod fts_query;
#[path = "../../src-tauri/src/index/fts_index.rs"]
pub mod fts_index;
pub mod index { pub use crate::{fts_index, fts_query, schema}; }
