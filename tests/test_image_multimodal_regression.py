"""No-model regression tests for the real image/audio ingest branch.

Runs the branch with fake encoders, so no model resolution or download occurs.
"""
import os
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = pathlib.Path(os.environ.get('IMAGE_INGEST_SOURCE', ROOT / "src-tauri/src/bg_ingest/mod.rs"))


class ImageMultimodalOptIn(unittest.TestCase):
    def test_ingest_encoder_dispatch(self):
        source = SOURCE.read_text()
        start = source.index('                let ext_lower = raw.ext.to_ascii_lowercase();')
        end = source.index('\n            // P13.7 Stage F + N', start)
        branch = source[start:end]
        # The final brace closes the original scope, opened below.
        harness = r'''
use std::sync::atomic::{AtomicUsize, Ordering};
use std::future::Future;
use std::task::{Context, Poll, Wake, Waker};
use std::sync::Arc;
static VIT: AtomicUsize = AtomicUsize::new(0);
static IMAGE: AtomicUsize = AtomicUsize::new(0);
static AUDIO: AtomicUsize = AtomicUsize::new(0);
mod images { pub mod vit_embed {
    pub fn is_vit_available() -> bool { true }
    pub fn embed_image(_: &std::path::Path) -> Result<Vec<f32>, String> {
        crate::VIT.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Ok(vec![1.])
    }
} }
mod index { pub mod omni_embed {
    pub fn is_omni_available() -> bool { true }
    pub fn encode_image_omni(_: &std::path::Path) -> Result<Vec<f32>, String> {
        crate::IMAGE.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Ok(vec![2.])
    }
    pub fn encode_audio_omni(_: &[f32]) -> Result<Vec<f32>, String> {
        crate::AUDIO.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Ok(vec![3.])
    }
} }
mod tokio { pub mod task {
    pub fn spawn_blocking<F, T>(f: F) -> impl std::future::Future<Output=Result<T, String>>
    where F: FnOnce() -> T { async move { Ok(f()) } }
} }
struct Raw { ext: String, embedding_vit: Option<Vec<f32>>, embedding_omni: Option<Vec<f32>> }
struct Extracted { audio_pcm: Option<Vec<f32>> }
async fn run(ext: &str, image_indexing_enabled: bool) -> Raw {
    let mut raw = Raw { ext: ext.into(), embedding_vit: None, embedding_omni: None };
    let p = std::path::PathBuf::from("fixture");
    let extracted = Extracted { audio_pcm: Some(vec![0.]) };
    {
''' + branch + r'''
    raw
}
struct Noop; impl Wake for Noop { fn wake(self: Arc<Self>) {} }
fn block_on<F: Future>(f: F) -> F::Output {
    let waker = Waker::from(Arc::new(Noop)); let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop { if let Poll::Ready(value) = f.as_mut().poll(&mut cx) { return value; } }
}
fn main() {
    let disabled = block_on(run("png", false));
    assert!(disabled.embedding_vit.is_none() && disabled.embedding_omni.is_none(), "disabled image must not call encoders");
    assert_eq!(VIT.load(Ordering::SeqCst), 0); assert_eq!(IMAGE.load(Ordering::SeqCst), 0);
    let enabled = block_on(run("png", true));
    assert!(enabled.embedding_vit.is_some() && enabled.embedding_omni.is_some());
    let audio = block_on(run("mp3", false));
    assert!(audio.embedding_omni.is_some(), "image flag must not gate audio");
    assert_eq!(AUDIO.load(Ordering::SeqCst), 1);
}
'''
        with tempfile.TemporaryDirectory() as tmp:
            rs = pathlib.Path(tmp) / "dispatch.rs"
            binary = pathlib.Path(tmp) / "dispatch"
            rs.write_text(harness)
            subprocess.run(["rustc", "--edition=2021", str(rs), "-o", str(binary)], check=True, capture_output=True, text=True)
            run = subprocess.run([str(binary)], capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)

    def test_disabled_image_guard_skips_both_encoders(self):
        import re
        source = SOURCE.read_text()
        condition = re.search(r'if (is_image && [^\n]+) \{', source).group(1)
        python_condition = condition.replace('&&', ' and ').replace('||', ' or ')
        values = dict(is_image=True, vit_available=True, omni_available=True,
                      image_indexing_enabled=False)
        self.assertFalse(eval(python_condition, {'__builtins__': {}}, values),
                         'disabled images must not dispatch either encoder')
        values['image_indexing_enabled'] = True
        self.assertTrue(eval(python_condition, {'__builtins__': {}}, values),
                        'enabled images should dispatch available encoders')

    def test_index_config_is_propagated_to_ingest(self):
        source = SOURCE.read_text()
        self.assertTrue('g.config.image_indexing_enabled' in source,
                        'the saved image indexing setting must reach the ingest path')


if __name__ == "__main__":
    unittest.main()
