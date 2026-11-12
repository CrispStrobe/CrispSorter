//! Compile the real CloudDrive/LocalDrive boundary without unrelated provider,
//! model or Tauri dependencies. No storage implementation is replaced by a stub.
use quote::ToTokens;
use std::{fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src");
    let source = root.join("drives/filesystem.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let parsed = syn::parse_file(&fs::read_to_string(&source).unwrap()).unwrap();
    let names = ["DriveCapabilities", "CloudDrive", "DriveType", "DirEntry", "FileVersion", "FileStat", "WalkEntry", "WalkScan", "LocalDrive", "NonReplayableWrite"];
    let functions = ["read_file_bounded", "content_search_matches", "walk", "walk_limited", "walk_with_scan_budget"];
    let mut generated = String::from("use anyhow::{anyhow, Context, Result}; use serde::{Deserialize, Serialize}; use std::{io::{Read,Write}, path::{Path,PathBuf}};");
    let mut selected = Vec::new();
    for item in parsed.items {
        match item {
            syn::Item::Struct(value) if names.contains(&value.ident.to_string().as_str()) => {
                selected.push(value.ident.to_string()); generated.push_str(&value.into_token_stream().to_string());
            }
            syn::Item::Enum(value) if names.contains(&value.ident.to_string().as_str()) => {
                selected.push(value.ident.to_string()); generated.push_str(&value.into_token_stream().to_string());
            }
            syn::Item::Trait(value) if value.ident == "CloudDrive" => {
                selected.push(value.ident.to_string()); generated.push_str(&value.into_token_stream().to_string());
            }
            syn::Item::Impl(value) if names.contains(&value.self_ty.to_token_stream().to_string().as_str()) => generated.push_str(&value.into_token_stream().to_string()),
            syn::Item::Fn(value) if functions.contains(&value.sig.ident.to_string().as_str()) => {
                selected.push(value.sig.ident.to_string()); generated.push_str(&value.into_token_stream().to_string());
            }
            syn::Item::Const(value) if value.ident.to_string().starts_with("MAX_") => generated.push_str(&value.into_token_stream().to_string()),
            _ => {}
        }
    }
    for name in names.into_iter().chain(functions) {
        assert!(selected.iter().any(|selected| selected == name), "production storage item missing: {name}");
    }
    for module in ["materialize", "mounted_read", "read_cache", "trash_receipts"] {
        let path = root.join(format!("drives/{module}.rs")).canonicalize().unwrap();
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!("#[path={:?}] pub(crate) mod {module};", path));
    }
    fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("drives.rs"), generated).unwrap();
    let output = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    fs::write(output.join("pair_conflicts.rs"), production_items(&root.join("sync/pairs.rs"),
        "use anyhow::Result; use serde::{Deserialize,Serialize}; use std::{collections::BTreeMap,path::Path,time::UNIX_EPOCH};",
        &["SyncPairMode", "SyncPair", "SyncPlanEntry", "SyncRemoteEntry", "SyncComparisonAction", "SyncComparisonEntry", "compare_plans", "local_push_plan",
          "plan_local", "plan_local_since", "visit_local", "filter_matches", "glob_matches", "glob_segments", "segment_matches"])).unwrap();
    fs::write(output.join("mutation_conflicts.rs"), production_items(&root.join("drives/tauri_commands.rs"),
        "use crate::sync::conflict::ConflictPolicy; use std::path::{Path,PathBuf};",
        &["MutationResolution", "mutation_destination", "mutation_conflict_tests"])).unwrap();
}

/// Compile the real pure conflict decisions and their mutation regressions,
/// without importing Tauri, keychain, provider clients or the full index stack.
fn production_items(source: &std::path::Path, prelude: &str, names: &[&str]) -> String {
    println!("cargo:rerun-if-changed={}", source.display());
    let parsed = syn::parse_file(&fs::read_to_string(source).unwrap()).unwrap();
    let mut generated = prelude.to_owned();
    let mut selected = Vec::new();
    for item in parsed.items {
        let name = match &item {
            syn::Item::Struct(value) => Some(value.ident.to_string()),
            syn::Item::Enum(value) => Some(value.ident.to_string()),
            syn::Item::Fn(value) => Some(value.sig.ident.to_string()),
            syn::Item::Mod(value) => Some(value.ident.to_string()),
            _ => None,
        };
        if let Some(name) = name.filter(|name| names.contains(&name.as_str())) {
            selected.push(name);
            generated.push_str(&item.into_token_stream().to_string());
        }
    }
    for name in names {
        assert!(selected.iter().any(|selected| selected.as_str() == *name), "production conflict item missing: {name}");
    }
    generated
}
