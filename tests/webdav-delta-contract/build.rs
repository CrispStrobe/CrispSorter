use std::{env, fs, path::PathBuf};
fn main() {
    let source = "../../src-tauri/src/drives/webdav.rs";
    println!("cargo:rerun-if-changed={source}");
    let text = fs::read_to_string(source).unwrap();
    let start = text.find("    pub fn delta_upload_file(").unwrap();
    let end = text[start..].find("\n}\n\n/// Minimal RFC").unwrap() + start;
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wiring.rs"),
        format!("impl WebDavDrive {{\n{}\n}}", &text[start..end]),
    )
    .unwrap();
    let types_start = text
        .find("#[derive(Debug, Clone, serde::Deserialize)]\nstruct DeltaStatus")
        .unwrap();
    let types_end = text[types_start..]
        .find("#[derive(Debug, Clone, serde::Serialize)]")
        .unwrap()
        + types_start;
    let methods_start = text.find("    pub fn delta_sync_available(").unwrap();
    let methods_end = text.find("    fn blockmap_from_server(").unwrap();
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("metadata_wiring.rs"),
        format!(
            "{}\nimpl WebDavDrive {{\n{}\n}}",
            &text[types_start..types_end],
            &text[methods_start..methods_end]
        ),
    )
    .unwrap();
}
