//! Side-by-side document comparison (P25.7).
//!
//! Compares two documents' text content at the word level using the
//! `similar` crate and returns a structured diff.

use serde::Serialize;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Serialize)]
pub struct DiffSegment {
    /// "equal", "insert", "delete"
    pub tag: String,
    /// The text content of this segment.
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonResult {
    /// Diff segments for document A (delete = removed from A, equal = shared).
    pub segments: Vec<DiffSegment>,
    /// Statistics
    pub total_words_a: usize,
    pub total_words_b: usize,
    pub added_words: usize,
    pub removed_words: usize,
    pub changed_ratio: f64,
}

/// Compare two text strings at the word level.
pub fn compare_texts(text_a: &str, text_b: &str) -> ComparisonResult {
    let diff = TextDiff::from_words(text_a, text_b);

    let mut segments = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    for change in diff.iter_all_changes() {
        let tag = match change.tag() {
            ChangeTag::Equal => "equal",
            ChangeTag::Insert => { added += 1; "insert" }
            ChangeTag::Delete => { removed += 1; "delete" }
        };
        // Merge consecutive same-tag segments
        if let Some(last) = segments.last_mut() {
            let last: &mut DiffSegment = last;
            if last.tag == tag {
                last.text.push_str(change.as_str().unwrap_or(""));
                continue;
            }
        }
        segments.push(DiffSegment {
            tag: tag.into(),
            text: change.as_str().unwrap_or("").to_string(),
        });
    }

    let words_a = text_a.split_whitespace().count();
    let words_b = text_b.split_whitespace().count();
    let total = (words_a + words_b).max(1);
    let changed_ratio = (added + removed) as f64 / total as f64;

    ComparisonResult {
        segments,
        total_words_a: words_a,
        total_words_b: words_b,
        added_words: added,
        removed_words: removed,
        changed_ratio,
    }
}

// ── Tauri commands ─────────────────────────────────────────────────────

pub mod tauri_commands {
    use super::*;
    use tauri::State;
    use crate::AppState;

    /// Compare two indexed documents by their doc_ids.  Fetches the
    /// full_text of each from the LanceDB index and returns a word-level
    /// diff.
    #[tauri::command]
    pub async fn compare_documents(
        state: State<'_, AppState>,
        doc_id_a: String,
        doc_id_b: String,
    ) -> Result<ComparisonResult, String> {
        let lock = state.index.lock().await;
        if !lock.config.enabled {
            return Err("Index is disabled".into());
        }
        let local = lock.local.as_ref().ok_or("Comparison requires local backend")?.clone();
        drop(lock);

        let text_a = local.fetch_full_text(&doc_id_a).await.map_err(|e| e.to_string())?;
        let text_b = local.fetch_full_text(&doc_id_b).await.map_err(|e| e.to_string())?;

        Ok(compare_texts(&text_a, &text_b))
    }

    /// Compare two raw text strings (no index lookup needed).
    #[tauri::command]
    pub async fn compare_texts_raw(
        text_a: String,
        text_b: String,
    ) -> Result<ComparisonResult, String> {
        Ok(super::compare_texts(&text_a, &text_b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts() {
        let r = compare_texts("hello world", "hello world");
        assert_eq!(r.added_words, 0);
        assert_eq!(r.removed_words, 0);
        assert!((r.changed_ratio).abs() < 0.001);
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.segments[0].tag, "equal");
    }

    #[test]
    fn completely_different() {
        let r = compare_texts("aaa bbb", "ccc ddd");
        assert!(r.added_words > 0);
        assert!(r.removed_words > 0);
    }

    #[test]
    fn insertion() {
        let r = compare_texts("hello world", "hello beautiful world");
        assert!(r.added_words > 0);
        assert_eq!(r.removed_words, 0);
    }

    #[test]
    fn deletion() {
        let r = compare_texts("hello beautiful world", "hello world");
        assert_eq!(r.added_words, 0);
        assert!(r.removed_words > 0);
    }

    #[test]
    fn empty_texts() {
        let r = compare_texts("", "");
        assert_eq!(r.total_words_a, 0);
        assert_eq!(r.total_words_b, 0);
        assert_eq!(r.added_words, 0);
    }

    #[test]
    fn one_empty() {
        let r = compare_texts("hello world", "");
        assert!(r.removed_words > 0);
        assert_eq!(r.added_words, 0);
    }

    #[test]
    fn segments_have_all_three_tags() {
        let r = compare_texts("a b c d e", "a b x y e");
        let tags: Vec<&str> = r.segments.iter().map(|s| s.tag.as_str()).collect();
        assert!(tags.contains(&"equal"));
        assert!(tags.contains(&"insert") || tags.contains(&"delete"));
    }

    #[test]
    fn changed_ratio_zero_for_identical() {
        let r = compare_texts("hello world", "hello world");
        assert!((r.changed_ratio).abs() < 0.001);
    }

    #[test]
    fn changed_ratio_high_for_different() {
        let r = compare_texts("aaa bbb ccc", "xxx yyy zzz");
        assert!(r.changed_ratio > 0.5);
    }

    #[test]
    fn long_text_comparison() {
        let a = (0..100).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
        let b = (0..100).map(|i| if i == 50 { "CHANGED".to_string() } else { format!("word{i}") }).collect::<Vec<_>>().join(" ");
        let r = compare_texts(&a, &b);
        assert!(r.added_words >= 1);
        assert!(r.removed_words >= 1);
        assert!(r.changed_ratio < 0.1); // only 1 word changed out of 100
    }

    #[test]
    fn whitespace_only_difference() {
        let r = compare_texts("hello  world", "hello world");
        // similar treats whitespace as separate tokens
        assert!(r.segments.len() >= 1);
    }
}
