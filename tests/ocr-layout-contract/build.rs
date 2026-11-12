//! Compile production workspace recognition and real boundary types without
//! Tauri/model linking. Only native inference and image preparation are fixtures.
use quote::ToTokens;
use std::{fs, path::PathBuf};
use syn::visit::Visit;

// The lightweight workspace harness does not link the CLI. Still check its
// exhaustive config literals against the real schema so added fields cannot
// pass this contract and then fail full application compilation with E0063.
fn check_cli_config_fields(source: &std::path::Path) {
    let schema =
        syn::parse_file(&fs::read_to_string(source.join("extractors/mod.rs")).unwrap()).unwrap();
    let fields: Vec<_> = schema
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == "OcrPipelineConfig" => Some(
                item.fields
                    .iter()
                    .map(|field| field.ident.as_ref().unwrap().to_string())
                    .collect(),
            ),
            _ => None,
        })
        .expect("production OcrPipelineConfig schema");
    struct ConfigLiterals<'a> {
        fields: &'a [String],
        checked: usize,
    }
    impl<'ast> Visit<'ast> for ConfigLiterals<'_> {
        fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
            if expr
                .path
                .segments
                .last()
                .is_some_and(|part| part.ident == "OcrPipelineConfig")
                && expr.rest.is_none()
            {
                let present: Vec<_> = expr
                    .fields
                    .iter()
                    .map(|field| field.member.to_token_stream().to_string())
                    .collect();
                let missing: Vec<_> = self
                    .fields
                    .iter()
                    .filter(|field| !present.contains(field))
                    .collect();
                assert!(
                    missing.is_empty(),
                    "exhaustive CLI OcrPipelineConfig literal misses fields: {missing:?}"
                );
                self.checked += 1;
            }
            syn::visit::visit_expr_struct(self, expr);
        }
    }
    let path = source.join("cli/mod.rs");
    println!("cargo:rerun-if-changed={}", path.display());
    let cli = syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap();
    let mut literals = ConfigLiterals {
        fields: &fields,
        checked: 0,
    };
    literals.visit_file(&cli);
    assert!(
        literals.checked > 0,
        "exhaustive CLI config coverage guard matched no constructors"
    );
}

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
    check_cli_config_fields(&source);
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
