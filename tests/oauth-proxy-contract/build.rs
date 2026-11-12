use quote::ToTokens;
use std::{fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src/drives");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let oauth = root.join("oauth.rs").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", oauth.display());
    fs::write(
        out.join("oauth_module.rs"),
        format!("#[path={:?}] pub mod oauth;", oauth),
    )
    .unwrap();
    let secret = root.join("secret.rs");
    println!("cargo:rerun-if-changed={}", secret.display());
    let mut credentials = None;
    for item in syn::parse_file(&fs::read_to_string(secret).unwrap())
        .unwrap()
        .items
    {
        if let syn::Item::Struct(value) = item {
            if value.ident == "DriveCredentials" {
                credentials = Some(value.into_token_stream().to_string());
            }
        }
    }
    fs::write(
        out.join("credentials.rs"),
        credentials.expect("production credentials type"),
    )
    .unwrap();
    let commands = root.join("tauri_commands.rs");
    println!("cargo:rerun-if-changed={}", commands.display());
    let wanted = [
        "configured_proxy",
        "drive_oauth_start",
        "drive_oauth_refresh",
        "drive_oauth_revoke",
    ];
    let mut found = Vec::new();
    let mut source = String::new();
    for item in syn::parse_file(&fs::read_to_string(commands).unwrap())
        .unwrap()
        .items
    {
        if let syn::Item::Fn(mut function) = item {
            let name = function.sig.ident.to_string();
            if wanted.contains(&name.as_str()) {
                // Tauri's command registration is outside this transport check.
                function
                    .attrs
                    .retain(|a| a.path().segments.first().unwrap().ident != "tauri");
                source.push_str(&function.into_token_stream().to_string());
                found.push(name);
            }
        }
    }
    assert_eq!(found.len(), wanted.len());
    fs::write(out.join("commands.rs"), source).unwrap();
}
