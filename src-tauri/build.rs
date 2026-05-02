use std::env;
use std::path::Path;

fn main() {
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
    let lib_dir = build_dir.join("src");
    let ggml_dir = build_dir.join("ggml").join("src");
    match target_os {
        "macos" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Frameworks");
        }
        "linux" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        }
        _ => {}
    }
}
