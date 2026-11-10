//! Compile and exercise the production OCR-only exporter without the app shell.
#[path = "../../src-tauri/src/ocr_text_export.rs"]
pub mod ocr_text_export;

#[cfg(all(test, feature = "ocr-text-export"))]
mod artifacts {
    #[test]
    fn write_actual_exporter_outputs_for_independent_readers() {
        let output = std::path::PathBuf::from(std::env::var_os("OCR_TEXT_EXPORT_ARTIFACTS")
            .expect("set OCR_TEXT_EXPORT_ARTIFACTS to a writable artifact directory"));
        std::fs::create_dir_all(&output).unwrap();
        let source = include_str!("fixtures.json");
        let fixtures: serde_json::Value = serde_json::from_str(source).unwrap();
        for fixture in fixtures["cases"].as_array().unwrap() {
            let name = fixture["id"].as_str().unwrap();
            let pages: Vec<Vec<&str>> = fixture["pages"].as_array().unwrap().iter()
                .map(|page| page.as_array().unwrap().iter().map(|region| region.as_str().unwrap()).collect()).collect();
            std::fs::write(output.join(format!("{name}.txt")), crate::ocr_text_export::text(&pages)).unwrap();
            std::fs::write(output.join(format!("{name}.html")), crate::ocr_text_export::html(&pages).unwrap()).unwrap();
            std::fs::write(output.join(format!("{name}.docx")), crate::ocr_text_export::docx(&pages).unwrap()).unwrap();
        }
        std::fs::write(output.join("fixtures.json"), source).unwrap();
    }
}
