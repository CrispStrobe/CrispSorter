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
    let functions = ["is_sorted_ascending", "filter_to_sql", "passage_key", "document_keys_sql", "sparse_dot", "fts_input_from_pieces", "batches_to_search_results_with_scores", "replacement_filter", "chunks_to_record_batch", "str_col_opt", "is_table_not_found", "table_embedding_dims", "migrate_add_parent_dir_column", "migrate_add_volume_id_column", "sql_values", "import_replacement_ids", "search_result_columns", "record_batches_to_search_results", "parse_volume_id_from_metadata", "str_col", "str_val", "str_col_val_opt", "list_str_col_val", "i32_col", "i32_col_opt", "f32_col_opt", "ts_ms_col_opt"];
    let methods = ["fts_inputs_for_keys", "fts_document_keys", "fetch_passages", "matching_document_keys", "fetch_best_chunk_per_keys", "rerank_with_colbert", "search_sparse_in_pool", "export_cidx", "open_cidx", "search_vector", "search_vector_column", "fetch_best_chunk_per_doc", "open_or_create", "table_ref", "ingest_batch", "ingest_journal_path", "replacement_doc_ids", "replace_documents", "upsert_metadata_rows", "generation_committed", "document_metadata", "fetch_full_text", "delete_doc", "clear_extraction_failure", "indexed_state_for_uri", "indexed_state_for_owner_uri", "indexed_state_for_scope", "parse_indexed_state", "append_legacy_rows_for_test", "read_guard", "write_guard", "ingest_batch_with_fts", "delete_document_with_fts", "delete_document_if_version_with_fts", "update_location", "update_location_by_uri", "documents_absent", "prepare_imported_chunks", "replace_imported_rows", "fts_inputs_after_import", "row_generation_committed", "update_l2_fields", "prepare_metadata_update", "apply_metadata_columns", "fts_inputs_for_docs", "fts_inputs_for_doc_changes", "fetch_document_summary", "fetch_document_summary_for_uri"];
    let mut generated = String::from(r#"
use std::{path::Path, sync::Arc};
use anyhow::{anyhow, Context, Result};
use arrow_array::{builder::{ListBuilder, StringBuilder}, Array, Float32Array, FixedSizeListArray, Int16Array, Int32Array, LargeBinaryArray, ListArray, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow_schema::Schema;
use futures::TryStreamExt;
use lancedb::{connect, query::{ExecutableQuery, QueryBase}, table::NewColumnTransform, Connection, DistanceType, Table};
use super::schema::{DocumentChunk, SearchFilters, SearchResult, build_schema};
use super::embedder::SparseVector;
use super::ingest::chunk_row_id;
use super::search::{maxsim, unpack_multivec};
const TABLE_NAME: &str = "documents";
"#);
    let mut selected = Vec::new();
    for item in parsed.items {
        match item {
            syn::Item::Struct(s) if ["LocalIndex", "IndexedState", "FtsPiece", "Passage"].contains(&s.ident.to_string().as_str()) => generated.push_str(&s.into_token_stream().to_string()),
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
    println!("cargo:rerun-if-changed={}", root.join("occurrence.rs").display());
    println!("cargo:rerun-if-changed={}", root.join("source_move.rs").display());
    println!("cargo:rerun-if-changed={}", root.join("container_index.rs").display());

    // Compile all production retrieval methods with only model/network calls
    // adapted below. This qualifies owner-scoped joins and fusion end to end.
    for name in ["search", "result_cache", "migrations"] {
        let file = root.join(format!("{name}.rs"));
        println!("cargo:rerun-if-changed={}", file.display());
        let mut parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
        parsed.attrs.clear();
        if name == "search" {
            parsed.items.retain(|item| !matches!(item, syn::Item::Mod(module) if module.ident == "tests"));
        }
        fs::write(out.join(format!("{name}.rs")), parsed.into_token_stream().to_string()).unwrap();
    }
    let file = root.join("embedder.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut sparse = String::from("use serde::{Serialize, Deserialize};");
    for item in parsed.items {
        match item {
            syn::Item::Struct(value) if value.ident == "SparseVector" => sparse.push_str(&value.into_token_stream().to_string()),
            syn::Item::Impl(value) if value.self_ty.to_token_stream().to_string() == "SparseVector" => sparse.push_str(&value.into_token_stream().to_string()),
            _ => {}
        }
    }
    fs::write(out.join("sparse_vector.rs"), sparse).unwrap();

    let file = root.join("fields.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated = String::from("use serde::{Serialize,Deserialize}; use sha2::{Digest,Sha256};");
    for item in parsed.items {
        match item {
            syn::Item::Struct(s) if s.ident == "FieldEvidence" => generated.push_str(&s.into_token_stream().to_string()),
            syn::Item::Fn(f) if ["from_extraction", "read", "preserve_reviews", "tag", "attach"].contains(&f.sig.ident.to_string().as_str()) => generated.push_str(&f.into_token_stream().to_string()),
            _ => {}
        }
    }
    fs::write(out.join("fields.rs"), generated).unwrap();

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

    // Qualify federated ranking with the actual wire type, production function
    // and its pure tests, excluding desktop state and remote HTTP adapters.
    let file = root.join("../sync/cloud_backup.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated = String::from("use serde::{Serialize, Deserialize};");
    let mut found = false;
    for item in parsed.items {
        if let syn::Item::Struct(value) = item {
            if value.ident == "FederatedHit" {
                found = true;
                generated.push_str(&value.into_token_stream().to_string());
            }
        }
    }
    assert!(found, "production FederatedHit type missing");
    fs::write(out.join("federated_hit.rs"), generated).unwrap();

    let file = root.join("../sync/tauri_commands.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated = String::from("use super::cloud_backup::FederatedHit;");
    let mut selected = Vec::new();
    for item in parsed.items {
        match item {
            syn::Item::Const(value) if value.ident == "RRF_K" => generated.push_str(&value.into_token_stream().to_string()),
            syn::Item::Fn(value) if ["rrf_score", "rrf_merge"].contains(&value.sig.ident.to_string().as_str()) => {
                selected.push(value.sig.ident.to_string());
                generated.push_str(&value.into_token_stream().to_string());
            }
            syn::Item::Mod(mut module) if module.ident == "tests" => {
                if let Some((_, items)) = &mut module.content {
                    items.retain(|item| match item {
                        syn::Item::Use(_) => true,
                        syn::Item::Fn(value) => {
                            let name = value.sig.ident.to_string();
                            name.starts_with("rrf_merge_") || ["make_hit", "make_hit_with_sha"].contains(&name.as_str())
                        }
                        _ => false,
                    });
                }
                generated.push_str(&module.into_token_stream().to_string());
            }
            _ => {}
        }
    }
    for expected in ["rrf_score", "rrf_merge"] {
        assert!(selected.iter().any(|name| name == expected), "production function missing: {expected}");
    }
    fs::write(out.join("federated_rank.rs"), generated).unwrap();

    // Compile the CLI's real local-hit adapter and owner-identity tests too.
    let file = root.join("../cli/mod.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated = String::new();
    let mut tests = String::from("#[cfg(test)] mod tests { use super::*;");
    let mut found = false;
    for item in parsed.items {
        match item {
            syn::Item::Fn(value) if value.sig.ident == "local_result_to_federated" => {
                found = true;
                generated.push_str(&value.into_token_stream().to_string());
            }
            syn::Item::Mod(module) if module.ident == "tests" => {
                if let Some((_, items)) = module.content {
                    for item in items {
                        if let syn::Item::Fn(value) = item {
                            if ["mk_local_result", "local_result_maps_to_federated_with_url_tags_and_highlight", "federated_local_identity_retains_owner_when_source_hash_is_absent"]
                                .contains(&value.sig.ident.to_string().as_str())
                            {
                                tests.push_str(&value.into_token_stream().to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    assert!(found, "production local_result_to_federated function missing");
    tests.push('}');
    generated.push_str(&tests);
    fs::write(out.join("cli_federated.rs"), generated).unwrap();


    let file = root.join("../extractors/mod.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let config_types = ["OcrPipelineConfig", "OcrStageSpec", "OcrCleanupSpec", "ExtractedDocument",
        "ExtractOptions", "OcrTier", "OcrRecLang", "AnydocMode"];
    let mut generated = String::new();
    for item in parsed.items {
        match item {
            syn::Item::Struct(s) if config_types.contains(&s.ident.to_string().as_str()) => generated.push_str(&s.into_token_stream().to_string()),
            syn::Item::Enum(e) if config_types.contains(&e.ident.to_string().as_str()) => generated.push_str(&e.into_token_stream().to_string()),
            syn::Item::Impl(imp) if config_types.contains(&imp.self_ty.to_token_stream().to_string().as_str()) => generated.push_str(&imp.into_token_stream().to_string()),
            syn::Item::Fn(f) if f.sig.ident.to_string().starts_with("default_") => generated.push_str(&f.into_token_stream().to_string()),
            _ => {}
        }
    }
    fs::write(out.join("extractor_options.rs"), generated).unwrap();

    for (source,type_name,output,preamble) in [
        ("../extractors/container.rs","ExtractedMember","extracted_member.rs","use super::ExtractedDocument; use crisp_index_protocol::ContainerEntry;"),
        ("../audio/probe.rs","AudioMetadata","audio_metadata.rs",""),
        ("../images/exif.rs","ExifSummary","image_metadata.rs","use serde::{Serialize,Deserialize};"),
    ] {
        let file=root.join(source);println!("cargo:rerun-if-changed={}",file.display());
        let parsed=syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
        let mut generated=String::from(preamble);let mut found=false;
        for item in parsed.items {if let syn::Item::Struct(value)=item {if value.ident==type_name {
            found=true;generated.push_str(&value.into_token_stream().to_string());
        }}}
        assert!(found,"Missing native extractor DTO {type_name}");fs::write(out.join(output),generated).unwrap();
    }
    let file=root.join("tauri_commands.rs");println!("cargo:rerun-if-changed={}",file.display());
    let parsed=syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated=String::from("use super::ingest::RawDocument;");
    for item in parsed.items {match item {
        syn::Item::Struct(value) if value.ident=="DocumentIngestInput"=>generated.push_str(&value.into_token_stream().to_string()),
        syn::Item::Fn(mut value) if value.sig.ident=="raw_from_input"=>{
            value.vis=syn::parse_quote!(pub);generated.push_str(&value.into_token_stream().to_string());
        }
        _=>{}
    }}
    fs::write(out.join("foreground_container.rs"),generated).unwrap();

    let file = root.join("../bg_ingest/mod.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&fs::read_to_string(&file).unwrap()).unwrap();
    let mut generated = String::from("use serde::{Serialize,Deserialize}; use std::{collections::{BTreeMap,HashMap,HashSet,VecDeque},path::PathBuf,sync::{Arc,atomic::{AtomicUsize,Ordering}}};");
    let state_types = ["ForegroundGuard", "PendingIngest", "BgStatus", "BgStatusSnapshot", "BackgroundIngest", "BackgroundOptions", "WorkerLease"];
    for item in parsed.items {
        match item {
            syn::Item::Fn(f) if ["skip_unchanged", "hash_source", "source_key", "load_ocr_options"].contains(&f.sig.ident.to_string().as_str()) => generated.push_str(&f.into_token_stream().to_string()),
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
