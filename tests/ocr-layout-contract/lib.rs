//! Compile and exercise actual workspace recognition with real image, config,
//! region, word-evidence and layout types. Native model inference and preparation
//! are explicit fixtures; this is not a native OCR accuracy qualification.
#![allow(dead_code, unexpected_cfgs)]
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[path = "../../src-tauri/src/ocr_reading_order.rs"]
mod ocr_reading_order;
#[path = "../../src-tauri/src/ocr_word_evidence.rs"]
mod ocr_word_evidence;
#[path = "../../src-tauri/src/extractors/image_input.rs"]
pub mod production_image_input;
#[path = "../../src-tauri/src/extractors/layout.rs"]
pub mod production_layout;
mod extractors {
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
    pub use crate::production_image_input as image_input;
    pub use crate::production_layout as layout;
    pub mod ocr_crispembed {
        include!(concat!(env!("OUT_DIR"), "/review.rs"));
        pub fn cc_detect_regions(_: &std::path::Path) -> Vec<super::layout::LayoutRegion> {
            crate::native_fixture::layout()
        }
        pub fn ocr_regions_for_review(
            path: &std::path::Path,
            config: &super::OcrPipelineConfig,
        ) -> anyhow::Result<OcrReviewResult> {
            crate::native_fixture::recognize(path, config)
        }
    }
}
include!(concat!(env!("OUT_DIR"), "/workspace.rs"));
// Preparation and unsupported engines do not participate in these tests.
fn prepare_page(
    _: &Path,
    path: &Path,
    _: &extractors::OcrPipelineConfig,
    size: Option<(f64, f64)>,
) -> Result<PreparedPage, String> {
    let (width, height) = checked_dimensions(path)?;
    let mut page = PageAsset {
        image_path: path.to_string_lossy().into(),
        width,
        height,
        page_width_points: None,
        page_height_points: None,
        warnings: vec![],
    };
    set_physical_size(&mut page, size);
    Ok(PreparedPage {
        page,
        warnings: vec![],
    })
}
fn persist_image(_: &Path, _: &DynamicImage) -> Result<PageAsset, String> {
    Err("zone fixture unavailable".into())
}
fn recognize_with_cpu_budget(
    _: &Path,
    _: &extractors::OcrPipelineConfig,
) -> Result<extractors::ocr_crispembed::OcrReviewResult, String> {
    Err("CPU worker fixture unavailable".into())
}
fn recognize_plain_engine_for_review(
    _: &Path,
    _: &extractors::OcrPipelineConfig,
    _: extractors::OcrRecLang,
) -> Result<extractors::ocr_crispembed::OcrReviewResult, String> {
    Err("plain engine fixture unavailable".into())
}

