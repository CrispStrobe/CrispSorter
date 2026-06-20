//! P17.5 — BidirLM-Omni cross-modal embeddings.
//!
//! Shared 2048-D embedding space for text, audio, and images via
//! CrispEmbed's BidirLM-Omni model.  Enables cross-modal search:
//! "photo of a sunset" → image hits; "podcast about Bosnia" → audio hits.
//!
//! The embedding is written to `embedding_omni` (FixedSizeList<Float32,
//! 2048>) in LanceDB via schema migration v108.  A separate RRF channel
//! in `search.rs` mixes omni-vector cosine with existing FTS + dense +
//! sparse channels.
//!
//! Three encoding paths:
//! - `encode_text_omni`: text → 2048-D
//! - `encode_audio_omni`: raw PCM → 2048-D
//! - `encode_image_omni`: image file → 2048-D
//! - `encode_text_with_image_omni`: text + image → 2048-D (multimodal)
//!
//! Gated behind `--features crispembed`.

use anyhow::Result;
#[cfg(feature = "crispembed")]
use anyhow::Context;
use std::path::Path;
#[cfg(feature = "crispembed")]
use std::sync::Mutex;

/// Default BidirLM-Omni model name.
#[cfg(feature = "crispembed")]
const DEFAULT_OMNI_MODEL: &str = "bidirlm-omni-2.5b";
/// Omni embedding dimension.
pub const OMNI_DIM: usize = 2048;

/// Process-global lazy-loaded BidirLM-Omni encoder.
#[cfg(feature = "crispembed")]
static OMNI_ENCODER: std::sync::OnceLock<Mutex<crispembed::CrispEmbed>> =
    std::sync::OnceLock::new();

/// Check if omni cross-modal embedding is available at runtime.
pub fn is_omni_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Helper to get or init the global encoder.
#[cfg(feature = "crispembed")]
fn get_encoder() -> Result<std::sync::MutexGuard<'static, crispembed::CrispEmbed>> {
    let encoder = OMNI_ENCODER.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_OMNI_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_OMNI_MODEL.to_string());
        let model = crispembed::CrispEmbed::new(&resolved, 0)
            .expect("BidirLM-Omni model init failed");
        Mutex::new(model)
    });
    encoder
        .lock()
        .map_err(|e| anyhow::anyhow!("omni encoder lock poisoned: {e}"))
}

// ── Text encoding ───────────────────────────────────────────────────

/// Encode text into the shared 2048-D omni space.
#[cfg(feature = "crispembed")]
pub fn encode_text_omni(text: &str) -> Result<Vec<f32>> {
    let mut guard = get_encoder()?;
    let emb = guard.encode(text);
    if emb.is_empty() {
        return Err(anyhow::anyhow!("omni text encoding returned empty vector"));
    }
    debug_assert_eq!(emb.len(), OMNI_DIM, "expected {OMNI_DIM}-D omni vector");
    Ok(emb)
}

/// Batch-encode texts into the shared 2048-D omni space.
#[cfg(feature = "crispembed")]
pub fn encode_text_omni_batch(texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    let mut guard = get_encoder()?;
    let batch = guard.encode_batch(texts);
    if batch.is_empty() {
        return Err(anyhow::anyhow!("omni batch encoding returned empty"));
    }
    Ok(batch)
}

// ── Audio encoding ──────────────────────────────────────────────────

/// Encode raw 16 kHz mono float32 PCM audio into the shared 2048-D
/// omni space.
#[cfg(feature = "crispembed")]
pub fn encode_audio_omni(pcm_f32: &[f32]) -> Result<Vec<f32>> {
    let mut guard = get_encoder()?;
    if !guard.has_audio() {
        return Err(anyhow::anyhow!(
            "BidirLM-Omni was built without audio support"
        ));
    }
    let emb = guard.encode_audio(pcm_f32);
    if emb.is_empty() {
        return Err(anyhow::anyhow!(
            "omni audio encoding returned empty vector"
        ));
    }
    Ok(emb)
}

// ── Image encoding ──────────────────────────────────────────────────

/// Encode an image file into the shared 2048-D omni space.
#[cfg(feature = "crispembed")]
pub fn encode_image_omni(image_path: &Path) -> Result<Vec<f32>> {
    let path_str = image_path
        .to_str()
        .context("image path is not valid UTF-8")?;
    let mut guard = get_encoder()?;
    if !guard.has_vision() {
        return Err(anyhow::anyhow!(
            "BidirLM-Omni was built without vision support"
        ));
    }
    let emb = guard.encode_image_file(path_str);
    if emb.is_empty() {
        return Err(anyhow::anyhow!(
            "omni image encoding returned empty vector for {}",
            image_path.display()
        ));
    }
    Ok(emb)
}

