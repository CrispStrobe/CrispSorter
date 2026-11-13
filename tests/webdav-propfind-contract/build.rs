use std::{env, fs, path::PathBuf};
fn main() {
    let source = "../../src-tauri/src/drives/webdav.rs";
    println!("cargo:rerun-if-changed={source}");
    let text = fs::read_to_string(source).unwrap();
    // Keep the app's representative server fixtures and entity regression in
    // this small target, alongside the focused regressions.
    let mut server_fixtures = String::new();
    for name in [
        "parse_propfind_handles_nextcloud_response",
        "parse_propfind_handles_unprefixed_dav_namespace",
        "parse_propfind_preserves_spaces_around_escaped_ampersand",
    ] {
        let start = text.find(&format!("    fn {name}()")).unwrap();
        let end = start + text[start..].find("\n    #[test]").unwrap();
        server_fixtures.push_str("#[test]\n");
        server_fixtures.push_str(&text[start..end]);
    }
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("server_fixtures.rs"),
        server_fixtures,
    )
    .unwrap();
    let start = text.find("/// One <D:response>").unwrap();
    let end = text.find("/// Parse RFC 7231 IMF-fixdate").unwrap();
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("webdav_propfind.rs"),
        &text[start..end],
    )
    .unwrap();
}
