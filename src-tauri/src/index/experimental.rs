//! Explicit opt-in HTTP adapter for newer CrispEmbed research capabilities.
//! This does not change the application's immutable native runtime.
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;
use crate::AppState;
use super::{ingest::chunk_row_id, SearchResult};

fn client(endpoint: &str) -> Result<(reqwest::Client, String), String> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| "Enter a valid CrispEmbed server URL")?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("Use HTTPS, or HTTP on localhost, without credentials, query or fragment".into());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none())
        .build().map_err(|e| e.to_string())?;
    Ok((client, endpoint.trim_end_matches('/').to_owned()))
}

async fn response_json(mut response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 { return Err("Server response exceeds 4 MiB".into()); }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() { return Err(format!("CrispEmbed server returned HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(512)]))); }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid CrispEmbed response: {e}"))
}

#[tauri::command]
pub async fn crispembed_probe(endpoint: String) -> Result<Value, String> {
    let (client, base) = client(&endpoint)?;
    let report = response_json(client.get(format!("{base}/capabilities")).send().await.map_err(|e| e.to_string())?).await?;
    if !report.get("routes").is_some_and(Value::is_object) { return Err("Server does not expose current CrispEmbed capability discovery".into()); }
    Ok(report)
}

fn validate_vectors(value: &Value, role: &str) -> Result<Vec<Vec<f32>>, String> {
    if value.get("role").and_then(Value::as_str) != Some(role) { return Err("MoEdern returned the wrong query/document role".into()); }
    let ids = value.get("ids").and_then(Value::as_array).ok_or("Missing MoEdern token IDs")?;
    if ids.is_empty() || ids.len() > 512 { return Err("Invalid MoEdern token count".into()); }
    for name in ["supplied_mask", "effective_mask", "scoring_mask"] {
        let mask = value.get(name).and_then(Value::as_array).ok_or("Missing MoEdern role mask")?;
        if mask.len() != ids.len() || mask.iter().any(|v| !matches!(v.as_u64(), Some(0 | 1))) {
            return Err(format!("Invalid MoEdern {name}"));
        }
    }
    let selected = value["scoring_mask"].as_array().unwrap().iter().filter(|v| v.as_u64() == Some(1)).count();
    let flat = value.get("selected_vectors").and_then(Value::as_array).ok_or("Missing selected MoEdern vectors")?;
    if selected == 0 || flat.len() != selected * 128 { return Err("MoEdern scoring vectors disagree with the scoring mask".into()); }
    let vectors: Result<Vec<f32>, String> = flat.iter().map(|v| {
        let v = v.as_f64().ok_or("Non-numeric MoEdern coordinate")? as f32;
        if v.is_finite() { Ok(v) } else { Err("Non-finite MoEdern coordinate".into()) }
    }).collect();
    Ok(vectors?.chunks_exact(128).map(|v| v.to_vec()).collect())
}

async fn encode(client: &reqwest::Client, base: &str, text: &str, role: &str) -> Result<Vec<Vec<f32>>, String> {
    if text.len() > 65536 || text.contains('\0') { return Err("MoEdern text must fit within 64 KiB without NUL characters".into()); }
    let value = response_json(client.post(format!("{base}/moedern/encode"))
        .json(&json!({"text": text, "role": role})).send().await.map_err(|e| e.to_string())?).await?;
    validate_vectors(&value, role)
}

#[derive(Debug, Serialize)]
pub struct RerankReport {
    pub results: Vec<SearchResult>,
    pub elapsed_ms: u128,
    pub document_vector_bytes: usize,
    pub experimental: bool,
}

#[tauri::command]
pub async fn crispembed_rerank_passages(
    state: State<'_, AppState>, endpoint: String, query: String,
    candidates: Vec<SearchResult>, limit: usize,
) -> Result<RerankReport, String> {
    if candidates.len() > 100 || query.trim().is_empty() { return Err("Supply a question and at most 100 candidates".into()); }
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let (client, base) = client(&endpoint)?;
    let local = state.index.lock().await.local.clone().ok_or("A local index is required for passage reranking")?;
    let passages = local.fetch_passages(&candidates).await.map_err(|e| e.to_string())?;
    let started = Instant::now();
    let query_vectors = encode(&client, &base, &query, "query").await?;
    let mut scored = Vec::new();
    let mut fallback = Vec::new();
    let mut bytes = 0;
    for mut candidate in candidates {
        let key = chunk_row_id(&candidate.doc_id, candidate.chunk_index);
        let Some(passage) = passages.get(&key) else { fallback.push(candidate); continue; };
        let vectors = encode(&client, &base, &passage.text, "document").await?;
        bytes += vectors.len() * 128 * std::mem::size_of::<f32>();
        candidate.score = super::search::maxsim(&query_vectors, &vectors);
        if !candidate.score.is_finite() { return Err("MoEdern returned an invalid similarity score".into()); }
        scored.push(candidate);
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.extend(fallback);
    scored.truncate(limit.min(100));
    Ok(RerankReport { results: scored, elapsed_ms: started.elapsed().as_millis(), document_vector_bytes: bytes, experimental: true })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionTask { Category, Collection, NeedsReview }

#[derive(Debug, Serialize)]
pub struct DecisionSuggestion {
    pub value: String,
    pub confidence: f64,
    pub model: String,
    pub input_excerpt: String,
    pub experimental: bool,
}

fn decision_request(text: &str, task: &DecisionTask, choices: &[String]) -> Result<Value, String> {
    if text.is_empty() || text.len() > 8192 { return Err("Decision evidence must contain 1–8192 bytes".into()); }
    let question = match task {
        DecisionTask::NeedsReview => json!({"type":"noul", "instructions":"Does this document have missing, inconsistent, or ambiguous metadata that requires human review?"}),
        DecisionTask::Category | DecisionTask::Collection => {
            if !(2..=12).contains(&choices.len()) || choices.iter().any(|c| c.trim().is_empty() || c.len() > 120) {
                return Err("Supply 2–12 distinct choices, at most 120 bytes each".into());
            }
            let unique: std::collections::HashSet<_> = choices.iter().collect();
            if unique.len() != choices.len() { return Err("Decision choices must be distinct".into()); }
            let criteria: serde_json::Map<String, Value> = choices.iter().enumerate()
                .map(|(i, choice)| (format!("c{i:02}"), Value::String(choice.clone()))).collect();
            json!({"type":"choice", "instructions": match task { DecisionTask::Category => "Choose the document type best supported by this evidence.", _ => "Choose the collection best supported by this evidence." }, "criteria":criteria})
        }
    };
    Ok(json!({"state": text, "questions":{"suggestion":question}}))
}

#[tauri::command]
pub async fn crispembed_classify_document(
    state: State<'_, AppState>, endpoint: String, document: SearchResult,
    task: DecisionTask, choices: Vec<String>,
) -> Result<DecisionSuggestion, String> {
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let local = state.index.lock().await.local.clone().ok_or("A local index is required for document decisions")?;
    let mut passages = local.fetch_passages(std::slice::from_ref(&document)).await.map_err(|e| e.to_string())?;
    let passage = passages.remove(&chunk_row_id(&document.doc_id, document.chunk_index)).ok_or("Process the document text before classifying it")?;
    // The upstream profile clips state to 512 model tokens; show exactly the
    // submitted excerpt and do not imply the whole document was evaluated.
    let excerpt: String = passage.text.chars().take(1600).collect();
    let request = decision_request(&excerpt, &task, &choices)?;
    let (client, base) = client(&endpoint)?;
    let result = response_json(client.post(format!("{base}/systemone")).json(&request)
        .send().await.map_err(|e| e.to_string())?).await?;
    let answer = &result["answers"]["suggestion"];
    let (value, confidence) = match task {
        DecisionTask::NeedsReview => {
            let probability = answer["noul"].as_f64().filter(|v| v.is_finite() && (0.0..=1.0).contains(v)).ok_or("Invalid SystemOne boolean score")?;
            (if probability >= 0.5 { "needs_review" } else { "no_review_suggested" }.into(), probability)
        }
        _ => {
            let id = answer["choice"].as_str().ok_or("Missing SystemOne choice")?;
            let index = id.strip_prefix('c').and_then(|i| i.parse::<usize>().ok()).filter(|&i| i < choices.len()).ok_or("SystemOne chose an unknown option")?;
            let confidence = answer["confidence"].as_f64().filter(|v| v.is_finite() && (0.0..=1.0).contains(v)).ok_or("Invalid SystemOne model score")?;
            (choices[index].clone(), confidence)
        }
    };
    Ok(DecisionSuggestion { value, confidence, model: result["model"].as_str().unwrap_or("SystemOne (server model identity unavailable)").into(), input_excerpt: excerpt, experimental: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_insecure_remote_or_credential_bearing_endpoints() {
        assert!(client("http://example.org").is_err());
        assert!(client("https://user:secret@example.org").is_err());
        assert!(client("http://127.0.0.1:8080").is_ok());
    }
    #[test]
    fn roles_and_masks_are_required_before_scoring() {
        let value = json!({"role":"document","ids":[1],"supplied_mask":[1],"effective_mask":[1],"scoring_mask":[1],"selected_vectors":vec![0.0;128]});
        assert!(validate_vectors(&value, "query").is_err());
        assert_eq!(validate_vectors(&value, "document").unwrap().len(), 1);
        let mut wrong = value;
        wrong["scoring_mask"] = json!([0]);
        assert!(validate_vectors(&wrong, "document").is_err());
    }
    #[test]
    fn bounded_decisions_preserve_options_and_reject_duplicates() {
        let request = decision_request("Invoice from Acme", &DecisionTask::Category, &["Invoice".into(), "Other".into()]).unwrap();
        assert_eq!(request["questions"]["suggestion"]["criteria"]["c00"], "Invoice");
        assert!(decision_request("text", &DecisionTask::Category, &["same".into(), "same".into()]).is_err());
    }
}
