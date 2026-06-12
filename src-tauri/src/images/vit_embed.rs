//! P17.7 — Standalone ViT image embeddings via CrispEmbed.
//!
//! Wraps `crispembed::CrispVit` (SigLIP / CLIP) to encode images into
//! a dense vector space shared with text.  Enables "find similar images"
//! by cosine similarity over the ViT embedding column — works across
//! different crops, formats, and resolutions unlike perceptual hashing.
//!
//! The embedding is written to `embedding_vit` (FixedSizeList) in
//! LanceDB via schema migration v109.
//!
//! Gated behind `--features crispembed`.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;

/// Default ViT model — SigLIP base (768-D).
const DEFAULT_VIT_MODEL: &str = "siglip-base";

/// Process-global lazy-loaded ViT encoder.
#[cfg(feature = "crispembed")]
static VIT_ENCODER: std::sync::OnceLock<Mutex<crispembed::CrispVit>> =
    std::sync::OnceLock::new();

/// Check if ViT image embedding is available at runtime.
pub fn is_vit_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Encode an image file to a dense embedding via SigLIP/CLIP.
///
/// Returns an L2-normalized embedding vector.  Empty on failure.
#[cfg(feature = "crispembed")]
pub fn embed_image(image_path: &Path) -> Result<Vec<f32>> {
    let path_str = image_path
        .to_str()
        .context("image path is not valid UTF-8")?;

    let encoder = VIT_ENCODER.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_VIT_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_VIT_MODEL.to_string());
        let vit = crispembed::CrispVit::new(&resolved, 0)
            .expect("ViT model init failed");
        Mutex::new(vit)
    });

    let mut guard = encoder
        .lock()
        .map_err(|e| anyhow::anyhow!("ViT encoder lock poisoned: {e}"))?;

    let embedding = guard.encode_file(path_str);
    if embedding.is_empty() {
        return Err(anyhow::anyhow!(
            "ViT encoding returned empty vector for {}",
            image_path.display()
        ));
    }
    Ok(embedding)
}

/// Get the embedding dimension of the loaded ViT model.
#[cfg(feature = "crispembed")]
pub fn vit_dim() -> Result<usize> {
    let encoder = VIT_ENCODER.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_VIT_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_VIT_MODEL.to_string());
        let vit = crispembed::CrispVit::new(&resolved, 0)
            .expect("ViT model init failed");
        Mutex::new(vit)
    });
    let guard = encoder
        .lock()
        .map_err(|e| anyhow::anyhow!("ViT encoder lock poisoned: {e}"))?;
    Ok(guard.dim() as usize)
}

/// Encode an image with a custom ViT model path.
#[cfg(feature = "crispembed")]
pub fn embed_image_with_model(image_path: &Path, model_path: &str) -> Result<Vec<f32>> {
    let path_str = image_path
        .to_str()
        .context("image path is not valid UTF-8")?;
    let resolved = crispembed::CrispEmbed::resolve_model(model_path, Some(true))
        .unwrap_or_else(|_| model_path.to_string());
    let mut vit = crispembed::CrispVit::new(&resolved, 0)
        .map_err(|e| anyhow::anyhow!("ViT init failed: {e}"))?;
    let embedding = vit.encode_file(path_str);
    if embedding.is_empty() {
        return Err(anyhow::anyhow!("ViT encoding returned empty vector"));
    }
    Ok(embedding)
}

/// Compute cosine similarity between two image embeddings.
pub fn image_similarity(a: &[f32], b: &[f32]) -> f32 {
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
pub fn embed_image(_image_path: &Path) -> Result<Vec<f32>> {
    Err(anyhow::anyhow!(
        "ViT image embedding requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn vit_dim() -> Result<usize> {
    Err(anyhow::anyhow!(
        "ViT image embedding requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn embed_image_with_model(_image_path: &Path, _model_path: &str) -> Result<Vec<f32>> {
    Err(anyhow::anyhow!(
        "ViT image embedding requires --features crispembed"
    ))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_matches_feature() {
        let available = is_vit_available();
        if cfg!(feature = "crispembed") {
            assert!(available);
        } else {
            assert!(!available);
        }
    }

    #[test]
    fn image_similarity_identical() {
        let v = vec![0.5, 0.5, 0.5, 0.5];
        let sim = image_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn image_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = image_similarity(&a, &b);
        assert!(sim.abs() < 1e-5);
    }

    #[test]
    fn image_similarity_empty() {
        assert_eq!(image_similarity(&[], &[]), 0.0);
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_embed_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("photo.jpg");
        std::fs::write(&p, b"\xFF\xD8").unwrap();
        assert!(embed_image(&p).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_dim_returns_error() {
        assert!(vit_dim().is_err());
    }

    // ── Live tests ──────────────────────────────────────────────────

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed vit_embed_live -- --ignored
    fn vit_embed_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(224, 224);
        img.save(&img_path).unwrap();
        let emb = embed_image(&img_path).expect("ViT should encode a valid image");
        assert!(!emb.is_empty(), "embedding should not be empty");
        println!("ViT embedding dim: {}", emb.len());
        // Check L2-normalization.
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "embedding should be L2-normalized, got norm={norm}"
        );
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn vit_similarity_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Two identical images should have sim ≈ 1.0.
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(224, 224);
        img.save(&img_path).unwrap();
        let emb1 = embed_image(&img_path).unwrap();
        let emb2 = embed_image(&img_path).unwrap();
        let sim = image_similarity(&emb1, &emb2);
        assert!(
            sim > 0.99,
            "identical images should have sim > 0.99, got {sim}"
        );
    }
}
