//! Locating a Kindle highlight inside the real document text (P32.4,
//! tier 2 of the `CrispStrobe/highlighter` port).
//!
//! A clipping arrives as a bare passage with no offsets — Kindle
//! *locations* are a device-internal unit that does not map to anything
//! in the source file.  Without anchoring, an imported highlight can only
//! be listed; it cannot be shown in position in the viewer, which is most
//! of the point.
//!
//! The passage is rarely byte-identical to the source: the exporter
//! normalises quotes and dashes, soft hyphens vanish, ligatures resolve,
//! and OCR'd sources drift further still.  So matching is a cascade,
//! cheapest first:
//!
//! 1. **Exact** — `find` on the raw text.
//! 2. **Normalised** — case-folded, punctuation-stripped, whitespace-
//!    collapsed, diacritics folded, with an index back to original byte
//!    offsets.  Catches the great majority of real cases.
//! 3. **Fuzzy** — similarity over candidate windows, anchored on a rare
//!    word so we do not slide across the whole book.
//! 4. **Semantic** — optional, supplied by the caller.  Kept as a
//!    callback so this module stays free of the embedding feature flags;
//!    `fastembed` / CrispEmbed plug in from above, and
//!    `crisp-docx-align::align_texts` can refine an approximate window
//!    down to token offsets.
//!
//! Every tier returns byte offsets into the *original* text, so callers
//! never have to reason about the normalised form.

use serde::{Deserialize, Serialize};

/// How a passage was located.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMethod {
    Exact,
    Normalized,
    Fuzzy,
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassageMatch {
    /// Byte offsets into the original document text.
    pub start: usize,
    pub end: usize,
    /// 0.0–1.0. Exact and normalised matches score 1.0.
    pub score: f32,
    pub method: MatchMethod,
}

// ── Normalisation ──────────────────────────────────────────────────────

/// A normalised copy of some text, plus an index from each normalised
/// byte back to the byte offset it came from in the original.
pub struct Normalized {
    pub text: String,
    /// `map[i]` is the original byte offset of normalised byte `i`.
    /// One entry per byte of `text`, so slicing stays in step.
    map: Vec<usize>,
}

impl Normalized {
    /// Translate a range in the normalised text back to the original.
    ///
    /// The end is the original offset *just past* the last contributing
    /// character, so the slice covers the whole matched region including
    /// any punctuation normalisation dropped inside it.
    pub fn to_original(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end || end > self.map.len() {
            return None;
        }
        let orig_start = self.map[start];
        // `end` is exclusive; the last contributing byte is at end-1.
        let orig_last = self.map[end - 1];
        Some((orig_start, orig_last))
    }
}

/// Case-fold, fold diacritics, strip punctuation, collapse whitespace.
///
/// Diacritic folding matters because exporters are inconsistent about
/// composed vs decomposed forms, so `é` may or may not be one character.
pub fn normalize(input: &str) -> Normalized {
    let mut text = String::with_capacity(input.len());
    let mut map: Vec<usize> = Vec::with_capacity(input.len());
    let mut pending_space = false;
    // Set once any real character is emitted, so leading space is dropped.
    let mut emitted = false;

    for (byte_off, ch) in input.char_indices() {
        if ch.is_whitespace() {
            if emitted {
                pending_space = true;
            }
            continue;
        }
        // Punctuation is dropped rather than mapped to a space: the
        // exporter's quote and dash substitutions are exactly what we are
        // trying to see past.  Soft hyphens are already covered here.
        if !ch.is_alphanumeric() {
            continue;
        }
        if pending_space {
            text.push(' ');
            map.push(byte_off);
            pending_space = false;
        }
        // deunicode expands some characters to several ASCII ones (æ→ae);
        // every emitted byte points back at the same source character.
        let folded = deunicode::deunicode_char(ch).unwrap_or("");
        for b in folded.to_lowercase().bytes() {
            text.push(b as char);
            map.push(byte_off);
        }
        emitted = true;
    }

    debug_assert_eq!(text.len(), map.len(), "offset map must track the normalised bytes");
    Normalized { text, map }
}

