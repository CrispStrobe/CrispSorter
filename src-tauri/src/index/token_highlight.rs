//! Token-level match highlighting via contextual embeddings.
//!
//! Encodes both query and document text through the dense embedder's
//! `encode_tokens` API (per-subword contextual embeddings), computes
//! cosine similarity between every query token and every document token,
//! and marks document tokens whose max-similarity exceeds a threshold.
//!
//! The output is a list of `(offset, length, score)` character spans in
//! the original document text — the frontend can wrap these in `<mark>`
//! tags for sub-sentence precision highlighting (finer than BM25 term
//! matching, which cannot handle synonyms or paraphrases).
//!
//! Requires the `crispembed` feature (GGUF backend).  Falls back to an
//! empty highlight set on the ONNX path.

use std::sync::Arc;
use tokio::sync::Mutex;

use super::embedder::Embedder;

/// A highlighted span in the document text.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenSpan {
    /// Byte offset in the document text where this token starts.
    pub offset: usize,
    /// Byte length of this token in the document text.
    pub length: usize,
    /// Max cosine similarity score between this document token and any
    /// query token.  Higher = more relevant.
    pub score: f32,
}

/// Cosine similarity between two vectors.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

/// Default similarity threshold for highlighting.
pub const DEFAULT_THRESHOLD: f32 = 0.65;

/// Compute token-level highlights for a document snippet given a query.
///
/// Returns spans in `doc_text` that are semantically similar to the query,
/// sorted by score descending.  Only spans with `max_sim >= threshold` are
/// returned.
///
/// This is a blocking call (holds the embedder lock twice).  Designed to
/// be called via `spawn_blocking` or similar.
pub async fn highlight_tokens(
    embedder: &Arc<Mutex<Embedder>>,
    query: &str,
    doc_text: &str,
    threshold: f32,
) -> Vec<TokenSpan> {
    if query.trim().is_empty() || doc_text.trim().is_empty() {
        return vec![];
    }

    // Truncate doc_text to avoid encoding huge texts (snippet is typically ~300 chars).
    let doc_truncated = if doc_text.len() > 2000 {
        &doc_text[..doc_text.floor_char_boundary(2000)]
    } else {
        doc_text
    };

    let query_tokens;
    let doc_tokens;

    {
        let mut emb = embedder.lock().await;
        query_tokens = emb.encode_tokens(query);
        doc_tokens = emb.encode_tokens(doc_truncated);
    }

    if query_tokens.is_empty() || doc_tokens.is_empty() {
        return vec![];
    }

    // For each document token, compute max cosine similarity with any query token.
    let mut spans: Vec<TokenSpan> = Vec::new();
    let mut byte_offset = 0usize;

    for (doc_tok_text, doc_tok_emb) in &doc_tokens {
        // Find this token's position in the remaining document text.
        let tok_start = if let Some(pos) = doc_truncated[byte_offset..].find(doc_tok_text.as_str()) {
            byte_offset + pos
        } else {
            // Subword tokens may have leading spaces or special chars stripped.
            // Use current offset as best guess.
            byte_offset
        };
        let tok_len = doc_tok_text.len();

        // Compute max similarity against all query tokens.
        let max_sim = query_tokens
            .iter()
            .map(|(_, q_emb)| cosine(q_emb, doc_tok_emb))
            .fold(0.0f32, f32::max);

        if max_sim >= threshold {
            spans.push(TokenSpan {
                offset: tok_start,
                length: tok_len,
                score: max_sim,
            });
        }

        // Advance byte_offset past this token.
        if tok_start + tok_len > byte_offset {
            byte_offset = tok_start + tok_len;
        }
    }

    spans.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    spans
}

/// Merge adjacent/overlapping spans into contiguous highlighted regions.
/// Input must be sorted by offset.
pub fn merge_spans(mut spans: Vec<TokenSpan>) -> Vec<TokenSpan> {
    if spans.is_empty() {
        return spans;
    }
    spans.sort_by_key(|s| s.offset);
    let mut merged: Vec<TokenSpan> = Vec::new();
    let mut current = spans[0].clone();

    for s in spans.into_iter().skip(1) {
        if s.offset <= current.offset + current.length {
            // Overlapping or adjacent — extend.
            let new_end = (s.offset + s.length).max(current.offset + current.length);
            current.length = new_end - current.offset;
            current.score = current.score.max(s.score);
        } else {
            merged.push(current);
            current = s;
        }
    }
    merged.push(current);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vecs() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vecs() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn merge_adjacent_spans() {
        let spans = vec![
            TokenSpan { offset: 0, length: 5, score: 0.8 },
            TokenSpan { offset: 5, length: 3, score: 0.7 },
            TokenSpan { offset: 20, length: 4, score: 0.9 },
        ];
        let merged = merge_spans(spans);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].offset, 0);
        assert_eq!(merged[0].length, 8);
        assert_eq!(merged[1].offset, 20);
    }

    #[test]
    fn merge_overlapping_spans() {
        let spans = vec![
            TokenSpan { offset: 0, length: 10, score: 0.8 },
            TokenSpan { offset: 5, length: 10, score: 0.9 },
        ];
        let merged = merge_spans(spans);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].offset, 0);
        assert_eq!(merged[0].length, 15);
        assert!((merged[0].score - 0.9).abs() < 1e-6);
    }
}
