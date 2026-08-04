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
    //
    // `String::len()` and slicing are both in *bytes*, so cutting at
    // `max_chars` lands mid-character whenever the text is not pure ASCII —
    // and German administrative prose is full of ä/ö/ü/ß, each two bytes in
    // UTF-8. That panicked the whole ingest with
    //   "byte index 300 is not a char boundary; it is inside 'ü'".
    // Walk back to the nearest boundary before slicing. (`rfind(' ')` is safe
    // either way: a space is ASCII, so its index is always a boundary.)
    if result.len() > max_chars {
        let mut cut = max_chars;
        while cut > 0 && !result.is_char_boundary(cut) {
            cut -= 1;
        }
        let truncated = &result[..cut];
        let last_space = truncated.rfind(' ').unwrap_or(cut);
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
    fn truncating_mid_umlaut_does_not_panic() {
        // Regression: `&result[..max_chars]` sliced by *bytes*, so a cap that
        // landed inside a two-byte character panicked the whole ingest with
        // "byte index N is not a char boundary; it is inside 'ü'". Every
        // German document is a candidate, so this crashed on real corpora.
        //
        // Sweep a range of caps so the cut lands inside a multi-byte
        // character for some of them regardless of the exact prefix length.
        let text = "Grundstück Gebührenordnung Fördermittel Niederschrift Beschluss \
                    Stellungnahme Liegenschaft Jahresabschluss Wirtschaftsplan \
                    Rechnungsprüfung Zuwendung Bescheid Satzung Personalrat."
            .repeat(4);
        for max_chars in 30..320 {
            let got = extractive_summary(&text, max_chars);
            if let Some(s) = got {
                // Whatever comes back must be valid UTF-8 the caller can slice
                // again — trivially true for String, but assert the cut did
                // not drop us below the caller's floor.
                assert!(!s.is_empty(), "max_chars={max_chars} produced an empty summary");
            }
        }
    }

    #[test]
    fn truncation_stays_within_the_cap_for_non_ascii() {
        let text = "Die Gebührenordnung für Grundstücke regelt Zuwendungen. ".repeat(20);
        let s = extractive_summary(&text, 100).expect("long text should summarise");
        // The ellipsis is added after the cut, so allow its 3 bytes.
        assert!(
            s.len() <= 100 + '…'.len_utf8(),
            "summary overshot the cap: {} bytes",
            s.len()
        );
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
