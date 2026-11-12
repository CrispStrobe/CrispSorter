//! Source-bound Rust contract: production execution/conversion code and actual
//! app/pinned-wrapper types. Native calls, model cache and logging are fixtures.
#![allow(dead_code, unexpected_cfgs, unused_imports, unused_variables)]
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
mod crispembed {
    include!(concat!(env!("OUT_DIR"), "/native.rs"));
}
mod audio {
    pub mod probe {
        include!(concat!(env!("OUT_DIR"), "/audio.rs"));
    }
}
mod images {
    pub mod exif {
        use serde::{Deserialize, Serialize};
        include!(concat!(env!("OUT_DIR"), "/exif.rs"));
    }
}
mod ocr_word_evidence {
    use serde::{Deserialize, Serialize};
    include!(concat!(env!("OUT_DIR"), "/words.rs"));
}
mod operation_log {
    pub struct OperationLog;
    impl OperationLog {
        pub fn start(_: &str, _: serde_json::Value) -> Self {
            Self
        }
        pub fn succeed(&self, _: serde_json::Value) {}
    }
}
fn app_log(_: &str, _: String) {}
#[path = "../../src-tauri/src/extractors/ovis_input.rs"]
pub mod production_ovis_input;
mod extractors {
    use serde::{Deserialize, Serialize};
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
    pub mod container {
        use super::ExtractedDocument;
        use crisp_index_protocol::container::ContainerEntry;
        include!(concat!(env!("OUT_DIR"), "/container.rs"));
    }
    pub use crate::production_ovis_input as ovis_input;
    pub mod ocr_render {
        include!(concat!(env!("OUT_DIR"), "/render.rs"));
    }
    pub mod ocr_crispembed {
        use super::ExtractedDocument;
        use crate::{crispembed, operation_log::OperationLog};
        use anyhow::{Context, Result};
        use serde_json::{json, Value};
        use std::path::Path;
        fn with_ocr_pipeline<T>(
            _: &super::OcrPipelineConfig,
            run: impl FnOnce(&mut crispembed::CrispOcrPipeline) -> Result<T>,
        ) -> Result<T> {
            run(&mut crispembed::CrispOcrPipeline)
        }
        include!(concat!(env!("OUT_DIR"), "/pipeline.rs"));
        pub fn execute(
            path: &Path,
            cfg: &super::OcrPipelineConfig,
        ) -> Result<(
            crispembed::OcrPipelineResult,
            Option<String>,
            Option<Value>,
            Option<super::ovis_input::InputScale>,
        )> {
            run_prepared_pipeline(path, cfg, false, false)
        }
    }
}
mod native_fixture {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub paths: Vec<PathBuf>,
        pub snapshot_calls: usize,
        pub language_calls: usize,
        pub snapshot: Option<Value>,
        pub fail: bool,
        pub invalid_box: bool,
        pub mode: Option<u32>,
    }
    thread_local! {pub static STATE:std::cell::RefCell<State>=std::cell::RefCell::new(State::default());}
    pub fn reset() {
        STATE.with(|s|*s.borrow_mut()=State {snapshot:Some(json!({"schema":"crispembed.ocr.review","schema_version":1,
            "geometry":{"coordinates_valid_for_input":true},"regions":[{"box":{"x":0,"y":0,"w":384,"h":640},"confidence_source":"recognizer","confidence":0.8}]})),..Default::default()});
    }
    pub fn run(path: &str) -> Result<crispembed::OcrPipelineResult, String> {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.paths.push(path.into());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                s.mode = Some(std::fs::metadata(path).unwrap().permissions().mode() & 0o777);
            }
            if s.fail {
                return Err("controlled native failure".into());
            }
            let (w, h) = image::image_dimensions(path).unwrap();
            Ok(crispembed::OcrPipelineResult {
                regions: vec![crispembed::OcrPipelineRegion {
                    text: "Grüße".into(),
                    x: 0.,
                    y: 0.,
                    w: if s.invalid_box { f32::NAN } else { w as f32 },
                    h: h as f32,
                    confidence: 0.8,
                    rec_confidence: 0.8,
                    char_conf: vec![0.8],
                }],
                full_text: "Grüße".into(),
                mean_confidence: 0.8,
                reading_order: vec![0],
                markdown: "Grüße".into(),
            })
        })
    }
    pub fn language() -> Option<String> {
        STATE.with(|s| s.borrow_mut().language_calls += 1);
        Some(String::from("de"))
    }
    pub fn snapshot() -> Option<String> {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.snapshot_calls += 1;
            s.snapshot.as_ref().map(ToString::to_string)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use extractors::ocr_crispembed as production;
    fn config() -> extractors::OcrPipelineConfig {
        serde_json::from_value(json!({"engine":"unified","router":false,"cleanup_enabled":false,
            "stages":[{"engine":"unified","source_type":"auto","det_model":"ovisocr2-f16","cleanup":{"enabled":false,"denoise":false}}]})).unwrap()
    }
    fn source() -> tempfile::NamedTempFile {
        let f = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        image::DynamicImage::new_rgb8(1280, 2077)
            .save(f.path())
            .unwrap();
        f
    }
    fn last_path() -> PathBuf {
        native_fixture::STATE.with(|s| s.borrow().paths.last().unwrap().clone())
    }
    fn assert_private_and_cleaned(original: &Path) {
        let path = last_path();
        assert_ne!(path, original);
        assert!(!path.exists());
        #[cfg(unix)]
        native_fixture::STATE.with(|s| assert_eq!(s.borrow().mode, Some(0o600)));
    }
    #[test]
    fn actual_boundary_feature_matrix() {
        std::env::remove_var("CRISPSORTER_OVIS_RESIZE_INPUT");
        native_fixture::reset();
        let file = source();
        let bytes = std::fs::read(file.path()).unwrap();
        let cfg = config();
        // Disabled behavior bypasses resize validation, keeps source path and
        // avoids newly introduced snapshot work in text/detailed/render paths.
        let mut legacy = cfg.clone();
        legacy.router = true;
        legacy.stages.clear();
        let doc = production::ocr_via_pipeline(file.path(), &legacy).unwrap();
        assert_eq!(doc.full_text, "Grüße");
        let detailed = production::ocr_regions_detailed(file.path(), &legacy).unwrap();
        assert_eq!(detailed[0].w, 1280.);
        let rendered = production::ocr_regions_via_pipeline(file.path(), &legacy).unwrap();
        assert_eq!(rendered[0].h, 2077.);
        native_fixture::STATE.with(|s| {
            assert_eq!(s.borrow().snapshot_calls, 0);
            assert_eq!(s.borrow().language_calls, 1);
            assert!(s.borrow().paths.iter().all(|p| p == file.path()));
        });
        let review = production::ocr_regions_for_review(file.path(), &cfg).unwrap();
        assert_eq!(review.width, 1280);
        native_fixture::STATE.with(|s| {
            assert_eq!(
                s.borrow().snapshot_calls,
                usize::from(cfg!(feature = "ocr-review-diagnostics"))
            )
        });
        std::env::set_var("CRISPSORTER_OVIS_RESIZE_INPUT", "1");
        native_fixture::reset();
        if !cfg!(feature = "ocr-review-diagnostics") {
            assert!(production::execute(file.path(), &cfg)
                .err()
                .unwrap()
                .to_string()
                .contains("diagnostics"));
            native_fixture::STATE.with(|s| assert!(s.borrow().paths.is_empty()));
        } else {
            let (r, _, snapshot, scale) = production::execute(file.path(), &cfg).unwrap();
            assert_eq!(scale.unwrap().inference, (384, 640));
            assert_eq!((r.regions[0].w, r.regions[0].h), (1280., 2077.));
            assert_eq!(snapshot.unwrap()["regions"][0]["box"]["w"], 384);
            assert_private_and_cleaned(file.path());
            let review = production::ocr_regions_for_review(file.path(), &cfg).unwrap();
            assert_eq!(
                (
                    review.width,
                    review.height,
                    review.regions[0].w,
                    review.regions[0].h
                ),
                (1280, 2077, 1280., 2077.)
            );
            assert_eq!(review.image_path, file.path());
            assert_eq!(review.diagnostics["inference_input"]["opt_in"], true);
            assert_eq!(
                review.diagnostics["inference_input"]["native_review_coordinate_space"],
                "inference_input_pixels"
            );
            assert_eq!(
                review.diagnostics["native_review"]["regions"][0]["box"]["w"],
                384
            );
            assert_private_and_cleaned(file.path());
            native_fixture::reset();
            let small = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
            image::DynamicImage::new_rgb8(512, 512)
                .save(small.path())
                .unwrap();
            let (unchanged, _, _, plan) = production::execute(small.path(), &cfg).unwrap();
            assert!(!plan.unwrap().changed());
            assert_eq!(unchanged.regions[0].w, 512.);
            assert_eq!(last_path(), small.path());
            assert!(small.path().exists());
            let detailed = production::ocr_regions_detailed(file.path(), &cfg).unwrap();
            assert_eq!(detailed[0].h, 2077.);
            let rendered = production::ocr_regions_via_pipeline(file.path(), &cfg).unwrap();
            assert_eq!(rendered[0].w, 1280.);
            let doc = production::ocr_via_pipeline(file.path(), &cfg).unwrap();
            assert_eq!(doc.full_text, "Grüße");
            assert_private_and_cleaned(file.path());
            for mode in 0..7 {
                native_fixture::reset();
                native_fixture::STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    match mode {
                        0 => s.snapshot = None,
                        1 => {
                            s.snapshot.as_mut().unwrap()["geometry"]
                                ["coordinates_valid_for_input"] = json!(false)
                        }
                        2 => {
                            s.snapshot.as_mut().unwrap()["regions"][0]["word_evidence"] =
                                json!({"words":[]})
                        }
                        3 => s.invalid_box = true,
                        4 => s.fail = true,
                        5 => s.snapshot.as_mut().unwrap()["schema"] = json!("unknown"),
                        _ => s.snapshot.as_mut().unwrap()["regions"] = json!({}),
                    }
                });
                assert!(
                    production::execute(file.path(), &cfg).is_err(),
                    "mode {mode}"
                );
                assert_private_and_cleaned(file.path());
            }
        }
        for mut invalid in [
            cfg.clone(),
            cfg.clone(),
            cfg.clone(),
            cfg.clone(),
            cfg.clone(),
            cfg.clone(),
        ]
        .into_iter()
        .enumerate()
        {
            match invalid.0 {
                0 => invalid.1.router = true,
                1 => invalid.1.stages.push(invalid.1.stages[0].clone()),
                2 => invalid.1.stages[0].det_model = Some("custom.gguf".into()),
                3 => invalid.1.stages[0].cleanup.enabled = true,
                4 => invalid.1.stages[0].cleanup.denoise = true,
                _ => invalid.1.stages[0].source_type = "photo".into(),
            };
            native_fixture::reset();
            assert!(production::execute(file.path(), &invalid.1).is_err());
            native_fixture::STATE.with(|s| assert!(s.borrow().paths.is_empty()));
        }
        // The option is scoped to unified/Ovis selection; other families keep
        // their original path and do not incur native snapshot work.
        let mut unrelated = cfg.clone();
        unrelated.engine = Some("tesseract".into());
        unrelated.stages[0].engine = "tesseract".into();
        native_fixture::reset();
        production::execute(file.path(), &unrelated).unwrap();
        assert_eq!(last_path(), file.path());
        native_fixture::STATE.with(|s| assert_eq!(s.borrow().snapshot_calls, 0));
        assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
        std::env::remove_var("CRISPSORTER_OVIS_RESIZE_INPUT");
    }
}
