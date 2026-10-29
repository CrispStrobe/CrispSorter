//! Model-free process-protocol check using the verbatim production worker launcher.
use crispsorter_ocr_qualification::extractors::{ocr_worker, OcrPipelineConfig};

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|v| v == "--crispsorter-ocr-worker") {
        assert_eq!(std::env::var("CRISPEMBED_MAX_PIXELS").unwrap(), "262144");
        assert_eq!(std::env::var("QWEN2VL_OCR_FORCE_CPU").unwrap(), "1");
        if std::env::var_os("CRISPSORTER_PROBE_REAL").is_some() {
            ocr_worker::execute(std::path::Path::new(&args[2]), std::path::Path::new(&args[3])).expect("real OCR worker");
            return;
        }
        let request: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
        assert_eq!(request["path"], "fixture.png");
        assert!(request["config"].is_object());
        std::fs::write(&args[3], br#"[{"text":"worker response","x":1.0,"y":2.0,"w":3.0,"h":4.0,"confidence":0.5,"char_conf":[]}]"#).unwrap();
        return;
    }
    let before_pixels = std::env::var_os("CRISPEMBED_MAX_PIXELS");
    let before_cpu = std::env::var_os("QWEN2VL_OCR_FORCE_CPU");
    if std::env::var_os("CRISPSORTER_PROBE_REAL").is_some() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let cfg = crispsorter_ocr_qualification::extractors::ocr_presets::apply(&OcrPipelineConfig::default(), "multilingual").unwrap();
        let regions = ocr_worker::run_cpu_budget(&fixture, &cfg).expect("isolated real-model OCR");
        let text = regions.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ").to_lowercase();
        assert!(["cobalt", "warranty", "2031"].iter().all(|word| text.contains(word)), "known fixture evidence missing; synthetic OCR output: {text:?}");
        assert_eq!(std::env::var_os("CRISPEMBED_MAX_PIXELS"), before_pixels);
        assert_eq!(std::env::var_os("QWEN2VL_OCR_FORCE_CPU"), before_cpu);
        println!("Real released-model OCR passed in an isolated CPU-budget worker; parent environment unchanged.");
        return;
    }
    let regions = ocr_worker::run_cpu_budget(std::path::Path::new("fixture.png"), &OcrPipelineConfig::default()).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].text, "worker response");
    assert_eq!(std::env::var_os("CRISPEMBED_MAX_PIXELS"), before_pixels);
    assert_eq!(std::env::var_os("QWEN2VL_OCR_FORCE_CPU"), before_cpu);
    println!("OCR worker process protocol and parent environment isolation passed; no model inference.");
}
