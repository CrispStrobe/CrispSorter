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
}
