use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use tokio::sync::Mutex;

use super::{
    Embedder, EmbedderConfig, EmbedderDevice, EmbedderModel, FtsIndex, IngestConfig,
    IngestPipeline, LocalIndex, RawDocument, SearchEngine, SearchFilters,
};

// ── embedding quality helpers ───────────────────────────────────────────────

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

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
        EmbedderModel::Qwen3EmbeddingInt8,  // onnx-community int8
        EmbedderModel::Qwen3EmbeddingUint8, // electroglyph uint8 calibrated
        // ── Octen-Embedding-0.6B (local exports) ─────────────────────────────
        EmbedderModel::Octen06bInt8FullLocal, // int8 MatMul+Gather (~570 MB)
        EmbedderModel::Octen06bInt4Local,     // int4 MatMul-only (~900 MB)
        EmbedderModel::Octen06bInt8Local,     // int8 MatMul-only (~1.1 GB)
        EmbedderModel::Octen06bFp32,          // fp32 reference (2.4 GB)
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
        .join("Library/Application Support/com.christianstrobele.crispsorter/models");
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
    let pipeline = IngestPipeline::new(
        fts.clone(),
        local.clone(),
        embedder_arc.clone(),
        IngestConfig::default(),
    );

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
    println!(
        "Ingest Speed: {:.2} chunks/sec",
        total_chunks as f64 / ingest_duration.as_secs_f64()
    );

    // 3. Search Latency
    let queries = vec!["Rahner", "Recht", "integration", "theology", "Druck"];
    let start_hybrid = Instant::now();
    for q in &queries {
        let _ = engine
            .search_hybrid(q, &SearchFilters::default(), 10)
            .await?;
    }
    println!(
        "Avg Hybrid Latency: {:?}",
        start_hybrid.elapsed() / queries.len() as u32
    );

    // 4. Accuracy (0.0 – 1.0) — check top-1 result for each test query.
    let mut score = 0.0f64;

    let res1 = engine
        .search_hybrid("Recht unter Druck", &SearchFilters::default(), 1)
        .await?;
    if res1.first().map(|r| r.title.as_deref().unwrap_or("")) == Some("Recht unter Druck") {
        score += 0.5;
    }
    let res2 = engine
        .search_hybrid("Integrationsdialog Heitmeyer", &SearchFilters::default(), 1)
        .await?;
    if res2.first().map(|r| r.title.as_deref().unwrap_or("")) == Some("Integration - Dialog") {
        score += 0.5;
    }
    println!("Accuracy Score: {:.2}", score);

    // 5. Memory Usage
    {
        use sysinfo::{RefreshKind, System};
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

// ── Embedding quality metrics ───────────────────────────────────────────────
//
// Measures quantization degradation (cosine drift vs FP32 baseline),
// semantic ordering, triplet margin, anisotropy, and unit-norm compliance
// for the four local Octen export variants.
//
// Run with: cargo test embedding_quality_metrics -- --nocapture
//
// Note: FP32 baseline must be present (~2.4 GB).  If missing, the test is
// skipped.  All variants share the same tokenizer from the int8 directory.
#[tokio::test]
async fn embedding_quality_metrics() {
    let corpus: Vec<(&str, &str)> = vec![
        // (label, text)
        ("ai_safety",   "Constitutional AI: Harmlessness from AI Feedback. RLHF safety alignment."),
        ("ai_rlhf",     "Reinforcement learning from human feedback improves model safety."),
        ("law_german",  "Während Angriffe auf den Rechtsstaat in Europa zunehmen, fordert das Grundgesetz besondere Schutzmaßnahmen."),
        ("law_german2", "Die Demokratie ist eine staatliche Ordnung, die das Recht auf freie Meinungsäußerung garantiert."),
        ("integration", "Integrationsdialog zwischen Islam und Christentum: Zeithistorische Perspektiven auf soziale Kohäsion."),
        ("theology",    "Karl Rahner and Hans Urs von Balthasar debated the nature of grace, salvation, and eschatology."),
        ("cooking",     "To make a sourdough starter, mix flour and water in equal parts and feed daily."),
        ("sports",      "The football team won the championship after a penalty shootout in the final match."),
    ];
    let texts: Vec<String> = corpus.iter().map(|(_, t)| t.to_string()).collect();

    // Semantic pairs: (anchor_idx, positive_idx, negative_idx)
    // anchor=ai_safety, positive=ai_rlhf (same topic), negative=cooking (unrelated)
    let triplets: &[(usize, usize, usize)] = &[
        (0, 1, 6), // ai_safety vs ai_rlhf vs cooking
        (2, 3, 7), // law_german vs law_german2 vs sports
        (4, 5, 6), // integration vs theology vs cooking
    ];

    let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/com.christianstrobele.crispsorter/models");

    // All Octen variants now support dynamic batch (INT4 batch=1 causal-mask was
    // fixed by patching dim_value=1 → dim_param='batch' in the ONNX value_info proto).
    let cfg_for = |model| EmbedderConfig::new(model, EmbedderDevice::Cpu, models_dir.clone());

    // Attempt to load FP32 baseline for cosine drift comparison (optional).
    let fp32_vecs: Option<Vec<Vec<f32>>> = {
        let cfg = cfg_for(EmbedderModel::Octen06bFp32);
        match Embedder::new(cfg).await {
            Ok(mut e) => match e.embed_dense(texts.clone()) {
                Ok(d) => {
                    println!("FP32 baseline loaded.");
                    Some(d.vectors)
                }
                Err(err) => {
                    println!("FP32 embed failed (drift skipped): {err:#}");
                    None
                }
            },
            Err(err) => {
                println!("FP32 load failed (drift skipped): {err:#}");
                None
            }
        }
    };

    let quantized_variants = vec![
        EmbedderModel::Octen06bInt8Local,
        EmbedderModel::Octen06bInt8FullLocal,
        EmbedderModel::Octen06bInt4Local,
    ];

    for model in quantized_variants {
        println!("\n=== Quality Metrics: {:?} ===", model);

        let cfg = cfg_for(model);
        let vecs: Vec<Vec<f32>> = match Embedder::new(cfg).await {
            Ok(mut e) => match e.embed_dense(texts.clone()) {
                Ok(d) => d.vectors,
                Err(err) => {
                    println!("SKIPPED — embed failed: {err:#}");
                    continue;
                }
            },
            Err(err) => {
                println!("SKIPPED — load failed: {err:#}");
                continue;
            }
        };

        // 1. Cosine drift vs FP32 (mean and min) — only when baseline loaded
        if let Some(ref fp32_vecs) = fp32_vecs {
            let drifts: Vec<f32> = vecs
                .iter()
                .zip(fp32_vecs)
                .map(|(q, fp)| cosine(q, fp))
                .collect();
            let mean_drift: f32 = drifts.iter().sum::<f32>() / drifts.len() as f32;
            let min_drift: f32 = drifts.iter().cloned().fold(f32::MAX, f32::min);
            println!(
                "  Cosine drift vs FP32 — mean: {:.4}  min: {:.4}  (1.0 = identical)",
                mean_drift, min_drift
            );
        } else {
            println!("  Cosine drift vs FP32 — n/a (FP32 model unavailable)");
        }

        // 2. Unit-norm compliance
        let norms: Vec<f32> = vecs.iter().map(|v| l2_norm(v)).collect();
        let max_norm_err = norms
            .iter()
            .map(|n| (n - 1.0f32).abs())
            .fold(0.0f32, f32::max);
        println!(
            "  Unit-norm max deviation: {:.6}  (< 1e-4 = compliant)",
            max_norm_err
        );

        // 3. Semantic ordering: related pair cosine > unrelated pair cosine
        let mut ordering_pass = 0usize;
        for &(a, p, n) in triplets {
            let sim_pos = cosine(&vecs[a], &vecs[p]);
            let sim_neg = cosine(&vecs[a], &vecs[n]);
            if sim_pos > sim_neg {
                ordering_pass += 1;
            }
            println!(
                "    triplet ({},{},{}) pos={:.4} neg={:.4} {}",
                a,
                p,
                n,
                sim_pos,
                sim_neg,
                if sim_pos > sim_neg { "✓" } else { "✗" }
            );
        }
        println!(
            "  Semantic ordering: {}/{} passed",
            ordering_pass,
            triplets.len()
        );

        // 4. Triplet margin: sim(a,p) - sim(a,n) for each triplet
        let margins: Vec<f32> = triplets
            .iter()
            .map(|&(a, p, n)| cosine(&vecs[a], &vecs[p]) - cosine(&vecs[a], &vecs[n]))
            .collect();
        let mean_margin: f32 = margins.iter().sum::<f32>() / margins.len() as f32;
        let min_margin: f32 = margins.iter().cloned().fold(f32::MAX, f32::min);
        println!(
            "  Triplet margin — mean: {:.4}  min: {:.4}  (> 0 = correct ordering)",
            mean_margin, min_margin
        );

        // 5. Anisotropy: average pairwise cosine over all unique pairs
        //    Low anisotropy (near 0) means embeddings spread uniformly over the sphere.
        let n = vecs.len();
        let mut pair_sum = 0.0f32;
        let mut pair_count = 0usize;
        for i in 0..n {
            for j in (i + 1)..n {
                pair_sum += cosine(&vecs[i], &vecs[j]);
                pair_count += 1;
            }
        }
        let anisotropy = pair_sum / pair_count as f32;
        println!(
            "  Anisotropy (avg pairwise cos): {:.4}  (< 0.3 = well-spread space)",
            anisotropy
        );
    }
}
