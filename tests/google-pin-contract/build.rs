use quote::ToTokens;
fn main() {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/src/drives/google_drive.rs");
    println!("cargo:rerun-if-changed={}", file.display());
    let parsed = syn::parse_file(&std::fs::read_to_string(file).unwrap()).unwrap();
    let mut output = String::from("use anyhow::Result; use crate::sync::proxy::ProxyConfig; #[cfg(not(feature = \"experimental-google-root-pins\"))] use crate::sync::proxy::build_blocking_client;");
    let mut constants = 0;
    let mut constructors = 0;
    let mut structs = 0;
    for item in parsed.items {
        match item {
            syn::Item::Const(c)
                if ["API_BASE", "UPLOAD_BASE"].contains(&c.ident.to_string().as_str()) =>
            {
                constants += 1;
                output += &c.into_token_stream().to_string();
            }
            syn::Item::Struct(s) if s.ident == "GoogleDriveDrive" => {
                structs += 1;
                output += "#[allow(dead_code)]";
                output += &s.into_token_stream().to_string();
            }
            syn::Item::Impl(mut i) => {
                i.items.retain(|item| matches!(item, syn::ImplItem::Fn(f) if ["new_with_proxy", "with_api_base_and_client"].contains(&f.sig.ident.to_string().as_str())));
                constructors += i.items.len();
                if !i.items.is_empty() {
                    output += &i.into_token_stream().to_string();
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        (constants, constructors, structs),
        (2, 2, 1),
        "production constructor extraction drift"
    );
    std::fs::write(
        std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("google_constructor.rs"),
        output,
    )
    .unwrap();
}
