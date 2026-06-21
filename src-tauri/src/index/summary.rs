/// Extractive summary generation for indexed documents.
///
/// Produces a short (≤ `max_chars`) snippet by taking the first 2–3
/// meaningful sentences from the document's full text.  No ML model
/// needed — purely string-based, so it adds zero latency to ingest.

/// Generate an extractive summary from document text.
///
/// Takes the first 2–3 meaningful sentences, cleans whitespace, and
/// returns `None` when the input is too short to be summarised
/// usefully (< 50 chars or the result would be < 30 chars).
pub fn extractive_summary(text: &str, max_chars: usize) -> Option<String> {
    if text.len() < 50 {
        return None;
    }

    let mut summary = String::new();
    let mut sentence_count = 0;

    for part in text.split_inclusive(&['.', '!', '?'][..]) {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !summary.is_empty() && !summary.ends_with(' ') {
            summary.push(' ');
        }
        summary.push_str(trimmed);
        sentence_count += 1;
        if sentence_count >= 3 || summary.len() >= max_chars {
            break;
        }
    }

    let result = summary.trim().to_string();
    if result.len() < 30 {
        return None;
    }

    // Truncate to max_chars on a word boundary if needed.
    if result.len() > max_chars {
        let truncated = &result[..max_chars];
        let last_space = truncated.rfind(' ').unwrap_or(max_chars);
        return Some(format!("{}…", &result[..last_space]));
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returns_none() {
        assert!(extractive_summary("Hello world.", 300).is_none());
    }

    #[test]
    fn extracts_first_sentences() {
        let text = "This is the first sentence. This is the second sentence. \
                     This is the third sentence. This is the fourth sentence.";
        let summary = extractive_summary(text, 300).unwrap();
        assert!(summary.contains("first sentence"));
        assert!(summary.contains("second sentence"));
        assert!(summary.contains("third sentence"));
        assert!(!summary.contains("fourth sentence"));
    }

    #[test]
    fn respects_max_chars() {
        let text = "A moderately long first sentence that has many words in it. \
                     A second sentence. A third sentence.";
        let summary = extractive_summary(text, 80).unwrap();
        assert!(summary.len() <= 85); // allow for the … character
    }

    #[test]
    fn handles_text_without_periods() {
        let text = "This is a long text without any sentence-ending punctuation \
                     that just keeps going and going and going without stopping";
        // No sentence-ending punctuation → entire text is one "sentence"
        // → returns it as the summary (it exceeds the 30-char minimum)
        let summary = extractive_summary(text, 300).unwrap();
        assert!(summary.starts_with("This is a long"));
    }

    #[test]
    fn truncates_on_word_boundary() {
        // Build a first sentence longer than max_chars=100 so the
        // truncation path is triggered.
        let long_sentence: String = "word ".repeat(100); // 500 chars, no period
        let text = format!("{}.", long_sentence.trim());
        let summary = extractive_summary(&text, 100).unwrap();
        // Must end with the ellipsis character and not exceed the limit by much.
        assert!(
            summary.ends_with('…'),
            "expected truncation ellipsis, got: {summary:?}"
        );
        // The byte length can slightly exceed max_chars because '…' is 3 bytes,
        // but the visible text before it must be ≤ max_chars.
        let without_ellipsis = summary.trim_end_matches('…');
        assert!(
            without_ellipsis.len() <= 100,
            "truncated portion exceeds max_chars: len={}", without_ellipsis.len()
        );
    }

    #[test]
    fn multiple_sentence_endings() {
        // The extractor must treat '!' and '?' as sentence boundaries, not just '.'.
        let text = "Great job! Really? Yes indeed. This fourth sentence should not appear.";
        let summary = extractive_summary(text, 300).unwrap();
        assert!(summary.contains("Great job!"), "should include '!' sentence: {summary}");
        assert!(summary.contains("Really?"), "should include '?' sentence: {summary}");
        assert!(
            !summary.contains("fourth sentence"),
            "should stop at 3 sentences: {summary}"
        );
    }
}
