//! Compile the real CloudDrive/LocalDrive boundary without unrelated provider,
//! model or Tauri dependencies. No storage implementation is replaced by a stub.
use quote::ToTokens;
use std::{fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src");
    let source = root.join("drives/mod.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let parsed = syn::parse_file(&fs::read_to_string(&source).unwrap()).unwrap();
    let names = ["DriveCapabilities", "CloudDrive", "DriveType", "DirEntry", "FileVersion", "FileStat", "WalkEntry", "LocalDrive"];
    let functions = ["read_file_bounded", "content_search_matches", "walk", "walk_limited"];
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
    for module in ["materialize", "mounted_read", "read_cache"] {
        let path = root.join(format!("drives/{module}.rs")).canonicalize().unwrap();
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!("#[path={:?}] pub(crate) mod {module};", path));
    }
    fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("drives.rs"), generated).unwrap();
}
