"""Exercise the real encoder wrappers using a no-network CrispEmbed test double."""
import os
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


def source(path):
    text = pathlib.Path(os.environ.get(path.name.upper() + '_SOURCE', path)).read_text()
    text = text.split('// ── Tests', 1)[0]
    return '\n'.join(line for line in text.splitlines() if not line.startswith('//!'))


class EncoderInitialization(unittest.TestCase):
    def test_resolution_and_initialization_failures_are_recoverable(self):
        omni = source(ROOT / 'src-tauri/src/index/omni_embed.rs')
        vit = source(ROOT / 'src-tauri/src/images/vit_embed.rs')
        harness = r'''
extern crate self as anyhow;
extern crate self as crispembed;
use std::sync::atomic::{AtomicUsize, Ordering};
pub type Result<T> = std::result::Result<T, String>;
#[macro_export] macro_rules! anyhow { ($($arg:tt)*) => { format!($($arg)*) }; }
pub trait Context<T> { fn context(self, s: &str) -> Result<T>; }
impl<T> Context<T> for Option<T> { fn context(self, s: &str) -> Result<T> { self.ok_or_else(|| s.to_string()) } }
static MODE: AtomicUsize = AtomicUsize::new(0);
static NEW_CALLS: AtomicUsize = AtomicUsize::new(0);
pub struct CrispEmbed;
impl CrispEmbed {
    pub fn resolve_model(path: &str, _: Option<bool>) -> std::result::Result<String, String> {
        if MODE.load(Ordering::SeqCst) == 1 { Err("resolve failure".into()) } else { Ok(path.into()) }
    }
    pub fn new(_: &str, _: i32) -> std::result::Result<Self, String> {
        NEW_CALLS.fetch_add(1, Ordering::SeqCst);
        if MODE.load(Ordering::SeqCst) == 2 { Err("init failure".into()) } else { Ok(Self) }
    }
    pub fn encode(&mut self, _: &str) -> Vec<f32> { vec![0.; 2048] }
    pub fn encode_batch(&mut self, _: &[&str]) -> Vec<Vec<f32>> { vec![vec![1.]] }
    pub fn has_audio(&self) -> bool { true }
    pub fn has_vision(&self) -> bool { true }
    pub fn encode_audio(&mut self, _: &[f32]) -> Vec<f32> { vec![1.] }
    pub fn encode_image_file(&mut self, _: &str) -> Vec<f32> { vec![1.] }
    pub fn encode_text_with_image_file(&mut self, _: &str, _: &str) -> Vec<f32> { vec![1.] }
}
pub struct CrispVit;
impl CrispVit {
    pub fn new(_: &str, _: i32) -> std::result::Result<Self, String> {
        NEW_CALLS.fetch_add(1, Ordering::SeqCst);
        if MODE.load(Ordering::SeqCst) == 2 { Err("init failure".into()) } else { Ok(Self) }
    }
    pub fn encode_file(&mut self, _: &str) -> Vec<f32> { vec![1.] }
    pub fn dim(&self) -> i32 { 768 }
}
mod embedder { pub fn crispembed_threads() -> i32 { 0 } }
mod omni {
''' + omni + '\n}\nmod vit {\n' + vit + r'''
}
fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let invoke: Box<dyn Fn() -> Result<()>> = match mode.as_str() {
        "omni" => Box::new(|| omni::encode_text_omni("test").map(|_| ())),
        "vit_image" => Box::new(|| vit::embed_image(std::path::Path::new("image.png")).map(|_| ())),
        "vit_dim" => Box::new(|| vit::vit_dim().map(|_| ())),
        "vit_custom" => Box::new(|| vit::embed_image_with_model(std::path::Path::new("image.png"), "model").map(|_| ())),
        _ => unreachable!(),
    };
    if std::env::args().nth(2).as_deref() == Some("init") {
        MODE.store(2, Ordering::SeqCst);
        let err = invoke().expect_err("initialization failure must be returned");
        assert!(err.contains("init failure"), "init error lost: {err}");
        MODE.store(0, Ordering::SeqCst);
        assert!(invoke().is_ok(), "a later attempt must recover");
        return;
    }
    MODE.store(1, Ordering::SeqCst);
    let err = invoke().expect_err("resolution failure must be returned");
    assert!(err.contains("resolve failure"), "resolution error lost: {err}");
    assert_eq!(NEW_CALLS.load(Ordering::SeqCst), 0, "do not try to load after resolution fails");
    MODE.store(2, Ordering::SeqCst);
    let err = invoke().expect_err("initialization failure must be returned");
    assert!(err.contains("init failure"), "init error lost: {err}");
    MODE.store(0, Ordering::SeqCst);
    assert!(invoke().is_ok(), "a later attempt must recover");
}
'''
        with tempfile.TemporaryDirectory() as tmp:
            rs = pathlib.Path(tmp) / 'encoders.rs'
            binary = pathlib.Path(tmp) / 'encoders'
            rs.write_text(harness)
            compiled = subprocess.run(['rustc', '--edition=2021', '--cfg', 'feature="crispembed"', str(rs), '-o', str(binary)], capture_output=True, text=True)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            for kind in ('omni', 'vit_image', 'vit_dim', 'vit_custom'):
                for failure in ('resolve', 'init'):
                    with self.subTest(kind=kind, failure=failure):
                        run = subprocess.run([str(binary), kind, failure], capture_output=True, text=True)
                        self.assertEqual(run.returncode, 0, run.stderr)


if __name__ == '__main__':
    unittest.main()
