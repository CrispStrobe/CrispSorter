use quote::ToTokens;
use std::{fs, path::PathBuf};
fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/src/drives/internxt_native_drive.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let parsed = syn::parse_file(&fs::read_to_string(source).unwrap()).unwrap();
    let mut generated = String::new();
    let mut count = 0;
    for item in &parsed.items {
        let keep = match &item {
            syn::Item::Fn(value) => ["receipt_account", "receipt_folder", "receipt_check"]
                .contains(&value.sig.ident.to_string().as_str()),
            syn::Item::Impl(value) => value.trait_.as_ref().is_some_and(|(_, path, _)| {
                path.segments
                    .last()
                    .is_some_and(|s| s.ident == "ReceiptTrash")
            }),
            _ => false,
        };
        if keep {
            generated.push_str(&item.into_token_stream().to_string());
            count += 1;
        }
    }
    assert_eq!(
        count, 4,
        "production receipt adapter changed: update the narrow fixture gate"
    );
    for item in &parsed.items {
        if let syn::Item::Impl(value) = item {
            if value.trait_.as_ref().is_some_and(|(_, path, _)| {
                path.segments
                    .last()
                    .is_some_and(|s| s.ident == "CloudDrive")
            }) {
                let mut methods = String::new();
                for item in &value.items {
                    if let syn::ImplItem::Fn(method) = item {
                        if ["capabilities", "receipt_trash"]
                            .contains(&method.sig.ident.to_string().as_str())
                        {
                            methods.push_str(&method.to_token_stream().to_string());
                        }
                    }
                }
                generated.push_str(&format!("impl NativeInternxtDrive {{ {methods} }}"));
            }
        }
    }
    let filesystem =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src/drives/filesystem.rs");
    println!("cargo:rerun-if-changed={}", filesystem.display());
    for item in syn::parse_file(&fs::read_to_string(filesystem).unwrap())
        .unwrap()
        .items
    {
        let keep = match &item {
            syn::Item::Struct(value) => value.ident == "DriveCapabilities",
            syn::Item::Impl(value) => {
                value.self_ty.to_token_stream().to_string() == "DriveCapabilities"
            }
            _ => false,
        };
        if keep {
            generated.push_str(&item.into_token_stream().to_string());
        }
    }
    fs::write(
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("internxt.rs"),
        generated,
    )
    .unwrap();
}
