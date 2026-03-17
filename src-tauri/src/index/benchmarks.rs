use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tempfile::TempDir;

use super::{
    Embedder, EmbedderConfig, EmbedderModel, EmbedderDevice,
    FtsIndex, LocalIndex, SearchEngine, IngestPipeline, IngestConfig,
    RawDocument, SearchFilters
};

#[tokio::test]
async fn benchmark_models() {
    let models = vec![
        // ── fastembed native ─────────────────────────────────────────────────
        EmbedderModel::BgeM3,
        // ── fastembed UserDefined (self-contained ONNX) ──────────────────────
        EmbedderModel::MultilingualMiniLm,
        EmbedderModel::JinaV2Small,
        EmbedderModel::JinaV2Base,
        EmbedderModel::SnowflakeArcticLv2,
        // ── OrtPath: external data models ────────────────────────────────────
        EmbedderModel::JinaV5Nano,
        EmbedderModel::JinaV3,
        EmbedderModel::PixieRuneV1,
        // ── Qwen3-Embedding-0.6B (base, decoder with KV-cache) ───────────────
        EmbedderModel::Qwen3EmbeddingInt8,   // onnx-community int8
        EmbedderModel::Qwen3EmbeddingUint8,  // electroglyph uint8 calibrated
        // ── Octen-Embedding-0.6B (Qwen3 finetune, encoder-style) ─────────────
        EmbedderModel::Octen06bInt8,         // geoffsee int8 (self-contained)
        EmbedderModel::Octen06bFp32,         // geoffsee fp32
        EmbedderModel::Octen06bInt4,         // geoffsee int4
        EmbedderModel::Octen06bFp16,         // geoffsee fp16 (may fail CPU)
    ];

    for model in models {
        println!("\n=== Benchmarking Model: {:?} ===", model);
        if let Err(e) = run_benchmark_for_model(model).await {
            println!("SKIPPED — {e:#}");
        }
    }
}

async fn run_benchmark_for_model(model: EmbedderModel) -> anyhow::Result<()> {
    // Use the actual app data dir path to share the model cache.
    let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/com.<user>.crispsorter/models");
    let data_dir = TempDir::new()?;

    // 1. Initialisation
    let start_init = Instant::now();
    let embedder_cfg = EmbedderConfig::new(model, EmbedderDevice::Cpu, models_dir);
    let embedder = Embedder::new(embedder_cfg).await?;
    let embedder_arc = Arc::new(Mutex::new(embedder));
    let init_duration = start_init.elapsed();
    println!("Init Time: {:?}", init_duration);

    let fts = Arc::new(FtsIndex::open_or_create(&data_dir.path().join("fts"))?);
    let local = Arc::new(LocalIndex::open_or_create(data_dir.path(), model.dims()).await?);
    let engine = SearchEngine::new(fts.clone(), local.clone(), embedder_arc.clone());
    let pipeline = IngestPipeline::new(fts.clone(), local.clone(), embedder_arc.clone(), IngestConfig::default());

    // 2. Ingestion Performance & Accuracy Data
    let docs = vec![
        RawDocument {
            full_text: "Constitutional AI: Harmlessness from AI Feedback. This document discusses safety and RLHF. Recht, Recht, Recht.".to_owned(),
            full_text_md: String::new(),
            headings: vec!["Safety".to_owned()],
            title: Some("Constitutional AI".to_owned()),
            author: Some("Bai et al".to_owned()),
            year: Some(2022),
            filename: "bai.pdf".to_owned(),
            ext: "pdf".to_owned(),
            language: "en".to_owned(),
            source_hash: "hash1".to_owned(),
            location_uri: "uri1".to_owned(),
            owner_id: "user1".to_owned(),
            tags: vec![],
        },
        RawDocument {
            full_text: "Während Angriffe auf den Rechtsstaat in Europa zunehmen...".to_owned(),
            full_text_md: String::new(),
            headings: vec!["Workshop".to_owned()],
            title: Some("Recht unter Druck".to_owned()),
            author: Some("Jüngling".to_owned()),
            year: Some(2027),
            filename: "recht.docx".to_owned(),
            ext: "docx".to_owned(),
            language: "de".to_owned(),
            source_hash: "hash2".to_owned(),
            location_uri: "uri2".to_owned(),
            owner_id: "user1".to_owned(),
            tags: vec![],
        },
        RawDocument {
            full_text: "Integrationsdialog? Zeithistorisch akzentuierte Perspektiven auf sozialintegrative Potentiale des christlich-islamischen Dialogs. Wilhelm Heitmeyer.".to_owned(),
            full_text_md: String::new(),
            headings: vec!["Essay".to_owned()],
            title: Some("Integration - Dialog".to_owned()),
            author: Some("Academic Author".to_owned()),
            year: Some(2019),
            filename: "ruesch.pdf".to_owned(),
            ext: "pdf".to_owned(),
            language: "de".to_owned(),
            source_hash: "hash3".to_owned(),
            location_uri: "uri3".to_owned(),
            owner_id: "user1".to_owned(),
            tags: vec![],
        }
    ];

    let start_ingest = Instant::now();
    let mut total_chunks = 0;
    for doc in docs {
        let stats = pipeline.ingest_document(doc).await?;
        total_chunks += stats.chunk_count;
    }
    let ingest_duration = start_ingest.elapsed();
    println!("Ingest Speed: {:.2} chunks/sec", total_chunks as f64 / ingest_duration.as_secs_f64());

    // 3. Search Latency
    let queries = vec!["Rahner", "Recht", "integration", "theology", "Druck"];
    let start_hybrid = Instant::now();
    for q in &queries {
        let _ = engine.search_hybrid(q, &SearchFilters::default(), 10).await?;
    }
    println!("Avg Hybrid Latency: {:?}", start_hybrid.elapsed() / queries.len() as u32);

    // 4. Accuracy (0.0 – 1.0) — check top-1 result for each test query.
    let mut score = 0.0f64;

    let res1 = engine.search_hybrid("Recht unter Druck", &SearchFilters::default(), 1).await?;
    if res1.first().map(|r| r.title.as_deref().unwrap_or("")) == Some("Recht unter Druck") {
        score += 0.5;
    }
    let res2 = engine.search_hybrid("Integrationsdialog Heitmeyer", &SearchFilters::default(), 1).await?;
    if res2.first().map(|r| r.title.as_deref().unwrap_or("")) == Some("Integration - Dialog") {
        score += 0.5;
    }
    println!("Accuracy Score: {:.2}", score);

    // 5. Memory Usage
    {
        use sysinfo::{System, RefreshKind};
        let mut sys = System::new_with_specifics(RefreshKind::everything());
        sys.refresh_all();
        if let Ok(pid) = sysinfo::get_current_pid() {
            if let Some(proc) = sys.process(pid) {
                println!("Memory (RSS): {} MB", proc.memory() / 1024 / 1024);
            }
        }
    }
    Ok(())
}
