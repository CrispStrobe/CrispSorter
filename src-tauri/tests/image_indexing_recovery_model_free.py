"""Model-free regression tests: python3 src-tauri/tests/image_indexing_recovery_model_free.py.

Compiles the production encoder modules against deterministic stand-ins; no
models, Cargo dependencies, downloads, or sibling checkout writes are needed.
"""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

HARNESS = r'''
#![allow(dead_code)]
extern crate self as anyhow;
extern crate self as crispembed;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::path::Path;

pub type Result<T> = std::result::Result<T, String>;
#[macro_export] macro_rules! anyhow { ($($arg:tt)*) => { format!($($arg)*) } }
pub trait Context<T> { fn context(self, msg: &str) -> Result<T>; }
impl<T> Context<T> for Option<T> {
    fn context(self, msg: &str) -> Result<T> { self.ok_or_else(|| msg.to_string()) }
}
// stage 1: resolution error; 2: constructor error; 3/4: panic at those stages.
static VIT_STAGE: AtomicUsize = AtomicUsize::new(0);
static OMNI_STAGE: AtomicUsize = AtomicUsize::new(0);
static VIT_CREATED: AtomicUsize = AtomicUsize::new(0);
static OMNI_CREATED: AtomicUsize = AtomicUsize::new(0);
fn state(model: &str) -> &'static AtomicUsize {
    if model.contains("siglip") { &VIT_STAGE } else { &OMNI_STAGE }
}
pub struct CrispEmbed;
impl CrispEmbed {
    pub fn resolve_model(model: &str, _: Option<bool>) -> Result<String> {
        match state(model).load(Ordering::SeqCst) {
            1 => Err("resolve failure".into()),
            3 => panic!("resolution panic"),
            _ => Ok(model.into()),
        }
    }
    pub fn new(model: &str, _: i32) -> Result<Self> {
        match state(model).load(Ordering::SeqCst) {
            2 => return Err("constructor failure".into()),
            4 => panic!("constructor panic"),
            _ => (),
        }
        OMNI_CREATED.fetch_add(1, Ordering::SeqCst);
        Ok(Self)
    }
    pub fn encode(&mut self, _: &str) -> Vec<f32> { vec![1.0; 2048] }
    pub fn encode_batch(&mut self, _: &[&str]) -> Vec<Vec<f32>> { vec![vec![1.0; 2048]] }
    pub fn has_audio(&self) -> bool { true }
    pub fn has_vision(&self) -> bool { true }
    pub fn encode_audio(&mut self, _: &[f32]) -> Vec<f32> { vec![1.0; 2048] }
    pub fn encode_image_file(&mut self, _: &str) -> Vec<f32> { vec![1.0; 2048] }
    pub fn encode_text_with_image_file(&mut self, _: &str, _: &str) -> Vec<f32> { vec![1.0; 2048] }
}
pub struct CrispVit;
impl CrispVit {
    pub fn new(model: &str, _: i32) -> Result<Self> {
        match state(model).load(Ordering::SeqCst) {
            2 => return Err("constructor failure".into()),
            4 => panic!("constructor panic"),
            _ => (),
        }
        VIT_CREATED.fetch_add(1, Ordering::SeqCst);
        Ok(Self)
    }
    pub fn encode_file(&mut self, _: &str) -> Vec<f32> { vec![1.0; 768] }
    pub fn dim(&self) -> i32 { 768 }
}
// The embedders resolve through the app's provisioning funnel (desktop:
// licence-aware registry resolution; mobile: verified installs). The stub
// forwards to the stubbed resolver so every injected stage is still exercised.
mod model_provisioning {
    pub fn resolve_model(model: &str, auto_download: Option<bool>) -> crate::Result<String> {
        crate::CrispEmbed::resolve_model(model, auto_download)
    }
}
#[path = "__VIT__"] mod vit;
mod index {
    pub mod embedder { pub fn crispembed_threads() -> i32 { 1 } }
    #[path = "__OMNI__"] pub mod omni;
}
fn main() {
    // Suppress only the deliberately injected panics.
    std::panic::set_hook(Box::new(|info| {
        let message = info.payload().downcast_ref::<&str>().copied().unwrap_or("");
        if message != "resolution panic" && message != "constructor panic" { eprintln!("{info}"); }
    }));
    for stage in [1, 2, 3, 4] {
        VIT_STAGE.store(stage, Ordering::SeqCst);
        let v = std::panic::catch_unwind(|| vit::vit_dim());
        assert!(v.is_ok(), "ViT initialization must not panic at stage {stage}");
        assert!(v.unwrap().is_err(), "ViT stage {stage} must fail safely");
        let custom = std::panic::catch_unwind(|| vit::embed_image_with_model(Path::new("photo.png"), "siglip-custom"));
        assert!(custom.is_ok(), "custom ViT initialization must not panic at stage {stage}");
        assert!(custom.unwrap().is_err(), "custom ViT stage {stage} must fail safely");
        OMNI_STAGE.store(stage, Ordering::SeqCst);
        let o = std::panic::catch_unwind(|| index::omni::encode_text_omni("test"));
        assert!(o.is_ok(), "Omni initialization must not panic at stage {stage}");
        assert!(o.unwrap().is_err(), "Omni stage {stage} must fail safely");
    }
    VIT_STAGE.store(0, Ordering::SeqCst);
    OMNI_STAGE.store(0, Ordering::SeqCst);
    let threads: Vec<_> = (0..16).map(|_| std::thread::spawn(|| {
        assert_eq!(vit::embed_image(Path::new("photo.png")).unwrap().len(), 768);
        assert_eq!(index::omni::encode_image_omni(Path::new("photo.png")).unwrap().len(), 2048);
    })).collect();
    for t in threads { t.join().unwrap(); }
    assert_eq!(VIT_CREATED.load(Ordering::SeqCst), 1);
    assert_eq!(OMNI_CREATED.load(Ordering::SeqCst), 1);
    assert_eq!(vit::vit_dim().unwrap(), 768);
    assert_eq!(index::omni::encode_audio_omni(&[0.0]).unwrap().len(), 2048);
}
'''

