//! Opt-in measurement of the real production index against externally judged
//! documents and real, hash-checked model embeddings. No model stub is called.
//! Run with the preparation/runner scripts in tools/quality.
use anyhow::{bail, ensure, Context, Result};
use crisp_index_protocol::{IngestBatch, IngestChunk, SearchFilters, SearchHit};
use crisp_search_query::query_plan::{prepare_query, QueryIntent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashSet}, io::Write, path::Path, sync::Arc, time::Instant};

const DIMS: usize = 384;
const OWNER: &str = "judged-public-retrieval";
const LIMIT: usize = 100;

#[derive(Deserialize)]
struct Document { id: String, title: String, text: String }
#[derive(Deserialize)]
struct Query { id: String, text: String, relevance: BTreeMap<String, i32> }
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode { LexicalStrict, LexicalAnyTerm, Dense, Hybrid, NaturalLexical, NaturalHybrid }
const MODES: [Mode; 6] = [Mode::LexicalStrict, Mode::LexicalAnyTerm, Mode::Dense, Mode::Hybrid,
    Mode::NaturalLexical, Mode::NaturalHybrid];

fn file_hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn checked_file(input: &Path, manifest: &Value, name: &str) -> Result<Vec<u8>> {
    let bytes = std::fs::read(input.join(name)).with_context(|| format!("Reading {name}"))?;
    ensure!(manifest["preparedFiles"][name]["sha256"].as_str() == Some(file_hash(&bytes).as_str()), "Prepared {name} hash mismatch");
    ensure!(manifest["preparedFiles"][name]["bytes"].as_u64() == Some(bytes.len() as u64), "Prepared {name} length mismatch");
    Ok(bytes)
}
fn embeddings(bytes: &[u8], rows: usize) -> Result<Vec<Vec<f32>>> {
    ensure!(bytes.len() == rows.checked_mul(DIMS * 4).context("Embedding length overflow")?, "Embedding rows/dimensions mismatch");
    bytes.chunks_exact(DIMS * 4).map(|row| {
        let values: Vec<f32> = row.chunks_exact(4).map(|value| f32::from_le_bytes(value.try_into().unwrap())).collect();
        let norm: f64 = values.iter().map(|value| f64::from(*value).powi(2)).sum();
        ensure!(values.iter().all(|value| value.is_finite()) && (norm - 1.0).abs() < 0.001, "Expected finite, normalized real embeddings");
        Ok(values)
    }).collect()
}
fn any_term_query(text: &str) -> Result<String> {
    let terms: std::collections::BTreeSet<_> = crisp_search_query::simple_tokenize(text).into_iter().collect();
    ensure!(!terms.is_empty(), "Query contains no indexed terms");
    // Each normalized token is quoted so language words such as AND remain
    // literal. This separately labelled comparison does not change the UI mode.
    Ok(terms.into_iter().map(|term| format!("\"{term}\"")).collect::<Vec<_>>().join(" OR "))
}
async fn search(index: &crate::index::SearchIndex, mode: Mode, query: &Query, embedding: &[f32], filters: &SearchFilters) -> Result<Vec<SearchHit>> {
    match mode {
        Mode::LexicalStrict => index.search_text(&query.text, filters, LIMIT).await,
        Mode::LexicalAnyTerm => index.search_text(&any_term_query(&query.text)?, filters, LIMIT).await,
        Mode::Dense => index.search_vector(embedding, filters, LIMIT).await,
        Mode::Hybrid => index.search_hybrid(&query.text, embedding, filters, LIMIT).await,
        Mode::NaturalLexical => {
            let plan = prepare_query(&query.text, QueryIntent::Natural, false, false)?;
            index.search_text(&plan.lexical_query, filters, LIMIT).await
        }
        Mode::NaturalHybrid => {
            let plan = prepare_query(&query.text, QueryIntent::Natural, false, false)?;
            index.search_hybrid_with_plan(&plan, embedding, filters, LIMIT).await
        }
    }
}
#[derive(Default, Clone, Copy, Serialize)]
struct Metrics { ndcg_at_10: f64, reciprocal_rank_at_10: f64, recall_at_100: f64, average_precision_at_100: f64 }
fn metrics(ids: &[String], relevance: &BTreeMap<String, i32>) -> Metrics {
    let mut ideal: Vec<_> = relevance.values().copied().filter(|gain| *gain > 0).collect();
    ideal.sort_unstable_by(|left, right| right.cmp(left));
    let discounted = |rank: usize, gain: i32| (2f64.powi(gain) - 1.0) / ((rank + 2) as f64).log2();
    let idcg: f64 = ideal.iter().take(10).enumerate().map(|(rank, gain)| discounted(rank, *gain)).sum();
    let mut result = Metrics::default();
    let mut found = 0usize;
    for (rank, id) in ids.iter().take(LIMIT).enumerate() {
        let gain = *relevance.get(id).unwrap_or(&0);
        if rank < 10 {
            result.ndcg_at_10 += discounted(rank, gain) / idcg;
            if gain > 0 && result.reciprocal_rank_at_10 == 0.0 { result.reciprocal_rank_at_10 = 1.0 / (rank + 1) as f64; }
        }
        if gain > 0 { found += 1; result.average_precision_at_100 += found as f64 / (rank + 1) as f64; }
    }
    result.recall_at_100 = found as f64 / ideal.len() as f64;
    result.average_precision_at_100 /= ideal.len() as f64;
    result
}
fn percentile(values: &[f64], fraction: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1).min(sorted.len() - 1)]
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().context("Output parent missing")?)?;
    serde_json::to_writer_pretty(&mut staged, value)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[test]