mod native_fixture {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    pub static LISTINGS: AtomicUsize = AtomicUsize::new(0);
    pub static INFERENCES: AtomicUsize = AtomicUsize::new(0);
    pub static EMPTY_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
    pub static FAIL_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
    pub fn reset(empty_at: usize) {
        LISTINGS.store(0, Ordering::SeqCst);
        INFERENCES.store(0, Ordering::SeqCst);
        EMPTY_AT.store(empty_at, Ordering::SeqCst);
        FAIL_AT.store(usize::MAX, Ordering::SeqCst);
    }
    pub fn layout() -> Vec<extractors::layout::LayoutRegion> {
        LISTINGS.fetch_add(1, Ordering::SeqCst);
        [
            (300., 74., 860., 54.),
            (1197., 74., 297., 54.),
            (81., 260., 377., 38.),
            (1043., 260., 368., 38.),
            (85., 409., 514., 48.),
            (1043., 409., 472., 48.),
            (80., 559., 554., 39.),
            (1045., 559., 491., 48.),
            (80., 709., 424., 39.),
            (1045., 709., 532., 39.),
        ]
        .into_iter()
        .map(|(x, y, w, h)| extractors::layout::LayoutRegion {
            x1: x,
            y1: y,
            x2: x + w,
            y2: y + h,
            score: 1.,
            kind: extractors::layout::RegionKind::Text,
            label_name: "text".into(),
        })
        .collect()
    }
    pub fn region() -> extractors::ocr_crispembed::OcrReviewRegion {
        use ocr_word_evidence::*;
        extractors::ocr_crispembed::OcrReviewRegion {
            text: "word".into(),
            x: 2.,
            y: 3.,
            w: 30.,
            h: 12.,
            confidence: Some(0.9),
            confidence_basis: "recognition".into(),
            char_conf: vec![],
            char_conf_unit: "unknown".into(),
            char_conf_aligned: false,
            kind: "text".into(),
            word_evidence: Some(WordEvidenceEnvelope {
                version: 1,
                source: "tesseract_tsv".into(),
                offset_unit: "utf16".into(),
                words: vec![NativeWordEvidence {
                    text: "word".into(),
                    start: 0,
                    end: 4,
                    x: 2.,
                    y: 3.,
                    w: 30.,
                    h: 12.,
                    confidence: Some(0.9),
                    confidence_basis: "recognition".into(),
                    alternatives: None,
                }],
            }),
        }
    }
    pub fn recognize(
        path: &Path,
        _: &extractors::OcrPipelineConfig,
    ) -> anyhow::Result<extractors::ocr_crispembed::OcrReviewResult> {
        let index = INFERENCES.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(
            index != FAIL_AT.load(Ordering::SeqCst),
            "native fixture inference failed"
        );
        let (width, height) = image::image_dimensions(path)?;
        Ok(extractors::ocr_crispembed::OcrReviewResult {
            image_path: path.into(),
            width,
            height,
            regions: if index == EMPTY_AT.load(Ordering::SeqCst) {
                vec![]
            } else {
                vec![region()]
            },
            warnings: vec![],
            diagnostics: json!({"native_review":{"engine":"tesseract","recognition_text":"word","raw_marker":index,
                "input_width":width,"input_height":height}}),
        })
    }
}

