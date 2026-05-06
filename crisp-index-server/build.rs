use std::env;

fn main() {
    // lance-encoding (pulled in transitively via lancedb) compiles `.proto`
    // files at build time via prost-build, which spawns `protoc`. If the
    // user doesn't have it on PATH the whole build dies with
    //   Could not find `protoc`. ... Try installing protobuf-compiler ...
    // Fall back to a bundled binary via `protoc-bin-vendored`. Honour any
    // pre-existing `PROTOC` so users with a system protoc still win.
    //
    // Mirrors `src-tauri/build.rs` so the two workspace members behave the
    // same way on machines without a system protoc installation.
    if env::var_os("PROTOC").is_none() {
        if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
            env::set_var("PROTOC", path);
        }
    }
}
