use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tokio::sync::Mutex;

use crisp_index_protocol::IngestBatch;

#[derive(Clone)]
pub struct ServerEmbedder {
    inner: Arc<Mutex<TextEmbedding>>,
}

impl ServerEmbedder {
    pub fn load_bge_m3(cache_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(cache_dir)
            .with_context(|| format!("creating embedder cache dir {}", cache_dir.display()))?;
        let opts = TextInitOptions::new(EmbeddingModel::BGEM3)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_show_download_progress(true);
        let model = TextEmbedding::try_new(opts).context("loading server fastembed BGE-M3")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(model)),
        })
    }

    pub async fn hydrate_missing_embeddings(&self, batch: &mut IngestBatch) -> Result<usize> {
        let missing: Vec<usize> = batch
            .chunks
            .iter()
            .enumerate()
            .filter_map(|(i, chunk)| chunk.embedding.is_empty().then_some(i))
            .collect();
        if missing.is_empty() {
            return Ok(0);
        }

        let texts: Vec<String> = missing
            .iter()
            .map(|&i| batch.chunks[i].full_text.clone())
            .collect();
        let vectors = {
            let mut inner = self.inner.lock().await;
            inner
                .embed(texts, Some(32))
                .context("server fastembed batch embed")?
        };
        if vectors.len() != missing.len() {
            anyhow::bail!(
                "server embedder returned {} vectors for {} missing chunks",
                vectors.len(),
                missing.len()
            );
        }
        for (row_idx, vector) in missing.into_iter().zip(vectors.into_iter()) {
            batch.chunks[row_idx].embedding = vector;
        }
        Ok(batch.chunks.iter().filter(|c| !c.embedding.is_empty()).count())
    }

    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let vectors = {
            let mut inner = self.inner.lock().await;
            inner
                .embed(vec![text.to_owned()], Some(1))
                .context("server fastembed query embed")?
        };
        vectors
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("server embedder returned no query vector"))
    }
}