// ── Similarity ─────────────────────────────────────────────────────────

/// Similarity of two normalised strings, 0.0–1.0.
///
/// Uses `similar`'s char-level diff ratio, which is already a dependency
/// (P25.7 document comparison) — no new crate for this.
fn ratio(a: &str, b: &str) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    similar::TextDiff::from_chars(a, b).ratio()
}

/// Pick the rarest long word to anchor the fuzzy search on.
///
/// Sliding a window across an entire book is O(doc × passage) and far too
/// slow; anchoring on a distinctive word reduces it to a handful of
/// candidate positions. Longest-word is a cheap proxy for rarest.
fn anchor_word(needle: &str) -> Option<&str> {
    needle
        .split(' ')
        .filter(|w| w.len() >= 5)
        .max_by_key(|w| w.len())
}

// ── The cascade ────────────────────────────────────────────────────────

/// Locate `passage` in `document`, returning byte offsets into `document`.
///
/// `min_score` gates the fuzzy tier; below it, no match is reported
/// rather than a wrong one. 0.82 is a reasonable default — high enough to
/// reject a different paragraph, loose enough to absorb OCR noise.
pub fn find_passage(document: &str, passage: &str, min_score: f32) -> Option<PassageMatch> {
    let trimmed = passage.trim();
    if trimmed.is_empty() || document.is_empty() {
        return None;
    }

    // 1. Exact.
    if let Some(pos) = document.find(trimmed) {
        return Some(PassageMatch {
            start: pos,
            end: pos + trimmed.len(),
            score: 1.0,
            method: MatchMethod::Exact,
        });
    }

    let ndoc = normalize(document);
    let npas = normalize(trimmed);
    if npas.text.is_empty() {
        return None;
    }

    // 2. Normalised exact.
    if let Some(pos) = ndoc.text.find(&npas.text) {
        if let Some((s, e)) = ndoc.to_original(pos, pos + npas.text.len()) {
            return Some(PassageMatch {
                start: s,
                // Extend to the end of the character that last contributed.
                end: char_end(document, e),
                score: 1.0,
                method: MatchMethod::Normalized,
            });
        }
    }

    // 3. Fuzzy over anchored candidate windows.
    let plen = npas.text.len();
    let mut best: Option<(usize, usize, f32)> = None;

    // Takes `best` as a parameter rather than capturing it, so the
    // closure stays Fn and the borrow checker allows the anchor loop
    // below to keep using `best` between calls.
    let consider = |start: usize, end: usize, best: &mut Option<(usize, usize, f32)>| {
        if start >= end || end > ndoc.text.len() {
            return;
        }
        if !ndoc.text.is_char_boundary(start) || !ndoc.text.is_char_boundary(end) {
            return;
        }
        let score = ratio(&ndoc.text[start..end], &npas.text);
        if best.map_or(true, |(_, _, b)| score > b) {
            *best = Some((start, end, score));
        }
    };

    let anchor = anchor_word(&npas.text);
    if let Some(anchor) = anchor {
        // Where the anchor sits inside the passage, so a window can be
        // positioned to line up with it.
        let anchor_off = npas.text.find(anchor).unwrap_or(0);
        let slack = (plen / 4).max(16) as isize;
        let mut search_from = 0;
        let mut hits = 0;
        while let Some(rel) = ndoc.text[search_from..].find(anchor) {
            let at = search_from + rel;
            // Test windows the *same length as the passage*, shifted around
            // the anchor. A window padded out to plen + 2*slack would score
            // badly for exactly the near-miss cases this tier exists to
            // catch: the extra text is all mismatch, and on a short passage
            // it dominates the ratio.
            let base = at as isize - anchor_off as isize;
            for d in [-slack, -slack / 2, 0, slack / 2, slack] {
                let s = base.saturating_add(d).max(0) as usize;
                let e = (s + plen).min(ndoc.text.len());
                consider(s, e, &mut best);
            }
            search_from = at + anchor.len();
            hits += 1;
            // A word common enough to appear this often is not an anchor;
            // stop rather than degenerate into a full scan.
            if hits >= 64 || search_from >= ndoc.text.len() {
                break;
            }
        }
    }

    if best.is_none() {
        // No usable anchor (very short passage, or an anchor that never
        // occurs). Fall back to a coarse stride so we still find
        // something, at a cost proportional to doc length / stride.
        let stride = (plen / 2).max(32);
        let mut start = 0;
        while start < ndoc.text.len() {
            let end = (start + plen).min(ndoc.text.len());
            consider(start, end, &mut best);
            start += stride;
        }
    }

    let (bs, be, score) = best?;
    if score < min_score {
        return None;
    }
    let (s, e) = ndoc.to_original(bs, be.min(ndoc.text.len()))?;
    Some(PassageMatch {
        start: s,
        end: char_end(document, e),
        score,
        method: MatchMethod::Fuzzy,
    })
}

