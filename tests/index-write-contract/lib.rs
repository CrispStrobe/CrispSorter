//! Model-free transaction qualification with the application's real LanceDB,
//! Arrow, SQLite and full-text implementation. Model calls are unavailable.
#![allow(dead_code, unused_imports)]

#[path = "../../src-tauri/src/index/snippet.rs"] pub mod snippet_production;
#[path = "../../src-tauri/src/index/summary.rs"] pub mod summary_production;
#[path = "../../src-tauri/src/index/barcode.rs"] pub mod barcode_production;
#[path = "../../src-tauri/src/index/task_failure.rs"] pub mod task_failure_production;

#[path = "../../src-tauri/src/index/fts_query.rs"] pub mod fts_query_production;

pub use fts_query_production as fts_query;
#[path = "../../src-tauri/src/index/synonyms.rs"] pub mod synonyms_production;

pub mod jobs { include!(concat!(env!("OUT_DIR"), "/jobs.rs")); }
#[path = "../../src-tauri/src/bg_ingest/queue.rs"] pub mod bg_queue_production;
pub mod extractors { include!(concat!(env!("OUT_DIR"), "/extractor_options.rs")); }
pub mod bg_ingest { pub use crate::bg_queue_production as queue; include!(concat!(env!("OUT_DIR"), "/bg_ingest.rs")); }

pub mod index {
    pub mod schema { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/src/index/schema.rs")); }
    pub use crate::fts_query_production as fts_query;
    pub use crate::synonyms_production as synonyms;
    pub mod fts_index { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/src/index/fts_index.rs")); }
    pub mod local_index { include!(concat!(env!("OUT_DIR"), "/local_index.rs")); }
    pub mod ingest { include!(concat!(env!("OUT_DIR"), "/ingest.rs")); }
    pub use fts_index::FtsIndex;
    pub use local_index::LocalIndex;
    pub use crate::summary_production as summary;
    pub use crate::barcode_production as barcode;
    pub use crate::task_failure_production as task_failure;
    pub use crate::snippet_production as snippet;
    pub mod result_cache { pub fn invalidate() {} }

    pub mod embedder {
        use anyhow::{Result, bail};
        pub struct Embedder;
        pub struct TextChunk { pub text: String, pub start_char: usize, pub end_char: usize, pub chunk_index: i32 }
        pub struct Dense { pub vectors: Vec<Vec<f32>> }
        pub struct Sparse;
        impl Sparse { pub fn to_json(&self) -> serde_json::Value { serde_json::Value::Null } }
        pub enum EmbedRole { Passage }
        impl Embedder {
            pub fn chunk_for_embedding(&self, _: &str) -> Result<Vec<TextChunk>> { bail!("Models are unavailable in the transaction harness") }
            pub fn has_colbert(&self) -> bool { false }
            pub fn embed_full(&mut self, _: Vec<String>, _: EmbedRole) -> Result<(Dense, Vec<Option<Sparse>>)> { bail!("Models are unavailable") }
            pub fn embed_multivec(&mut self, _: Vec<String>) -> Result<Vec<Vec<Vec<f32>>>> { bail!("Models are unavailable") }
            pub fn model(&self) -> &str { "unavailable" }
        }
    }
    pub mod ner {
        pub struct NerHandle;
        pub struct ScoredEntity { pub label: String, pub text: String, pub score: f32 }
        pub struct Model;
        impl Model { pub fn display_name(&self) -> &str { "unavailable" } }
        impl NerHandle {
            pub async fn extract_fields(&self, _: &str) -> Vec<(String, String, f32)> { panic!("NER is unavailable") }
            pub fn entity_limit(&self) -> usize { 0 }
            pub fn model(&self) -> Model { Model }
        }
        pub fn build_entity_tags(_: Vec<ScoredEntity>, _: f32, _: usize) -> Vec<String> { panic!("NER is unavailable") }
    }
    pub mod fields {
        pub struct Field { pub review_status: String }
        pub fn from_extraction(_: &str, _: &str, _: &str, _: Vec<(String, String, f32)>) -> Vec<Field> { panic!("NER is unavailable") }
        pub fn read(_: &serde_json::Value) -> Vec<Field> { panic!("NER is unavailable") }
        pub fn preserve_reviews(_: &mut Vec<Field>, _: &Vec<Field>) { panic!("NER is unavailable") }
        pub fn attach(_: &mut Option<String>, _: &Vec<Field>) { panic!("NER is unavailable") }
        pub fn tag(_: &Field) -> String { panic!("NER is unavailable") }
    }
}
