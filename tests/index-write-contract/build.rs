//! Compile the production transaction implementation directly from its AST.
//! Only unrelated search, model loading and Tauri adapters are excluded.
use quote::ToTokens;
use std::{fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src/index");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let file = root.join("local_index.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let functions = ["replacement_filter", "chunks_to_record_batch", "str_col_opt", "is_table_not_found", "table_embedding_dims", "migrate_add_parent_dir_column", "migrate_add_volume_id_column", "sql_values", "import_replacement_ids", "search_result_columns", "record_batches_to_search_results", "parse_volume_id_from_metadata", "str_col", "str_val", "str_col_val_opt", "list_str_col_val", "i32_col", "i32_col_opt", "f32_col_opt", "ts_ms_col_opt"];
    let methods = ["open_or_create", "table_ref", "ingest_batch", "ingest_journal_path", "replacement_doc_ids", "replace_documents", "upsert_metadata_rows", "generation_committed", "document_metadata", "fetch_full_text", "delete_doc", "clear_extraction_failure", "indexed_state_for_uri", "parse_indexed_state", "append_legacy_rows_for_test", "read_guard", "write_guard", "ingest_batch_with_fts", "delete_document_with_fts", "documents_absent", "prepare_imported_chunks", "replace_imported_rows", "fts_inputs_after_import", "row_generation_committed", "update_l2_fields", "prepare_metadata_update", "apply_metadata_columns", "fts_inputs_for_docs", "fts_inputs_for_doc_changes", "fetch_document_summary", "fetch_document_summary_for_uri"];
    let mut generated = String::from(r#"
use std::{path::Path, sync::Arc};
use anyhow::{anyhow, Context, Result};
use arrow_array::{builder::{ListBuilder, StringBuilder}, Array, Float32Array, FixedSizeListArray, Int16Array, Int32Array, LargeBinaryArray, ListArray, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow_schema::Schema;
use futures::TryStreamExt;
use lancedb::{connect, query::{ExecutableQuery, QueryBase}, table::NewColumnTransform, Connection, Table};
use super::schema::{DocumentChunk, SearchResult, build_schema};
const TABLE_NAME: &str = "documents";
"#);
    let mut selected = Vec::new();
    for item in parsed.items {
        match item {
            syn::Item::Struct(s) if ["LocalIndex", "IndexedState", "FtsPiece"].contains(&s.ident.to_string().as_str()) => generated.push_str(&s.into_token_stream().to_string()),
            syn::Item::Fn(f) if functions.contains(&f.sig.ident.to_string().as_str()) => generated.push_str(&f.into_token_stream().to_string()),
            syn::Item::Impl(mut imp) if imp.trait_.is_none() && imp.self_ty.to_token_stream().to_string() == "LocalIndex" => {
                imp.items.retain(|item| matches!(item, syn::ImplItem::Fn(f) if methods.contains(&f.sig.ident.to_string().as_str())));
                for item in &imp.items { if let syn::ImplItem::Fn(f) = item { selected.push(f.sig.ident.to_string()); } }
                generated.push_str(&imp.into_token_stream().to_string());
            }
            syn::Item::Impl(imp) if imp.self_ty.to_token_stream().to_string() == "FtsPiece" => generated.push_str(&imp.into_token_stream().to_string()),
            _ => {}
        }
    }
    for expected in methods { assert!(selected.iter().any(|s| s == expected), "production method missing: {expected}"); }
    fs::write(out.join("local_index.rs"), generated).unwrap();

    let file = root.join("ingest.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let mut parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    for item in &mut parsed.items {
        if let syn::Item::Mod(module) = item {
            if module.ident == "tests" {
                if let Some((_, items)) = &mut module.content {
                    // This preexisting test invokes the full retrieval engine;
                    // all transaction tests below it run without alteration.
                    items.retain(|i| !matches!(i, syn::Item::Fn(f) if f.sig.ident == "text_only_ingest_is_searchable_without_an_embedder"));
                }
            }
        }
    }
    fs::write(out.join("ingest.rs"), parsed.into_token_stream().to_string()).unwrap();
    let file = root.join("../jobs/mod.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let mut parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    parsed.attrs.clear();
    parsed.items.retain(|item| !matches!(item, syn::Item::Mod(module) if module.ident == "tauri_commands"));
    fs::write(out.join("jobs.rs"), parsed.into_token_stream().to_string()).unwrap();


    let file = root.join("../extractors/mod.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let config_types = ["OcrPipelineConfig", "OcrStageSpec", "OcrCleanupSpec"];
    let mut generated = String::new();
    for item in parsed.items {
        match item {
            syn::Item::Struct(s) if config_types.contains(&s.ident.to_string().as_str()) => generated.push_str(&s.into_token_stream().to_string()),
            syn::Item::Impl(imp) if config_types.contains(&imp.self_ty.to_token_stream().to_string().as_str()) => generated.push_str(&imp.into_token_stream().to_string()),
            syn::Item::Fn(f) if f.sig.ident.to_string().starts_with("default_") => generated.push_str(&f.into_token_stream().to_string()),
            _ => {}
        }
    }
    fs::write(out.join("extractor_options.rs"), generated).unwrap();

    let file = root.join("../bg_ingest/mod.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated = String::from("use serde::{Serialize,Deserialize}; use std::{collections::{BTreeMap,HashMap,HashSet,VecDeque},path::PathBuf,sync::{Arc,atomic::{AtomicUsize,Ordering}}};");
    let state_types = ["ForegroundGuard", "PendingIngest", "BgStatus", "BgStatusSnapshot", "BackgroundIngest", "BackgroundOptions", "WorkerLease"];
    for item in parsed.items {
        match item {
            syn::Item::Fn(f) if ["skip_unchanged", "hash_source", "source_key"].contains(&f.sig.ident.to_string().as_str()) => generated.push_str(&f.into_token_stream().to_string()),
            syn::Item::Struct(s) if state_types.contains(&s.ident.to_string().as_str()) => generated.push_str(&s.into_token_stream().to_string()),
            syn::Item::Enum(e) if state_types.contains(&e.ident.to_string().as_str()) => generated.push_str(&e.into_token_stream().to_string()),
            syn::Item::Impl(imp) if state_types.contains(&imp.self_ty.to_token_stream().to_string().as_str()) => generated.push_str(&imp.into_token_stream().to_string()),
            syn::Item::Const(c) if c.ident == "EXTRACTION_TIMEOUT_SECS" => generated.push_str(&c.into_token_stream().to_string()),
            syn::Item::Mod(module) if module.ident == "tests" => generated.push_str(&module.into_token_stream().to_string()),
            _ => {}
        }
    }
    fs::write(out.join("bg_ingest.rs"), generated).unwrap();
}
