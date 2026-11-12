//! Compile production workspace recognition and real boundary types without
//! Tauri/model linking. Only native inference and image preparation are fixtures.
use quote::ToTokens;
use std::{fs, path::PathBuf};
fn select(path: &std::path::Path, names: &[&str], defaults: bool) -> String {
    println!("cargo:rerun-if-changed={}", path.display());
    let parsed = syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap();
    let mut out = String::new();
    let mut seen = Vec::new();
    for item in parsed.items {
        let name = match &item {
            syn::Item::Struct(v) => Some(v.ident.to_string()),
            syn::Item::Enum(v) => Some(v.ident.to_string()),
            syn::Item::Fn(v) => Some(v.sig.ident.to_string()),
            syn::Item::Const(v) => Some(v.ident.to_string()),
            _ => None,
        };
        let wanted = name
            .as_ref()
            .is_some_and(|n| names.contains(&n.as_str()) || defaults && n.starts_with("default_"));
        let implementation = matches!(&item,syn::Item::Impl(v) if names.contains(&v.self_ty.to_token_stream().to_string().as_str()));
        if wanted || implementation {
            if let Some(n) = name {
                seen.push(n);
            }
            out.push_str(&item.into_token_stream().to_string());
        }
    }
    for n in names {
        assert!(seen.iter().any(|s| s == n), "missing production item {n}");
    }
    out
}
fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src");
    let output = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for (file, path, names, defaults) in [
        (
            "config.rs",
            "extractors/mod.rs",
            vec![
                "OcrPipelineConfig",
                "OcrStageSpec",
                "OcrCleanupSpec",
                "OcrRecLang",
            ],
            true,
        ),
        (
            "review.rs",
            "extractors/ocr_crispembed.rs",
            vec!["OcrReviewRegion", "OcrReviewResult"],
            false,
        ),
        (
            "workspace.rs",
            "ocr_workspace.rs",
            vec![
                "PageAsset",
                "PreparedPage",
                "PixelRect",
                "RecognizedPage",
                "MAX_PAGE_PIXELS",
                "MAX_REGIONS",
                "checked_dimensions",
                "decode_page",
                "checked_rect",
                "set_physical_size",
                "recognize_page",
                "recognize_cc_columns",
                "translate_crop_review_regions",
                "map_system_tesseract_regions",
                "organize_review_regions",
                "has_aligned_character_scores",
                "merge_word_regions_into_lines",
                "group_word_run",
                "join_line",
                "group_lines_into_paragraphs",
                "join_paragraph",
                "annotate_layout",
                "apply_review_layout",
            ],
            false,
        ),
    ] {
        fs::write(
            output.join(file),
            select(&source.join(path), &names, defaults),
        )
        .unwrap();
    }
}
