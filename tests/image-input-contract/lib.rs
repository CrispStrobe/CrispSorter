// Compile the application's actual streaming source guard with the exact JPEG
// decoder versions pinned by the full app, without bootstrapping Tauri/ONNX.
#[cfg(test)]
#[path = "../../src-tauri/src/extractors/image_input.rs"]
mod image_input;

#[cfg(test)]
mod tests {
    use super::image_input;
    use base64::Engine;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct SourceDirectory(PathBuf);
    impl SourceDirectory {
        fn new(case: &str) -> Self {
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "ocr-image-source-{}-{nonce}-{case}", std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for SourceDirectory {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn fixture() -> Vec<u8> {
        let encoded = include_str!("../fixtures/mobile-documents/oriented.jpg.b64")
            .split_whitespace().collect::<String>();
        base64::engine::general_purpose::STANDARD.decode(encoded).unwrap()
    }

    fn observe(case: &str, bytes: &[u8], complete: bool) {
        let directory = SourceDirectory::new(case);
        let path = directory.0.join("photo.jpg");
        std::fs::write(&path, bytes).unwrap();
        let source_check = image_input::validate_file(&path);
        let decoded = image::open(&path);
        println!("OCR_JPEG_OBSERVATION {{\"case\":\"{case}\",\"bytes\":{},\"decoder_accepted\":{},\"source_guard_accepted\":{}}}",
            bytes.len(), decoded.is_ok(), source_check.is_ok());
        assert_eq!(source_check.is_ok(), complete, "{case}: {source_check:?}");
        if complete {
            let image = decoded.unwrap();
            assert_eq!((image.width(), image.height()), (120, 80));
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "source changed: {case}");
    }

    #[test]
    fn rejects_incomplete_real_jpegs_and_records_raw_decoder_tolerance() {
        let complete = fixture();
        observe("missing-end-marker", &complete[..complete.len() - 2], false);
        observe("truncated-scan", &complete[..complete.len() - 100], false);
        let mut metadata_end = vec![0xff, 0xd8, 0xff, 0xef, 0, 4, 0xff, 0xd9];
        metadata_end.extend_from_slice(&complete[2..complete.len() - 100]);
        observe("metadata-end-marker-with-truncated-scan", &metadata_end, false);
    }

    #[test]
    fn preserves_complete_jpegs_with_metadata_markers_and_legal_trailing_data() {
        let complete = fixture();
        observe("complete", &complete, true);
        let mut trailing = complete.clone();
        trailing.extend_from_slice(b"trailing camera metadata");
        observe("legal-trailing-data", &trailing, true);
        let mut metadata_end = vec![0xff, 0xd8, 0xff, 0xef, 0, 4, 0xff, 0xd9];
        metadata_end.extend_from_slice(&complete[2..]);
        observe("legal-metadata-end-marker", &metadata_end, true);
    }

    #[test]
    fn unavailable_sources_fail_before_any_decoder_or_fallback() {
        let directory = SourceDirectory::new("missing");
        assert_eq!(image_input::validate_file(&directory.0.join("missing.jpg")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound);
    }
}
