//! Term-highlighted search snippets.
//!
//! Builds a compact, HTML-escaped window of body text centred on the first
//! query-term match, wrapping each matched term in `<mark>…</mark>`.  Used by
//! the unified `search` verb (both the local and federated legs) so a result
//! row reads like a search engine rather than a wall of raw `full_text`.
//!
//! The whole thing is pure and synchronous — no allocation surprises, no
//! Unicode boundary panics (we operate on `char` vectors, not byte slices) —
//! so it's cheap to run on every hit before rendering.

/// Default snippet window, in characters (roughly ±150 around the match).
pub const SNIPPET_WINDOW: usize = 300;

/// HTML-escape the five characters that matter inside a `{@html …}` block so
/// the snippet is safe to render verbatim frontend-side.
fn escape_html_char(c: char, out: &mut String) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(c),
    }
}

/// Lowercase a single `char` to a single `char`, keeping a 1:1 alignment with
/// the original character vector.  A handful of code points lowercase to more
/// than one char (e.g. `İ`); we keep only the first so index alignment with
/// the original `chars` vector is preserved — good enough for substring
/// matching, and it never panics.
fn lower1(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Tokenise the query into lowercased `char` needles.  Tokens shorter than two
/// characters are dropped to avoid highlighting noise (`a`, `de`-style stop
/// fragments); if that leaves nothing, we fall back to the raw non-empty
/// tokens so a single-character CJK query still highlights.
fn query_tokens(query: &str) -> Vec<Vec<char>> {
    let raw: Vec<Vec<char>> = query
        .split_whitespace()
        .map(|t| t.chars().map(lower1).collect::<Vec<char>>())
        .filter(|t| !t.is_empty())
        .collect();
    let kept: Vec<Vec<char>> = raw.iter().filter(|t| t.len() >= 2).cloned().collect();
    if kept.is_empty() {
        raw
    } else {
        kept
    }
}

/// Index of the first occurrence of `needle` in `hay[from..]`, or `None`.
fn find_sub(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    (from..=last).find(|&i| hay[i..i + needle.len()] == needle[..])
}

/// Build a highlighted snippet from `body` for `query`.
///
/// - Finds the earliest case-insensitive occurrence of any query token in
///   `body` and centres a `window`-char window on it (snapped to char
///   boundaries), with leading/trailing `…` when the body is truncated.
/// - HTML-escapes the window, then wraps every case-insensitive token match
///   inside it in `<mark>…</mark>`.
/// - When no token matches, falls back to the leading `window` chars
///   (escaped, no highlight) so the caller always gets *some* context.
/// - Returns `None` only when `body` is empty/whitespace.
pub fn highlight_snippet(body: &str, query: &str, window: usize) -> Option<String> {
    let chars: Vec<char> = body.chars().collect();
    if chars.iter().all(|c| c.is_whitespace()) {
        return None;
    }
    let lower: Vec<char> = chars.iter().map(|&c| lower1(c)).collect();
    let tokens = query_tokens(query);

    // Earliest match position across all tokens (if any).
    let first_match = tokens
        .iter()
        .filter_map(|t| find_sub(&lower, t, 0))
        .min();

    let len = chars.len();
    let (start, end) = match first_match {
        Some(m) => {
            // Place the match about a third of the way into the window so the
            // reader gets some leading context but mostly trailing context.
            let lead = window / 3;
            let mut s = m.saturating_sub(lead);
            let mut e = (s + window).min(len);
            // If we bumped against the end, pull the start back to fill it.
            if e == len {
                s = len.saturating_sub(window);
            }
            // Don't let the snap-back hide the match itself.
            s = s.min(m);
            e = e.max((m + 1).min(len));
            (s, e)
        }
        None => (0, window.min(len)),
    };

    let mut out = String::with_capacity((end - start) + 16);
    if start > 0 {
        out.push('…');
    }
    let mut i = start;
    while i < end {
        // Longest token match anchored at i and fully inside the window wins.
        let matched_len = tokens
            .iter()
            .filter(|t| i + t.len() <= end && lower[i..i + t.len()] == t[..])
            .map(|t| t.len())
            .max();
        match matched_len {
            Some(n) if n > 0 => {
                out.push_str("<mark>");
                for &c in &chars[i..i + n] {
                    escape_html_char(c, &mut out);
                }
                out.push_str("</mark>");
                i += n;
            }
            _ => {
                escape_html_char(chars[i], &mut out);
                i += 1;
            }
        }
    }
    if end < len {
        out.push('…');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_the_matched_term() {
        let body = "Stiftung Warentest hat in Nuss-Nougat-Cremes Schimmelpilz-Gifte gefunden.";
        let s = highlight_snippet(body, "schimmelpilz", SNIPPET_WINDOW).unwrap();
        assert!(s.contains("<mark>Schimmelpilz</mark>"), "got: {s}");
        // Original casing is preserved inside the mark.
        assert!(!s.contains("<mark>schimmelpilz</mark>"));
    }

    #[test]
    fn windows_around_a_deep_match() {
        let prefix = "x".repeat(1000);
        let body = format!("{prefix} needle tail");
        let s = highlight_snippet(&body, "needle", 60).unwrap();
        assert!(s.contains("<mark>needle</mark>"), "got: {s}");
        assert!(s.starts_with('…'), "leading ellipsis expected: {s}");
        // The 1000-char prefix must not be dragged in wholesale.
        assert!(s.chars().count() < 120, "window too wide: {} chars", s.chars().count());
    }

    #[test]
    fn escapes_html() {
        let body = "tags are <b>bold</b> & \"quoted\" plus needle";
        let s = highlight_snippet(body, "needle", SNIPPET_WINDOW).unwrap();
        assert!(s.contains("&lt;b&gt;"), "got: {s}");
        assert!(s.contains("&amp;"), "got: {s}");
        assert!(s.contains("&quot;"), "got: {s}");
        assert!(!s.contains("<b>"));
    }

    #[test]
    fn does_not_inject_raw_query_html() {
        // A query token that happens to contain markup must not break escaping
        // of the body around it.
        let body = "before <script> after";
        let s = highlight_snippet(body, "script", SNIPPET_WINDOW).unwrap();
        assert!(s.contains("&lt;<mark>script</mark>&gt;"), "got: {s}");
    }

    #[test]
    fn no_match_falls_back_to_leading_window() {
        let body = "alpha beta gamma delta epsilon";
        let s = highlight_snippet(body, "zzz", 11).unwrap();
        assert!(!s.contains("<mark>"));
        assert!(s.starts_with("alpha"), "got: {s}");
        assert!(s.ends_with('…'), "trailing ellipsis expected: {s}");
    }

    #[test]
    fn empty_body_is_none() {
        assert_eq!(highlight_snippet("", "x", SNIPPET_WINDOW), None);
        assert_eq!(highlight_snippet("   \n\t ", "x", SNIPPET_WINDOW), None);
    }

    #[test]
    fn case_insensitive_and_unicode_safe() {
        let body = "Müller über ÜBERMUT und Überblick";
        let s = highlight_snippet(body, "über", SNIPPET_WINDOW).unwrap();
        // Matches both "über" and the upper-case "ÜBER" prefix; original
        // casing preserved, no panic on multibyte boundaries.
        assert!(s.contains("<mark>über</mark>"), "got: {s}");
        assert!(s.contains("<mark>ÜBER</mark>"), "got: {s}");
    }

    #[test]
    fn multi_token_query_highlights_each() {
        let body = "the quick brown fox jumps";
        let s = highlight_snippet(body, "quick fox", SNIPPET_WINDOW).unwrap();
        assert!(s.contains("<mark>quick</mark>"), "got: {s}");
        assert!(s.contains("<mark>fox</mark>"), "got: {s}");
    }
}