class ImageIndexingRecovery(unittest.TestCase):
    def test_default_off_and_gate_covers_both_image_models(self):
        config = (ROOT / 'src-tauri/src/index/mod.rs').read_text()
        self.assertIn('#[serde(default)]\n    pub image_indexing_enabled: bool', config)
        self.assertIn('image_indexing_enabled: false', config)
        ingest = (ROOT / 'src-tauri/src/bg_ingest/mod.rs').read_text()
        self.assertIn('let image_indexing_enabled = g.config.image_indexing_enabled;', ingest)
        start = ingest.index('if is_image && image_indexing_enabled && (vit_available || omni_available)')
        end = ingest.index('// Omni audio embedding', start)
        branch = ingest[start:end]
        self.assertIn('vit_embed::embed_image(&p2)', branch)
        self.assertIn('omni_embed::encode_image_omni(&p2)', branch)
        self.assertIn('if is_audio && omni_available', ingest[end:])

    def test_feature_disabled_stubs_return_errors(self):
        source = HARNESS.split('fn main() {', 1)[0] + r'''
fn main() {
    assert!(!vit::is_vit_available());
    assert!(vit::embed_image(Path::new("photo.png")).is_err());
    assert!(vit::vit_dim().is_err());
    assert!(vit::embed_image_with_model(Path::new("photo.png"), "model").is_err());
    assert!(!index::omni::is_omni_available());
    assert!(index::omni::encode_image_omni(Path::new("photo.png")).is_err());
    assert!(index::omni::encode_text_omni("test").is_err());
    assert!(index::omni::encode_audio_omni(&[0.0]).is_err());
}
'''
        source = source.replace('__VIT__', str(ROOT / 'src-tauri/src/images/vit_embed.rs'))
        source = source.replace('__OMNI__', str(ROOT / 'src-tauri/src/index/omni_embed.rs'))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'harness.rs'
            binary = Path(tmp) / 'harness'
            path.write_text(source)
            subprocess.run(['rustc', '--edition=2021', str(path), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)

    def test_real_encoder_modules_fail_safely_retry_and_initialize_once(self):
        source = HARNESS.replace('__VIT__', str(ROOT / 'src-tauri/src/images/vit_embed.rs'))
        source = source.replace('__OMNI__', str(ROOT / 'src-tauri/src/index/omni_embed.rs'))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'harness.rs'
            binary = Path(tmp) / 'harness'
            path.write_text(source)
            subprocess.run(['rustc', '--edition=2021', '--cfg', 'feature="crispembed"', str(path), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)

if __name__ == '__main__':
    unittest.main()