/// Advance a byte offset to the end of the character starting there, so
/// returned ranges never split a multi-byte character.
fn char_end(s: &str, byte_off: usize) -> usize {
    if byte_off >= s.len() {
        return s.len();
    }
    let mut e = byte_off + 1;
    while e < s.len() && !s.is_char_boundary(e) {
        e += 1;
    }
    e
}

/// Signature for the optional semantic tier.
///
/// Given the passage and a list of candidate chunks, return the index of
/// the best chunk and its score. Implemented by the caller so the
/// embedding backend (fastembed / CrispEmbed) and its feature flags stay
/// out of this module.
pub type SemanticMatcher<'a> = &'a dyn Fn(&str, &[&str]) -> Option<(usize, f32)>;

/// The cascade, with the semantic tier appended.
///
/// `chunks` are `(byte_offset, text)` pairs from the same document, as
/// produced by the indexer. Only consulted when tiers 1–3 fail.
pub fn find_passage_semantic(
    document: &str,
    passage: &str,
    min_score: f32,
    chunks: &[(usize, String)],
    matcher: SemanticMatcher<'_>,
) -> Option<PassageMatch> {
    if let Some(m) = find_passage(document, passage, min_score) {
        return Some(m);
    }
    if chunks.is_empty() {
        return None;
    }
    let texts: Vec<&str> = chunks.iter().map(|(_, t)| t.as_str()).collect();
    let (idx, score) = matcher(passage, &texts)?;
    if score < min_score {
        return None;
    }
    let (offset, text) = chunks.get(idx)?;
    Some(PassageMatch {
        start: *offset,
        end: (*offset + text.len()).min(document.len()),
        score,
        method: MatchMethod::Semantic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "Chapter One\n\nThe confidence people have in their beliefs is not a measure \
of the quality of evidence. It is a measure of the coherence of the story.\n\n\
Chapter Two\n\nNothing else here matters very much at all.";

    #[test]
    fn exact_match_is_found_verbatim() {
        let m = find_passage(DOC, "coherence of the story", 0.8).unwrap();
        assert_eq!(m.method, MatchMethod::Exact);
        assert_eq!(&DOC[m.start..m.end], "coherence of the story");
    }

    #[test]
    fn curly_quotes_and_dashes_match_through_normalisation() {
        // Exporters substitute punctuation; the passage is otherwise identical.
        let doc = "He said “it is a measure” — of the story.";
        let m = find_passage(doc, "it is a measure - of the story", 0.8).unwrap();
        assert_eq!(m.method, MatchMethod::Normalized);
        assert!(doc[m.start..m.end].contains("measure"));
    }

    #[test]
    fn case_and_whitespace_differences_do_not_defeat_matching() {
        let m = find_passage(DOC, "THE   CONFIDENCE\n\nPEOPLE have", 0.8).unwrap();
        assert!(matches!(m.method, MatchMethod::Normalized | MatchMethod::Fuzzy));
        assert!(DOC[m.start..m.end].to_lowercase().contains("confidence"));
    }

    #[test]
    fn diacritics_fold() {
        let doc = "Ein Satz über Qualität und Kohärenz.";
        let m = find_passage(doc, "uber Qualitat und Koharenz", 0.8).unwrap();
        assert!(matches!(m.method, MatchMethod::Normalized | MatchMethod::Fuzzy));
    }

    #[test]
    fn offsets_land_on_character_boundaries_for_multibyte_text() {
        let doc = "Vorwort. Ein Satz über Qualität und Kohärenz. Ende.";
        let m = find_passage(doc, "uber Qualitat und Koharenz", 0.8).unwrap();
        // Slicing would panic if the offsets split a multi-byte character.
        let slice = &doc[m.start..m.end];
        assert!(slice.contains("ber"), "got: {slice:?}");
    }

    #[test]
    fn near_miss_matches_fuzzily() {
        // One word altered, as OCR or a typo would.
        let m = find_passage(DOC, "the coherence of the stony", 0.8).unwrap();
        assert_eq!(m.method, MatchMethod::Fuzzy);
        assert!(m.score >= 0.8);
    }

    #[test]
    fn an_unrelated_passage_is_rejected_rather_than_forced() {
        // The whole point of min_score: a wrong anchor is worse than none.
        assert!(find_passage(DOC, "entirely unrelated sentence about penguins", 0.9).is_none());
    }

    #[test]
    fn empty_inputs_are_handled() {
        assert!(find_passage(DOC, "", 0.8).is_none());
        assert!(find_passage("", "something", 0.8).is_none());
        assert!(find_passage(DOC, "   \n  ", 0.8).is_none());
    }

    #[test]
    fn punctuation_only_passage_does_not_match_everything() {
        // Normalising strips punctuation, so this reduces to empty and
        // must not be reported as matching at offset 0.
        assert!(find_passage(DOC, "!!! ---- ???", 0.8).is_none());
    }

    #[test]
    fn normalisation_offset_map_tracks_bytes() {
        let n = normalize("Héllo,  wörld!");
        assert_eq!(n.text, "hello world");
        assert_eq!(n.text.len(), n.map.len());
        // The 'w' of "world" must map back into the original "wörld".
        let w = n.text.find('w').unwrap();
        assert_eq!(&"Héllo,  wörld!"[n.map[w]..n.map[w] + 1], "w");
    }

    #[test]
    fn semantic_tier_runs_only_when_the_others_fail() {
        use std::cell::Cell;
        // Cell rather than a plain bool: the callback is taken as `&dyn Fn`,
        // so a closure that assigns to a captured local would only be FnMut.
        let called = Cell::new(false);
        let matcher = |_: &str, _: &[&str]| -> Option<(usize, f32)> {
            called.set(true);
            Some((0, 0.95))
        };

        // An exact match must short-circuit before the callback.
        let m = find_passage_semantic(DOC, "Chapter Two", 0.8, &[(0, "x".into())], &matcher);
        assert_eq!(m.unwrap().method, MatchMethod::Exact);
        assert!(!called.get(), "semantic tier ran despite an exact match");

        let chunks = vec![(12usize, "The confidence people have".to_string())];
        let m = find_passage_semantic(DOC, "a paraphrase about certainty", 0.9, &chunks, &matcher);
        assert_eq!(m.unwrap().method, MatchMethod::Semantic);
        assert!(called.get(), "semantic tier should have been consulted");
    }

    #[test]
    fn semantic_result_below_threshold_is_rejected() {
        let matcher = |_: &str, _: &[&str]| -> Option<(usize, f32)> { Some((0, 0.10)) };
        let chunks = vec![(0usize, "irrelevant".to_string())];
        assert!(find_passage_semantic(DOC, "no such text here at all", 0.9, &chunks, &matcher).is_none());
    }
}
