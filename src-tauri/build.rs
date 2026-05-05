use std::env;
use std::path::Path;

fn main() {
    // lance-encoding (pulled in transitively via lancedb) compiles `.proto`
    // files at build time via `prost-build`, which spawns `protoc`. If the
    // user doesn't have it on PATH the whole build dies with
    //   Could not find `protoc`. ... Try installing protobuf-compiler ...
    // Fall back to a bundled binary via `protoc-bin-vendored`. Honour any
    // pre-existing `PROTOC` so users who have a system protoc (or want a
    // specific version) still win. On Windows `paths.ps1` also bootstraps
    // a real protoc release into `gh_temp/protoc/` and prepends it to
    // PATH, since lance-encoding additionally needs the bundled
    // `include/google/protobuf/*.proto` files which the vendored binary
    // doesn't ship with.
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
            std::env::set_var("PROTOC", path);
        }
    }

    // ── Native-lib runtime search (rpath) propagation ──────────────────────
    //
    // crispasr-sys (and crispembed-sys) use `links = "crispasr"` /
    // `links = "crispembed"` so their build scripts can publish
    // `cargo:LIB_DIR=…` metadata that we pick up here as
    // DEP_CRISPASR_LIB_DIR / DEP_CRISPEMBED_LIB_DIR.
    //
    // Why we do it here instead of in the -sys crates' build scripts:
    // `cargo:rustc-link-arg` only takes effect when emitted by the build
    // script of the package that owns the binary being linked. From a
    // transitive lib it's silently dropped. Emitting from this build.rs
    // gives the final tauri-app executable the right LC_RPATH (macOS) /
    // DT_RUNPATH (Linux) entries.
    //
    // The runtime entries we add:
    //
    //   * Absolute path to the cmake build dir's lib outputs — lets
    //     `cargo run` work directly out of the workspace, without copying
    //     anything around.
    //   * `@executable_path/../Frameworks` (macOS) /
    //     `$ORIGIN/../lib` (Linux) — lets the bundled .app / .deb find
    //     libcrispasr after the bundling step has copied it next to the
    //     executable. Windows needs no rpath; DLLs are looked up next to
    //     the .exe.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for var in ["DEP_CRISPASR_LIB_DIR", "DEP_CRISPEMBED_LIB_DIR"] {
        if let Ok(build_dir) = env::var(var) {
            emit_rpath_for(&target_os, Path::new(&build_dir));
        }
    }

    tauri_build::build()
}

fn emit_rpath_for(target_os: &str, build_dir: &Path) {
    // Cover both layouts:
    //   * CrispASR / build-flutter-bundle convention: libs in
    //     `<build>/src/` + `<build>/ggml/src/`.
    //   * CrispEmbed convention (flat): libs at `<build>/` itself
    //     (libcrispembed.dylib, libggml*.dylib all in the same dir).
    //
    // dyld silently skips rpath entries that don't exist on disk, so
    // emitting both is harmless and keeps consumer code identical
    // across the two layouts.
    let lib_dir = build_dir.join("src");
    let ggml_dir = build_dir.join("ggml").join("src");
    match target_os {
        "macos" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", build_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Frameworks");
        }
        "linux" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", build_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        }
        _ => {}
    }
}
