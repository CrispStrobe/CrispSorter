//! Model-free transaction qualification with the application's real LanceDB,
//! Arrow, SQLite and full-text implementation. Model calls are unavailable.
#![allow(dead_code, unused_imports)]

#[path = "../../src-tauri/src/index/snippet.rs"] pub mod snippet_production;
#[path = "../../src-tauri/src/index/summary.rs"] pub mod summary_production;
#[path = "../../src-tauri/src/index/barcode.rs"] pub mod barcode_production;
#[path = "../../src-tauri/src/index/task_failure.rs"] pub mod task_failure_production;
#[path = "../../shared/source_identity.rs"] pub mod source_identity_production;
#[path = "../../src-tauri/src/sync/conflict.rs"] pub mod sync_conflict_production;
pub mod sync {
    pub use crate::sync_conflict_production as conflict;
    pub mod cloud_backup { include!(concat!(env!("OUT_DIR"), "/federated_hit.rs")); }
    pub mod tauri_commands { include!(concat!(env!("OUT_DIR"), "/federated_rank.rs")); }
}
pub mod cli_federated_contract { include!(concat!(env!("OUT_DIR"), "/cli_federated.rs")); }

#[path = "../../src-tauri/src/index/fts_query.rs"] pub mod fts_query_production;
#[path = "../../src-tauri/src/index/search_summary.rs"] pub mod search_summary_production;
#[path = "../../src-tauri/src/index/report_output.rs"] pub mod report_output_production;

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
    pub use crate::search_summary_production as search_summary;
    pub use crate::report_output_production as report_output;
    pub mod local_index {
        include!(concat!(env!("OUT_DIR"), "/local_index.rs"));
        mod occurrence { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/src/index/occurrence.rs")); }
        pub(crate) use occurrence::{OccurrencePlan, SourceOccurrence};
        mod source_move { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/src/index/source_move.rs")); }
    }
    pub mod ingest { include!(concat!(env!("OUT_DIR"), "/ingest.rs")); }
    pub use fts_index::FtsIndex;
    pub use local_index::LocalIndex;
    pub use schema::SearchResult;
    pub use snippet::{highlight_snippet, SNIPPET_WINDOW};
    pub use crate::summary_production as summary;
    pub use crate::barcode_production as barcode;
    pub use crate::task_failure_production as task_failure;
    pub use crate::source_identity_production as source_identity;
    pub use crate::snippet_production as snippet;
    pub mod result_cache { include!(concat!(env!("OUT_DIR"), "/result_cache.rs")); }
    pub mod search { include!(concat!(env!("OUT_DIR"), "/search.rs")); }
    pub mod reranker {
        pub struct RerankerHandle;
        impl RerankerHandle {
            pub async fn score_batch(&self, _: &str, _: &[&str]) -> Vec<f32> { panic!("Reranker models are unavailable") }
        }
    }
    pub mod omni_embed {
        pub fn is_omni_available() -> bool { false }
        pub fn encode_text_omni(_: &str) -> anyhow::Result<Vec<f32>> { anyhow::bail!("Models are unavailable") }
        pub fn encode_image_omni(_: &std::path::Path) -> anyhow::Result<Vec<f32>> { anyhow::bail!("Models are unavailable") }
    }

    pub mod embedder {
        use anyhow::{Result, bail};
        pub struct Embedder;
        pub struct TextChunk { pub text: String, pub start_char: usize, pub end_char: usize, pub chunk_index: i32 }
        pub struct Dense { pub vectors: Vec<Vec<f32>> }
        include!(concat!(env!("OUT_DIR"), "/sparse_vector.rs"));
        pub type Sparse = SparseVector;
        pub enum EmbedRole { Passage, Query }
        impl Embedder {
            pub fn chunk_for_embedding(&self, _: &str) -> Result<Vec<TextChunk>> { bail!("Models are unavailable in the transaction harness") }
            pub fn has_colbert(&self) -> bool { false }
            pub fn embed_full(&mut self, _: Vec<String>, _: EmbedRole) -> Result<(Dense, Vec<Option<Sparse>>)> { bail!("Models are unavailable") }
            pub fn embed_multivec(&mut self, _: Vec<String>) -> Result<Vec<Vec<Vec<f32>>>> { bail!("Models are unavailable") }
            pub fn model(&self) -> &str { "unavailable" }
            pub fn has_sparse(&self) -> bool { false }
            pub fn embed_sparse(&mut self, _: Vec<String>) -> Result<Vec<Option<SparseVector>>> { bail!("Models are unavailable") }
            pub fn rerank_biencoder(&mut self, _: &str, _: &[String]) -> Result<Vec<f32>> { bail!("Models are unavailable") }
            // A fixed query vector makes retrieval joins deterministic; model
            // inference and relevance quality remain outside this contract.
            pub fn embed_dense(&mut self, texts: Vec<String>, _: EmbedRole) -> Result<Dense> {
                Ok(Dense { vectors: texts.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect() })
            }
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
    pub mod fields { include!(concat!(env!("OUT_DIR"), "/fields.rs")); }
}

#[cfg(test)]
mod retrieval_contract;

#[path = "../../src-tauri/src/index/passage.rs"]
pub mod passage;

#[cfg(test)]
mod filter_contract;

#[cfg(test)]
mod search_summary_contract;

pub mod images { pub mod vit_embed {
    pub fn embed_image(_: &std::path::Path) -> anyhow::Result<Vec<f32>> { anyhow::bail!("Image models are unavailable") }
} }