#[cfg(test)]
mod layout_contract_tests {
    use super::*;
    use std::sync::{atomic::Ordering, Mutex};
    static SERIAL: Mutex<()> = Mutex::new(());
    fn config() -> extractors::OcrPipelineConfig {
        serde_json::from_value(json!({"enabled":true,"engine":"tesseract","language":"eng","router":false,"layout":true,"layout_engine":"cc","experimental_cc_column_crops":true,"cleanup_enabled":false,"stages":[{"engine":"tesseract","rec_model":"tesseract-eng","cleanup":{"enabled":false}}]})).unwrap()
    }
    fn page() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("page.png");
        image::RgbImage::from_pixel(1800, 1000, image::Rgb([255, 255, 255]))
            .save(&p)
            .unwrap();
        (root, p)
    }
    #[test]
    fn layout_contract_ordinary_cc_profile_keeps_whole_page_recognition() {
        let _lock = SERIAL.lock().unwrap();
        for explicit_false in [false, true] {
            native_fixture::reset(usize::MAX);
            let (root, path) = page();
            let mut requested = serde_json::to_value(config()).unwrap();
            requested
                .as_object_mut()
                .unwrap()
                .remove("experimental_cc_column_crops");
            if explicit_false {
                requested["experimental_cc_column_crops"] = json!(false);
            }
            let cfg: extractors::OcrPipelineConfig = serde_json::from_value(requested).unwrap();
            assert!(!cfg.experimental_cc_column_crops);
            let result = recognize_page(
                root.path(),
                &path,
                &cfg,
                None,
                vec![],
                None,
                false,
                extractors::OcrRecLang::Auto,
            )
            .unwrap();
            assert_eq!(native_fixture::INFERENCES.load(Ordering::SeqCst), 1);
            assert_eq!(native_fixture::LISTINGS.load(Ordering::SeqCst), 1);
            assert_eq!(result.regions.len(), 1);
            assert_eq!((result.regions[0].x, result.regions[0].y), (2., 3.));
            assert_eq!(result.diagnostics["native_review"]["raw_marker"], 0);
            assert_eq!(result.diagnostics["native_review"]["input_width"], 1800);
            assert_eq!(result.diagnostics["native_review"]["input_height"], 1000);
            assert!(result.diagnostics.get("layout_recognition").is_none());
            assert_eq!(
                result.diagnostics["requested_config"]["experimental_cc_column_crops"],
                false
            );
        }
    }
    #[test]
    fn layout_contract_workspace_preserves_request_raw_diagnostics_and_detects_layout_once() {
        let _lock = SERIAL.lock().unwrap();
        native_fixture::reset(usize::MAX);
        let (root, path) = page();
        let cfg = config();
        let result = recognize_page(
            root.path(),
            &path,
            &cfg,
            None,
            vec![],
            None,
            false,
            extractors::OcrRecLang::Auto,
        )
        .unwrap();
        assert_eq!(
            result.diagnostics["requested_config"],
            serde_json::to_value(&cfg).unwrap()
        );
        assert_eq!(result.diagnostics["engine"], "tesseract");
        assert_eq!(
            result.diagnostics["layout_recognition"]["crops"]
                .as_array()
                .unwrap()
                .len(),
            9
        );
        assert_eq!(
            result.diagnostics["layout_recognition"]["crops"][0]["raw_regions"][0]["x"],
            2.
        );
        assert_eq!(
            result.diagnostics["layout_recognition"]["crops"][0]["diagnostics"]["native_review"]
                ["recognition_text"],
            "word"
        );
        assert_eq!(native_fixture::LISTINGS.load(Ordering::SeqCst), 1);
        assert_eq!(native_fixture::INFERENCES.load(Ordering::SeqCst), 9);
        assert_eq!(result.regions.len(), 9);
        assert!(result.regions[0].x > 200.);
        for region in result.regions {
            ocr_word_evidence::validate_word_evidence(
                region.word_evidence.as_ref().unwrap(),
                &region.text,
                1800,
                1000,
            )
            .unwrap();
        }
    }
    #[test]
    fn layout_contract_empty_crop_falls_back_to_whole_page_with_warning_and_raw_evidence() {
        let _lock = SERIAL.lock().unwrap();
        native_fixture::reset(1);
        let (root, path) = page();
        let cfg = config();
        let result = recognize_page(
            root.path(),
            &path,
            &cfg,
            None,
            vec![],
            None,
            false,
            extractors::OcrRecLang::Auto,
        )
        .unwrap();
        assert_eq!(result.regions.len(), 1);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("whole-page recognition")));
        assert_eq!(
            result.diagnostics["layout_recognition"]["strategy"],
            "whole_page_after_empty_cc_crop"
        );
        assert_eq!(
            result.diagnostics["layout_recognition"]["crops"][1]["raw_regions"],
            json!([])
        );
        assert_eq!(
            result.diagnostics["requested_config"],
            serde_json::to_value(&cfg).unwrap()
        );
        assert_eq!(native_fixture::LISTINGS.load(Ordering::SeqCst), 1);
        assert_eq!(native_fixture::INFERENCES.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn layout_contract_rejects_word_outside_crop_even_when_it_would_fit_page() {
        let mut r = native_fixture::region();
        r.word_evidence.as_mut().unwrap().words[0].x = 90.;
        let crop = ocr_reading_order::Rect {
            x: 1000.,
            y: 200.,
            w: 100.,
            h: 50.,
        };
        assert!(translate_crop_review_regions(&mut [r], crop, 1800, 1000).is_err());
        let mut valid = vec![native_fixture::region()];
        translate_crop_review_regions(&mut valid, crop, 1800, 1000).unwrap();
        assert_eq!(valid[0].word_evidence.as_ref().unwrap().words[0].x, 1002.);
        assert_eq!(valid[0].x, 1002.);
    }
    #[test]
    fn layout_contract_native_error_is_not_hidden_by_empty_crop_fallback() {
        let _lock = SERIAL.lock().unwrap();
        native_fixture::reset(usize::MAX);
        native_fixture::FAIL_AT.store(1, Ordering::SeqCst);
        let (root, path) = page();
        let error = recognize_page(
            root.path(),
            &path,
            &config(),
            None,
            vec![],
            None,
            false,
            extractors::OcrRecLang::Auto,
        )
        .err()
        .unwrap();
        assert!(error.contains("native fixture inference failed"));
        assert_eq!(native_fixture::INFERENCES.load(Ordering::SeqCst), 2);
        assert_eq!(native_fixture::LISTINGS.load(Ordering::SeqCst), 1);
    }
}
