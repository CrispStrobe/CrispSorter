use std::{env, fs, path::PathBuf};
fn main() {
    let source = "../../src-tauri/src/drives/webdav.rs";
    println!("cargo:rerun-if-changed={source}");
    let text = fs::read_to_string(source).unwrap();
    // Keep the app's representative server fixtures and parser regressions in
    // this small target, alongside the focused regressions.
    let mut server_fixtures = String::new();
    for name in [
        "parse_propfind_handles_nextcloud_response",
        "parse_propfind_filters_foreign_namespace_properties",
        "parse_propfind_handles_unprefixed_dav_namespace",
        "parse_propfind_preserves_spaces_around_escaped_ampersand",
        "parse_propfind_rejects_truncation_after_complete_response",
        "parse_propfind_requires_a_multistatus_document_root",
        "parse_propfind_honors_response_status",
        "parse_propfind_keeps_response_and_propstat_status_separate",
        "parse_propfind_rejects_invalid_response_status",
        "parse_propfind_honors_propstat_outcomes",
        "parse_propfind_rejects_unavailable_type_and_bad_status",
        "parse_propfind_preserves_statusless_legacy_properties",
        "parse_propfind_preserves_direct_type_and_ignores_vendor_propstat",
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
