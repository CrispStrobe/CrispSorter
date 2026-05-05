fn main() {
    // lance-encoding (pulled in transitively via lancedb) compiles `.proto`
    // files at build time via `prost-build`, which spawns `protoc`. If the
    // user doesn't have it on PATH the whole build dies with
    //   Could not find `protoc`. ... Try installing protobuf-compiler ...
    // Avoid the externally-installed dependency by pointing `PROTOC` at a
    // bundled binary via the `protoc-bin-vendored` build-script crate.
    // Honour any pre-existing `PROTOC` so users who have a system protoc
    // (or want a specific version) still win.
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
            std::env::set_var("PROTOC", path);
        }
    }
    tauri_build::build()
}