fn judged_metrics_count_misses_and_use_graded_relevance() {
    let truth = [("a".into(), 2), ("b".into(), 1)].into_iter().collect();
    let perfect = metrics(&["a".into(), "b".into()], &truth);
    assert!((perfect.ndcg_at_10 - 1.0).abs() < 1e-9);
    assert_eq!(perfect.reciprocal_rank_at_10, 1.0);
    assert_eq!(perfect.average_precision_at_100, 1.0);
    let missed = metrics(&["missing".into(), "b".into()], &truth);
    assert_eq!(missed.recall_at_100, 0.5);
    assert_eq!(missed.reciprocal_rank_at_10, 0.5);
    assert_eq!(missed.average_precision_at_100, 0.25);
    assert_eq!(metrics(&[], &truth).ndcg_at_10, 0.0);
    assert_eq!(any_term_query("AND alpha alpha").unwrap(), "\"alpha\" OR \"and\"");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires hash-checked real model vectors and an external judged corpus"]
async fn native_judged_retrieval_benchmark() -> Result<()> {
    let input = std::path::PathBuf::from(std::env::var("CRISP_QUALITY_INPUT").context("Set CRISP_QUALITY_INPUT to prepared real-model data")?);
    let output = std::path::PathBuf::from(std::env::var("CRISP_QUALITY_OUTPUT").context("Set CRISP_QUALITY_OUTPUT to a result directory")?);
    let revision = std::env::var("CRISP_QUALITY_REVISION").context("Set CRISP_QUALITY_REVISION to the tested Git revision")?;
    ensure!(revision.len() == 40 && revision.chars().all(|character| character.is_ascii_hexdigit()), "Expected a full revision SHA");
    let manifest: Value = serde_json::from_slice(&std::fs::read(input.join("preparation.json"))?)?;
    ensure!(manifest["completed"] == true && manifest["model"]["dimensions"] == DIMS, "Incomplete preparation or model dimension mismatch");
    let documents: Vec<Document> = serde_json::from_slice(&checked_file(&input, &manifest, "documents.json")?)?;
    let queries: Vec<Query> = serde_json::from_slice(&checked_file(&input, &manifest, "queries.json")?)?;
    ensure!(!documents.is_empty() && !queries.is_empty(), "Empty evaluation input");
    let document_vectors = embeddings(&checked_file(&input, &manifest, "documents.f32")?, documents.len())?;
    let query_vectors = embeddings(&checked_file(&input, &manifest, "queries.f32")?, queries.len())?;
    let identities: HashSet<_> = documents.iter().map(|document| document.id.clone()).collect();
    ensure!(identities.len() == documents.len() && !identities.contains(""), "Duplicate or empty document identity");
    let query_ids: HashSet<_> = queries.iter().map(|query| query.id.as_str()).collect();
    ensure!(query_ids.len() == queries.len() && !query_ids.contains(""), "Duplicate or empty query identity");
    for query in &queries {
        ensure!(query.relevance.iter().all(|(id, gain)| identities.contains(id) && (0..=30).contains(gain)), "Invalid judgment for {}", query.id);
        ensure!(query.relevance.values().any(|gain| *gain > 0), "No positive judgment for {}", query.id);
    }
    std::fs::create_dir_all(&output)?;
    let mut result = json!({"schema":"crispsorter-native-retrieval-result-v1", "completed":false,
        "revision":revision, "preparation":manifest, "documents":documents.len(), "queries":queries.len(),
        "cutoff":LIMIT, "modes":[], "scope":"Production server index, CPU model inference prepared separately. Native retrieval latency excludes embedding, HTTP, and UI. Public-corpus judgments are not a user-corpus or full-desktop qualification.",
        "policy":"All test queries count in every mean; failed and zero-result queries score zero. One warm run per mode/query after ten unmeasured warmup queries. No test-judgment tuning or query removal.",
        "lexical_any_term":"Explicit comparison using normalized literal OR terms; separate from the shipped strict lexical query mode.",
        "natural_language":"Production Natural query intent with fuzzy and synonym expansion disabled. Original query text supplies the frozen embedding; generated lexical terms do not become mandatory constraints on hybrid retrieval."});
    write_json(&output.join("result.json"), &result)?;
    let directory = tempfile::tempdir()?;
    let vector = Arc::new(crate::index::VectorStore::open_or_create(directory.path(), DIMS).await?);
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&directory.path().join("fts"))?);
    let index = crate::index::SearchIndex::new(vector, fts);
    index.ensure_ready().await?;
    let started = Instant::now();
    for (batch_index, batch) in documents.chunks(256).enumerate() {
        let chunks = batch.iter().enumerate().map(|(offset, document)| IngestChunk {
            doc_id: document.id.clone(), owner_id: OWNER.into(), source_hash: file_hash(document.text.as_bytes()),
            chunk_index: 0, chunk_total: Some(1), chunk_start_char: Some(0), chunk_end_char: Some(document.text.len() as i32),
            full_text: document.text.clone(), title: Some(document.title.clone()), filename: format!("{}.txt", document.id),
            ext: "txt".into(), language: "en".into(), location_uri: format!("benchmark://{OWNER}/{}", document.id),
            embedding: document_vectors[batch_index * 256 + offset].clone(), ..Default::default()
        }).collect();
        index.ingest_batch(&IngestBatch { chunks, container_group_version: None }).await?;
        eprintln!("Indexed {} / {} public documents", ((batch_index + 1) * 256).min(documents.len()), documents.len());
    }
    let ingestion_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (chunk_count, document_count) = index.stats().await?;
    ensure!(chunk_count == documents.len() && document_count == documents.len(), "Native index coverage differs from the complete corpus");
    result["ingestion_ms"] = json!(ingestion_ms);
    result["vector_index"] = json!("Exact vector scan; no approximate vector index is built for this corpus");
    let filters = SearchFilters { owner_id: Some(OWNER.into()), ..Default::default() };
    let mut trace = std::fs::File::create(output.join("queries.jsonl"))?;
    for mode in MODES {
        for (query, embedding) in queries.iter().zip(&query_vectors).take(10) {
            let _ = search(&index, mode, query, embedding, &filters).await;
        }
        let mut aggregate = Metrics::default();
        let mut latencies = Vec::new();
        let mut errors = Vec::new();
        let mut zero_results = 0usize;
        for (position, (query, embedding)) in queries.iter().zip(&query_vectors).enumerate() {
            let begin = Instant::now();
            let found = search(&index, mode, query, embedding, &filters).await;
            let elapsed = begin.elapsed().as_secs_f64() * 1000.0;
            latencies.push(elapsed);
            let (ids, scores, error) = match found {
                Ok(hits) => {
                    let mut seen = HashSet::new();
                    if hits.iter().any(|hit| hit.owner_id != OWNER || !identities.contains(&hit.doc_id) || !hit.score.is_finite() || !seen.insert(hit.doc_id.clone())) {
                        bail!("Index returned invalid identity, score, or duplicate for {}", query.id);
                    }
                    (hits.iter().map(|hit| hit.doc_id.clone()).collect::<Vec<_>>(), hits.iter().map(|hit| hit.score).collect::<Vec<_>>(), None)
                }
                Err(error) => {
                    let description = format!("{error:#}");
                    errors.push(json!({"query_id":query.id,"error":description}));
                    (Vec::new(), Vec::new(), Some(description))
                }
            };
            if ids.is_empty() { zero_results += 1; }
            let score = metrics(&ids, &query.relevance);
            aggregate.ndcg_at_10 += score.ndcg_at_10;
            aggregate.reciprocal_rank_at_10 += score.reciprocal_rank_at_10;
            aggregate.recall_at_100 += score.recall_at_100;
            aggregate.average_precision_at_100 += score.average_precision_at_100;
            serde_json::to_writer(&mut trace, &json!({"mode":mode,"query_id":query.id,"latency_ms":elapsed,"metrics":score,"document_ids":ids,"scores":scores,"error":error}))?;
            trace.write_all(b"\n")?;
            if (position + 1) % 50 == 0 { eprintln!("{mode:?}: {} / {} judged queries", position + 1, queries.len()); }
        }
        for value in [&mut aggregate.ndcg_at_10, &mut aggregate.reciprocal_rank_at_10, &mut aggregate.recall_at_100, &mut aggregate.average_precision_at_100] { *value /= queries.len() as f64; }
        let summary = json!({"mode":mode,"queries":queries.len(),"metrics":aggregate,"zero_results":zero_results,"failed_queries":errors,
            "latency_ms":{"mean":latencies.iter().sum::<f64>() / latencies.len() as f64,"p50":percentile(&latencies,0.5),"p95":percentile(&latencies,0.95),"p99":percentile(&latencies,0.99)}});
        eprintln!("{}", serde_json::to_string(&summary)?);
        result["modes"].as_array_mut().unwrap().push(summary);
        write_json(&output.join("result.json"), &result)?;
    }
    trace.sync_all()?;
    result["completed"] = json!(true);
    result["all_queries_succeeded"] = json!(result["modes"].as_array().unwrap().iter().all(|mode| mode["failed_queries"].as_array().unwrap().is_empty()));
    result["query_trace_sha256"] = json!(file_hash(&std::fs::read(output.join("queries.jsonl"))?));
    write_json(&output.join("result.json"), &result)?;
    Ok(())
}
