//! Extract production items unchanged; only native execution is a fixture.
use quote::ToTokens;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn selected(source: &str, names: &[&str], defaults: bool) -> String {
    let mut found = Vec::new();
    let mut out = String::new();
    for item in syn::parse_file(source).unwrap().items {
        let name = match &item {
            syn::Item::Struct(v) => Some(v.ident.to_string()),
            syn::Item::Enum(v) => Some(v.ident.to_string()),
            syn::Item::Fn(v) => Some(v.sig.ident.to_string()),
            _ => None,
        };
        let wanted = name
            .as_ref()
            .is_some_and(|s| names.contains(&s.as_str()) || defaults && s.starts_with("default_"));
        let implementation = matches!(&item,syn::Item::Impl(v) if names.contains(&v.self_ty.to_token_stream().to_string().as_str()));
        if wanted || implementation {
            if let Some(n) = name {
                found.push(n);
            }
            out.push_str(&item.to_token_stream().to_string());
        }
    }
    for name in names {
        assert!(
            found.iter().any(|n| n == name),
            "missing source item {name}"
        );
    }
    out
}
fn extract(root: &Path, out: &Path, file: &str, path: &str, names: &[&str], defaults: bool) {
    let input = root.join(path);
    println!("cargo:rerun-if-changed={}", input.display());
    fs::write(
        out.join(file),
        selected(&fs::read_to_string(input).unwrap(), names, defaults),
    )
    .unwrap();
}
fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for (file, path, names, defaults) in [
        (
            "config.rs",
            "src-tauri/src/extractors/mod.rs",
            vec![
                "OcrPipelineConfig",
                "OcrStageSpec",
                "OcrCleanupSpec",
                "OcrRecLang",
                "ExtractedDocument",
            ],
            true,
        ),
        (
            "container.rs",
            "src-tauri/src/extractors/container.rs",
            vec!["ExtractedMember"],
            false,
        ),
        (
            "audio.rs",
            "src-tauri/src/audio/probe.rs",
            vec!["AudioMetadata"],
            false,
        ),
        (
            "exif.rs",
            "src-tauri/src/images/exif.rs",
            vec!["ExifSummary"],
            false,
        ),
        (
            "words.rs",
            "src-tauri/src/ocr_word_evidence.rs",
            vec![
                "NativeWordAlternative",
                "NativeWordEvidence",
                "WordEvidenceEnvelope",
            ],
            false,
        ),
        (
            "render.rs",
            "src-tauri/src/extractors/ocr_render.rs",
            vec!["OcrRegion"],
            false,
        ),
        (
            "pipeline.rs",
            "src-tauri/src/extractors/ocr_crispembed.rs",
            vec![
                "run_prepared_pipeline",
                "ocr_via_pipeline",
                "ocr_regions_via_pipeline",
                "apply_reading_order",
                "RenderRegionInput",
                "render_regions_in_reading_order",
                "render_regions_from_pipeline",
                "RegionConf",
                "ocr_regions_detailed",
                "effective_confidence",
                "OcrReviewRegion",
                "OcrReviewResult",
                "review_pipeline_config",
                "parse_review_snapshot",
                "ocr_regions_for_review",
            ],
            false,
        ),
    ] {
        extract(&root, &out, file, path, &names, defaults);
    }
    // Source-bound real pinned wrapper types and method signatures, without FFI linking.
    println!("cargo:rerun-if-env-changed=CRISPEMBED_SOURCE");
    let vendor = std::env::var_os("CRISPEMBED_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../CrispEmbed"));
    let lock = root.join("scripts/crispembed-runtime-lock.json");
    println!("cargo:rerun-if-changed={}", lock.display());
    let lock_text = fs::read_to_string(lock).unwrap();
    let commit = lock_text
        .split('"')
        .collect::<Vec<_>>()
        .windows(3)
        .find(|v| v[0] == "source_commit")
        .unwrap()[2];
    let reference = format!("{commit}:crispembed/src/lib.rs");
    let output = Command::new("git")
        .arg("-C")
        .arg(vendor)
        .args(["show", &reference])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "pinned CrispEmbed source must be available"
    );
    let vendor_source = String::from_utf8(output.stdout).unwrap();
    let mut native = selected(
        &vendor_source,
        &["OcrPipelineRegion", "OcrPipelineResult"],
        false,
    );
    native.push_str("pub struct CrispOcrPipeline; impl CrispOcrPipeline {");
    let mut methods = Vec::new();
    for item in syn::parse_file(&vendor_source).unwrap().items {
        if let syn::Item::Impl(v) = item {
            if v.self_ty.to_token_stream().to_string() != "CrispOcrPipeline" {
                continue;
            }
            for item in v.items {
                if let syn::ImplItem::Fn(mut f) = item {
                    let body = match f.sig.ident.to_string().as_str() {
                        "run" => "{ crate::native_fixture::run(image_path) }",
                        "review_json" => "{ crate::native_fixture::snapshot() }",
                        "detected_lang" => "{ crate::native_fixture::language() }",
                        _ => continue,
                    };
                    methods.push(f.sig.ident.to_string());
                    f.block = syn::parse_str(body).unwrap();
                    native.push_str(&f.to_token_stream().to_string());
                }
            }
        }
    }
    assert_eq!(methods.len(), 3);
    native.push('}');
    fs::write(out.join("native.rs"), native).unwrap();
}
