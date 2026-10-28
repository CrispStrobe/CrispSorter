//! Structured extraction with inspectable source anchors and explicit review.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldEvidence {
    pub id: String,
    pub label: String,
    pub original_value: String,
    pub normalized_value: String,
    pub model: String,
    pub confidence: Option<f32>,
    pub start_char: Option<usize>,
    pub end_char: Option<usize>,
    pub page: Option<usize>,
    pub anchor_method: String,
    pub review_status: String,
    pub revision: u64,
}

pub fn from_extraction(doc_id: &str, text: &str, model: &str, fields: Vec<(String, String, f32)>) -> Vec<FieldEvidence> {
    let mut seen = std::collections::HashSet::new();
    fields.into_iter().filter_map(|(label, original, confidence)| {
        if original.trim().is_empty() || !confidence.is_finite() { return None; }
        let id = format!("{:x}", Sha256::digest(format!("{doc_id}\0{label}\0{original}").as_bytes()));
        if !seen.insert(id.clone()) { return None; }
        let at = text.find(&original);
        let start_char = at.map(|at| text[..at].chars().count());
        let end_char = start_char.map(|start| start + original.chars().count());
        let page = at.filter(|_| text.contains('\u{c}')).map(|at| text[..at].chars().filter(|&c| c == '\u{c}').count() + 1);
        Some(FieldEvidence {
            id, label: label.trim().to_lowercase(),
            normalized_value: original.split_whitespace().collect::<Vec<_>>().join(" "),
            original_value: original, model: model.into(), confidence: Some(confidence),
            start_char, end_char, page,
            anchor_method: if at.is_some() { "first_exact_match" } else { "unavailable" }.into(),
            review_status: "suggested".into(), revision: 0,
        })
    }).collect()
}

pub fn read(metadata: &serde_json::Value) -> Vec<FieldEvidence> {
    metadata.get("field_evidence").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// Keep human decisions when the same source is extracted again, while using
/// the current extraction's anchors, model identity and model score.
pub fn preserve_reviews(current: &mut [FieldEvidence], previous: &[FieldEvidence]) {
    for field in current {
        if let Some(prior) = previous.iter().find(|p| p.id == field.id && p.review_status != "suggested") {
            field.normalized_value = prior.normalized_value.clone();
            field.review_status = prior.review_status.clone();
            field.revision = prior.revision;
        }
    }
}

pub fn tag(field: &FieldEvidence) -> String {
    format!("field:{}:{}", field.label, field.normalized_value)
}

#[tauri::command]
pub async fn verify_sort_field_source(path: String, expected_hash: String) -> Result<(), String> {
    if expected_hash.len() != 64 || !expected_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Reindex this document before using its fields for sorting".into());
    }
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let length = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if length == 0 { break; }
            hasher.update(&buffer[..length]);
        }
        if format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(&expected_hash) { Ok(()) }
        else { Err("The file changed since these fields were extracted. Reindex and review its current contents before sorting.".into()) }
    }).await.map_err(|e| e.to_string())?
}

pub fn attach(metadata: &mut Option<String>, fields: &[FieldEvidence]) {
    let mut value: serde_json::Value = metadata.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| serde_json::json!({}));
    value["field_evidence"] = serde_json::json!(fields);
    *metadata = Some(value.to_string());
}

#[tauri::command]
pub async fn index_get_fields(state: State<'_, AppState>, doc_id: String) -> Result<Vec<FieldEvidence>, String> {
    let local = state.index.lock().await.local.clone().ok_or("Local index is not ready")?;
    let metadata = local.document_metadata(&doc_id).await.map_err(|e| e.to_string())?;
    Ok(read(&metadata))
}

#[tauri::command]
pub async fn index_review_field(
    state: State<'_, AppState>, doc_id: String, field_id: String,
    normalized_value: String, review_status: String, expected_revision: u64,
) -> Result<Vec<FieldEvidence>, String> {
    if !matches!(review_status.as_str(), "accepted" | "corrected" | "rejected" | "suggested") {
        return Err("Invalid field review status".into());
    }
    if normalized_value.len() > 4096 || normalized_value.trim().is_empty() {
        return Err("A field value must contain 1–4096 bytes".into());
    }
    let lock = state.index.lock().await;
    let local = lock.local.clone().ok_or("Local index is not ready")?;
    let metadata = local.document_metadata(&doc_id).await.map_err(|e| e.to_string())?;
    let mut fields = read(&metadata);
    let field = fields.iter_mut().find(|f| f.id == field_id).ok_or("Field is no longer present; reload the document")?;
    if field.revision != expected_revision { return Err("This field changed elsewhere; reload before editing".into()); }
    field.normalized_value = normalized_value.trim().into();
    field.review_status = review_status;
    field.revision += 1;
    let mut updated = metadata.clone();
    updated["field_evidence"] = serde_json::json!(fields);
    local.update_field_evidence(&doc_id, &metadata, &updated).await.map_err(|e| e.to_string())?;
    super::result_cache::invalidate();
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_retain_original_text_and_unicode_source_offsets() {
        let fields = from_extraction("doc", "Ä intro\u{c}Total: €  42,50", "model", vec![("total".into(), "€  42,50".into(), 0.9)]);
        assert_eq!(fields[0].page, Some(2));
        assert_eq!(fields[0].original_value, "€  42,50");
        assert_eq!(fields[0].normalized_value, "€ 42,50");
        assert_eq!(fields[0].start_char, Some(15));
        assert_eq!(fields[0].review_status, "suggested");
    }
    #[test]
    fn absent_anchors_and_confidence_are_not_invented() {
        let fields = from_extraction("doc", "plain text", "model", vec![("vendor".into(), "Acme".into(), 0.8), ("bad".into(), "bad".into(), f32::NAN)]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].page, None);
        assert_eq!(fields[0].start_char, None);
        assert_eq!(fields[0].anchor_method, "unavailable");
    }
    #[test]
    fn reindex_preserves_review_without_restoring_stale_source_anchors() {
        let mut previous = from_extraction("same-file", "Vendor: Acme", "model-v1", vec![("vendor".into(), "Acme".into(), 0.7)]);
        previous[0].normalized_value = "ACME GmbH".into();
        previous[0].review_status = "corrected".into();
        previous[0].revision = 3;
        let mut current = from_extraction("same-file", "Introduction\u{c}Vendor: Acme", "model-v2", vec![("vendor".into(), "Acme".into(), 0.9)]);
        let current_anchor = current[0].start_char;
        assert_ne!(current_anchor, previous[0].start_char);
        preserve_reviews(&mut current, &previous);
        assert_eq!(current[0].normalized_value, "ACME GmbH");
        assert_eq!(current[0].review_status, "corrected");
        assert_eq!(current[0].revision, 3);
        assert_eq!(current[0].start_char, current_anchor);
        assert_eq!(current[0].page, Some(2));
        assert_eq!(current[0].model, "model-v2");
        assert_eq!(current[0].confidence, Some(0.9));
    }
}