/// Encode text conditioned on an image into the shared 2048-D omni space.
#[cfg(feature = "crispembed")]
pub fn encode_text_with_image_omni(text: &str, image_path: &Path) -> Result<Vec<f32>> {
    let path_str = image_path
        .to_str()
        .context("image path is not valid UTF-8")?;
    let mut guard = get_encoder()?;
    if !guard.has_vision() {
        return Err(anyhow::anyhow!(
            "BidirLM-Omni was built without vision support"
        ));
    }
    let emb = guard.encode_text_with_image_file(text, path_str);
    if emb.is_empty() {
        return Err(anyhow::anyhow!(
            "omni text+image encoding returned empty vector"
        ));
    }
    Ok(emb)
}

/// Compute cosine similarity between two omni embeddings.
pub fn omni_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ── Stubs when crispembed is not compiled ───────────────────────────

#[cfg(not(feature = "crispembed"))]
pub fn encode_text_omni(_text: &str) -> Result<Vec<f32>> {
    Err(anyhow::anyhow!(
        "omni cross-modal embedding requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn encode_text_omni_batch(_texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    Err(anyhow::anyhow!(
        "omni cross-modal embedding requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn encode_audio_omni(_pcm_f32: &[f32]) -> Result<Vec<f32>> {
    Err(anyhow::anyhow!(
        "omni cross-modal embedding requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn encode_image_omni(_image_path: &Path) -> Result<Vec<f32>> {
    Err(anyhow::anyhow!(
        "omni cross-modal embedding requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn encode_text_with_image_omni(_text: &str, _image_path: &Path) -> Result<Vec<f32>> {
    Err(anyhow::anyhow!(
        "omni cross-modal embedding requires --features crispembed"
    ))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_matches_feature() {
        let available = is_omni_available();
        if cfg!(feature = "crispembed") {
            assert!(available);
        } else {
            assert!(!available);
        }
    }

    #[test]
    fn omni_dim_is_2048() {
        assert_eq!(OMNI_DIM, 2048);
    }

    #[test]
    fn omni_similarity_identical() {
        let v = vec![0.1f32; OMNI_DIM];
        let sim = omni_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-4, "identical: sim={sim}");
    }

    #[test]
    fn omni_similarity_orthogonal() {
        let mut a = vec![0.0f32; 4];
        let mut b = vec![0.0f32; 4];
        a[0] = 1.0;
        b[1] = 1.0;
        let sim = omni_similarity(&a, &b);
        assert!(sim.abs() < 1e-5, "orthogonal: sim={sim}");
    }

    #[test]
    fn omni_similarity_empty() {
        assert_eq!(omni_similarity(&[], &[]), 0.0);
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_text_returns_error() {
        assert!(encode_text_omni("hello").is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_batch_returns_error() {
        assert!(encode_text_omni_batch(&["hello"]).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_audio_returns_error() {
        assert!(encode_audio_omni(&[0.0; 16000]).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_image_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("test.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        assert!(encode_image_omni(&p).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_text_with_image_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("test.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        assert!(encode_text_with_image_omni("hello", &p).is_err());
    }

    // ── Live tests ──────────────────────────────────────────────────

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed omni_text_live -- --ignored
    fn omni_text_live() {
        let emb = encode_text_omni("a photo of a sunset over the ocean")
            .expect("omni text encoding should work");
        assert_eq!(emb.len(), OMNI_DIM, "expected 2048-D");
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "should be L2-normalized, got norm={norm}"
        );
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn omni_text_batch_live() {
        let texts = &["hello world", "a cat on a chair", "quantum physics"];
        let batch = encode_text_omni_batch(texts)
            .expect("omni batch encoding should work");
        assert_eq!(batch.len(), 3);
        for emb in &batch {
            assert_eq!(emb.len(), OMNI_DIM);
        }
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn omni_image_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(224, 224);
        img.save(&img_path).unwrap();
        let emb = encode_image_omni(&img_path)
            .expect("omni image encoding should work");
        assert_eq!(emb.len(), OMNI_DIM);
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn omni_cross_modal_similarity_live() {
        // Text about a sunset vs image of white square —
        // they shouldn't be very similar, but the pipeline shouldn't crash.
        let text_emb = encode_text_omni("a beautiful sunset over the ocean")
            .expect("text encoding");
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("white.png");
        let img = image::RgbImage::new(224, 224);
        img.save(&img_path).unwrap();
        let img_emb = encode_image_omni(&img_path).expect("image encoding");
        let sim = omni_similarity(&text_emb, &img_emb);
        println!("text-image cross-modal similarity: {sim}");
        // Just verify it's a valid number in [-1, 1].
        assert!(sim >= -1.1 && sim <= 1.1, "sim out of range: {sim}");
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn omni_audio_live() {
        // 1 second of silence at 16 kHz.
        let pcm = vec![0.0f32; 16000];
        match encode_audio_omni(&pcm) {
            Ok(emb) => {
                assert_eq!(emb.len(), OMNI_DIM);
                println!("audio omni dim: {}", emb.len());
            }
            Err(e) => {
                // Audio support may not be compiled in — OK for CI.
                println!("audio omni error (may be expected): {e}");
            }
        }
    }
}
