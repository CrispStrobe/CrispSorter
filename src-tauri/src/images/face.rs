//! P17.4 — Face detection (presence + location) via CrispEmbed.
//!
//! Wraps `crispembed::CrispFace` (YuNet 0.2 MB or SCRFD 16 MB) for
//! detecting WHETHER and WHERE faces appear in an image.
//!
//! **EU AI Act compliance**: This module performs face DETECTION only
//! (presence + bounding box + confidence).  It does NOT perform biometric
//! identification or recognition — no face embeddings, no person matching,
//! no identity inference.  Face detection alone (without biometric
//! identification) is not classified as high-risk under the EU AI Act.
//!
//! Use cases: "this photo contains 3 faces", auto-cropping thumbnails to
//! include faces, filtering search results to "photos with people".
//!
//! Gated behind `--features crispembed`.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;

/// Default detection model — YuNet (0.2 MB, fastest).
const DEFAULT_DET_MODEL: &str = "yunet";

/// A detected face with bounding box and confidence.
#[derive(Debug, Clone)]
pub struct FaceDetection {
    /// Bounding box: (x, y, width, height) in pixels.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Detection confidence [0, 1].
    pub confidence: f32,
    /// 5 facial landmarks (for auto-crop, NOT for identification):
    /// [left_eye_x, left_eye_y, right_eye_x, right_eye_y,
    /// nose_x, nose_y, left_mouth_x, left_mouth_y,
    /// right_mouth_x, right_mouth_y].
    pub landmarks: [f32; 10],
}

/// Lazy-loaded face detector.
#[cfg(feature = "crispembed")]
static FACE_DETECTOR: std::sync::OnceLock<Mutex<crispembed::CrispFace>> =
    std::sync::OnceLock::new();

/// Check if face detection is available at runtime.
pub fn is_face_detection_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Detect faces in an image (bounding boxes + confidence only).
///
/// Returns the number and location of faces found.  Does NOT produce
/// any biometric data (embeddings, identity, age, gender, emotion).
#[cfg(feature = "crispembed")]
pub fn detect_faces(image_path: &Path, conf_threshold: f32) -> Result<Vec<FaceDetection>> {
    let path_str = image_path
        .to_str()
        .context("image path is not valid UTF-8")?;

    let detector = FACE_DETECTOR.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_DET_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_DET_MODEL.to_string());
        let d = crispembed::CrispFace::new(&resolved, 0)
            .expect("face detector init failed");
        Mutex::new(d)
    });

    let mut guard = detector
        .lock()
        .map_err(|e| anyhow::anyhow!("face detector lock poisoned: {e}"))?;

    let raw = guard.detect(path_str, conf_threshold);
    Ok(raw
        .into_iter()
        .map(|d| FaceDetection {
            x: d.x,
            y: d.y,
            w: d.w,
            h: d.h,
            confidence: d.confidence,
            landmarks: d.landmarks,
        })
        .collect())
}

/// Count faces in an image (convenience wrapper).
#[cfg(feature = "crispembed")]
pub fn count_faces(image_path: &Path, conf_threshold: f32) -> Result<usize> {
    Ok(detect_faces(image_path, conf_threshold)?.len())
}

// ── Stubs when crispembed is not compiled ───────────────────────────

#[cfg(not(feature = "crispembed"))]
pub fn detect_faces(_image_path: &Path, _conf_threshold: f32) -> Result<Vec<FaceDetection>> {
    Err(anyhow::anyhow!(
        "face detection requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn count_faces(_image_path: &Path, _conf_threshold: f32) -> Result<usize> {
    Err(anyhow::anyhow!(
        "face detection requires --features crispembed"
    ))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_matches_feature() {
        let available = is_face_detection_available();
        if cfg!(feature = "crispembed") {
            assert!(available);
        } else {
            assert!(!available);
        }
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_detect_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("photo.jpg");
        std::fs::write(&p, b"\xFF\xD8").unwrap();
        assert!(detect_faces(&p, 0.5).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_count_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("photo.jpg");
        std::fs::write(&p, b"\xFF\xD8").unwrap();
        assert!(count_faces(&p, 0.5).is_err());
    }

    // ── Live tests ──────────────────────────────────────────────────

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed face_detect_live -- --ignored
    fn face_detect_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(200, 200);
        img.save(&img_path).unwrap();
        let faces = detect_faces(&img_path, 0.5)
            .expect("detector should not crash on blank image");
        println!("detected {} faces on blank image", faces.len());
        assert!(faces.is_empty(), "blank image should have no faces");
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn face_count_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(200, 200);
        img.save(&img_path).unwrap();
        let n = count_faces(&img_path, 0.5)
            .expect("count should not crash on blank image");
        assert_eq!(n, 0, "blank image should have 0 faces");
    }
}
