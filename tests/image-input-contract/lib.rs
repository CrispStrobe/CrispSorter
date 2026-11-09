// Compile the application's actual streaming source guard with the exact JPEG
// decoder versions pinned by the full app, without bootstrapping Tauri/ONNX.
#[cfg(test)]
#[path = "../../src-tauri/src/extractors/image_input.rs"]
mod image_input;
#[cfg(test)]
#[path = "../../src-tauri/src/ocr_asset_store.rs"]
mod asset_store;

#[cfg(test)]
mod tests {
    use super::{asset_store, image_input};
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

    #[test]
    fn identical_existing_assets_are_verified_in_bounded_chunks_without_modification() {
        let directory = SourceDirectory::new("identical-asset");
        let assets = asset_store::directory(&directory.0).unwrap();
        let path = assets.join("page.png");
        let bytes: Vec<u8> = (0..200_003).map(|index| (index % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mut expected = std::io::Cursor::new(bytes.clone());
        expected.set_position(bytes.len() as u64);
        asset_store::verify_existing(&path, &mut expected).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(expected.into_inner(), bytes);
    }

    #[test]
    fn damaged_existing_assets_fail_without_overwriting_either_snapshot() {
        let directory = SourceDirectory::new("damaged-asset");
        let assets = asset_store::directory(&directory.0).unwrap();
        let path = assets.join("page.png");
        let bytes = vec![42u8; 200_003];
        let mut damaged = bytes.clone();
        damaged[150_000] ^= 1;
        let mut oversized = bytes.clone(); oversized.push(0);
        for existing in [&bytes[..bytes.len() - 1], damaged.as_slice(), oversized.as_slice()] {
            std::fs::write(&path, existing).unwrap();
            let mut expected = std::io::Cursor::new(bytes.clone());
            assert!(asset_store::verify_existing(&path, &mut expected).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), existing);
            assert_eq!(expected.into_inner(), bytes);
        }
    }

    #[cfg(unix)]
    #[test]
    fn asset_and_directory_aliases_cannot_redirect_private_snapshot_writes() {
        use std::os::unix::fs::symlink;
        let directory = SourceDirectory::new("asset-alias");
        let outside = SourceDirectory::new("outside-asset");
        let bytes = b"complete snapshot";
        let target = outside.0.join("source.png");
        std::fs::write(&target, bytes).unwrap();
        symlink(&outside.0, directory.0.join("assets")).unwrap();
        assert!(asset_store::directory(&directory.0).is_err());
        std::fs::remove_file(directory.0.join("assets")).unwrap();
        let assets = asset_store::directory(&directory.0).unwrap();
        let alias = assets.join("page.png");
        symlink(&target, &alias).unwrap();
        assert!(asset_store::verify_existing(&alias, &mut std::io::Cursor::new(bytes)).is_err());
        assert!(asset_store::verify_existing(&assets, &mut std::io::Cursor::new(bytes)).is_err());
        assert_eq!(std::fs::read(target).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 1);
    }
}
