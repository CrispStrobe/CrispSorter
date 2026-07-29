//! Kindle `My Clippings.txt` parsing (P32.4).
//!
//! Ported from `CrispStrobe/highlighter` (Python, MIT).  Upstream's tier 1
//! is the parse; tier 2 is locating each highlight in the real document
//! text, which lives in [`crate::kindle_match`].  Upstream's tier 3 —
//! Calibre library integration and highlighted-DOCX generation — is out
//! of scope for this slice.
//!
//! ## Format
//!
//! Records are separated by a line of ten `=`.  Each is:
//!
//! ```text
//! Title (Author)
//! - Your Highlight on page 42 | Location 1234-1236 | Added on Monday, 1 January 2024 12:00:00
//!
//! The highlighted passage.
//! ==========
//! ```
//!
//! The metadata line is localised — a German device writes
//! `- Ihre Markierung bei Position 1234-1236`.  Rather than carry a table
//! of every locale's wording, we key off the numbers and the structural
//! `|` separators, and fall back to matching any of a handful of known
//! keywords.  A record whose kind cannot be determined is kept as a
//! highlight rather than dropped: losing a passage is worse than
//! mislabelling one.

use serde::{Deserialize, Serialize};

/// What kind of clipping a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClippingKind {
    Highlight,
    Note,
    Bookmark,
}

impl ClippingKind {
    /// Map to the `ann_type` vocabulary of the annotations table.
    pub fn ann_type(self) -> &'static str {
        match self {
            ClippingKind::Highlight => "highlight",
            ClippingKind::Note => "note",
            ClippingKind::Bookmark => "bookmark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clipping {
    pub title: String,
    pub author: Option<String>,
    pub kind: ClippingKind,
    /// Printed page, when the book carries page numbers.
    pub page: Option<u32>,
    /// Kindle location range.
    pub location_start: Option<u32>,
    pub location_end: Option<u32>,
    /// The `Added on …` line, kept verbatim: it is localised and its
    /// format varies by firmware, so parsing it into a timestamp is
    /// unreliable and nothing downstream needs it sorted.
    pub added_raw: Option<String>,
    pub text: String,
}

/// Keywords that identify a record kind, across the locales we have seen.
/// Lowercased before comparison.
const NOTE_WORDS: &[&str] = &["note", "notiz", "note personnelle", "nota", "notitie"];
const BOOKMARK_WORDS: &[&str] = &["bookmark", "lesezeichen", "signet", "marcador", "bladwijzer"];

fn classify(meta: &str) -> ClippingKind {
    let m = meta.to_lowercase();
    // Check bookmark and note before highlight: a German highlight line
    // says "Markierung", which shares no prefix with these, but some
    // locales embed the word "note" inside a longer highlight phrase.
    if BOOKMARK_WORDS.iter().any(|w| m.contains(w)) {
        return ClippingKind::Bookmark;
    }
    if NOTE_WORDS.iter().any(|w| m.contains(w)) {
        return ClippingKind::Note;
    }
    ClippingKind::Highlight
}

/// Pull the first integer out of a string, if any.
fn first_number(s: &str) -> Option<u32> {
    let mut digits = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }
    digits.parse().ok()
}

/// Parse a `1234-1236` or `1234` range.
fn number_range(s: &str) -> (Option<u32>, Option<u32>) {
    let digits_and_dash: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    if digits_and_dash.is_empty() {
        return (None, None);
    }
    let mut parts = digits_and_dash.split('-');
    let start = parts.next().and_then(|p| p.parse().ok());
    let end = parts.next().and_then(|p| p.parse().ok());
    (start, end)
}

/// Split `Title (Author)` — the author is the *last* parenthesised group,
/// because titles legitimately contain parentheses
/// (e.g. `Dune (Dune Chronicles Book 1) (Frank Herbert)`).
fn split_title_author(line: &str) -> (String, Option<String>) {
    let line = line.trim().trim_start_matches('\u{feff}').trim();
    if line.ends_with(')') {
        if let Some(open) = line.rfind('(') {
            let title = line[..open].trim().to_string();
            let author = line[open + 1..line.len() - 1].trim().to_string();
            if !title.is_empty() && !author.is_empty() {
                return (title, Some(author));
            }
        }
    }
    (line.to_string(), None)
}

/// Parse the whole of a `My Clippings.txt`.
///
/// Malformed records are skipped rather than failing the import: these
/// files are appended to by the device over years and routinely contain
/// truncated tails.
pub fn parse_clippings(input: &str) -> Vec<Clipping> {
    let input = input.trim_start_matches('\u{feff}');
    let mut out = Vec::new();

    for record in input.split("==========") {
        let record = record.trim_matches(|c| c == '\r' || c == '\n');
        if record.trim().is_empty() {
            continue;
        }
        let mut lines = record.lines();
        let title_line = match lines.next() {
            Some(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let meta_line = match lines.next() {
            Some(l) => l,
            None => continue,
        };
        // The body is everything after the blank line that follows the
        // metadata. Joining with \n preserves multi-paragraph highlights.
        let body: String = lines
            .collect::<Vec<_>>()
            .join("\n")
            .trim_matches(|c: char| c == '\n' || c == '\r')
            .to_string();

        let (title, author) = split_title_author(title_line);
        let kind = classify(meta_line);

        // Metadata fields are `|`-separated; identify each by keyword
        // rather than position, since a book without page numbers omits
        // the page field entirely.
        let mut page = None;
        let mut location_start = None;
        let mut location_end = None;
        let mut added_raw = None;
        for field in meta_line.split('|') {
            let f = field.trim();
            let lower = f.to_lowercase();
            if lower.contains("page") || lower.contains("seite") || lower.contains("página") {
                page = first_number(f);
            } else if lower.contains("location")
                || lower.contains("position")
                || lower.contains("pos.")
                || lower.contains("emplacement")
            {
                let (s, e) = number_range(f);
                location_start = s;
                location_end = e;
            } else if lower.contains("added")
                || lower.contains("hinzugefügt")
                || lower.contains("ajouté")
                || lower.contains("añadido")
            {
                added_raw = Some(f.to_string());
            }
        }

        // Bookmarks have no body; every other kind without text is noise.
        if body.trim().is_empty() && kind != ClippingKind::Bookmark {
            continue;
        }

        out.push(Clipping {
            title,
            author,
            kind,
            page,
            location_start,
            location_end,
            added_raw,
            text: body,
        });
    }
    out
}

/// Collapse the duplicates a Kindle accumulates.
///
/// Adjusting a highlight's boundaries does not edit the record in place —
/// the device appends a new one. The result is runs of near-identical
/// entries where the useful one is the longest. We keep, for each
/// (title, kind, overlapping-location) group, the entry with the most
/// text, preserving first-seen order.
pub fn dedupe(clippings: Vec<Clipping>) -> Vec<Clipping> {
    let mut kept: Vec<Clipping> = Vec::new();

    'outer: for c in clippings {
        for existing in kept.iter_mut() {
            if existing.title != c.title || existing.kind != c.kind {
                continue;
            }
            // Same passage if one text contains the other (the usual
            // extend-a-highlight case) or the locations overlap.
            let text_related = !c.text.is_empty()
                && !existing.text.is_empty()
                && (existing.text.contains(&c.text) || c.text.contains(&existing.text));
            let loc_overlap = match (
                existing.location_start,
                existing.location_end.or(existing.location_start),
                c.location_start,
                c.location_end.or(c.location_start),
            ) {
                (Some(a0), Some(a1), Some(b0), Some(b1)) => a0 <= b1 && b0 <= a1,
                _ => false,
            };
            if text_related || (loc_overlap && text_related) {
                if c.text.len() > existing.text.len() {
                    *existing = c;
                }
                continue 'outer;
            }
        }
        kept.push(c);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}Thinking, Fast and Slow (Daniel Kahneman)\r\n\
- Your Highlight on page 12 | Location 234-236 | Added on Monday, 1 January 2024 12:00:00\r\n\
\r\n\
The confidence people have in their beliefs is not a measure of the quality of evidence.\r\n\
==========\r\n\
Thinking, Fast and Slow (Daniel Kahneman)\r\n\
- Your Note on page 12 | Location 236 | Added on Monday, 1 January 2024 12:01:00\r\n\
\r\n\
Remember this for the talk.\r\n\
==========\r\n";

    #[test]
    fn parses_title_author_kind_and_body() {
        let cs = parse_clippings(SAMPLE);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].title, "Thinking, Fast and Slow");
        assert_eq!(cs[0].author.as_deref(), Some("Daniel Kahneman"));
        assert_eq!(cs[0].kind, ClippingKind::Highlight);
        assert_eq!(cs[0].page, Some(12));
        assert_eq!(cs[0].location_start, Some(234));
        assert_eq!(cs[0].location_end, Some(236));
        assert!(cs[0].text.starts_with("The confidence people"));
        assert_eq!(cs[1].kind, ClippingKind::Note);
    }

    #[test]
    fn strips_the_byte_order_mark() {
        let cs = parse_clippings(SAMPLE);
        assert!(!cs[0].title.starts_with('\u{feff}'), "BOM leaked into the title");
    }

    #[test]
    fn author_is_the_last_parenthesised_group() {
        // Series titles carry their own parentheses.
        let (t, a) = split_title_author("Dune (Dune Chronicles Book 1) (Frank Herbert)");
        assert_eq!(t, "Dune (Dune Chronicles Book 1)");
        assert_eq!(a.as_deref(), Some("Frank Herbert"));
    }

    #[test]
    fn title_without_author_is_kept_whole() {
        let (t, a) = split_title_author("Some Report");
        assert_eq!(t, "Some Report");
        assert_eq!(a, None);
    }

    #[test]
    fn german_locale_is_understood() {
        let de = "Der Prozess (Franz Kafka)\n\
- Ihre Markierung bei Position 100-104 | Hinzugefügt am Montag, 1. Januar 2024 10:00:00\n\
\n\
Jemand musste Josef K. verleumdet haben.\n\
==========\n";
        let cs = parse_clippings(de);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].kind, ClippingKind::Highlight);
        assert_eq!(cs[0].location_start, Some(100));
        assert_eq!(cs[0].location_end, Some(104));
        assert!(cs[0].added_raw.is_some());
    }

    #[test]
    fn german_note_is_classified_as_a_note() {
        let de = "Der Prozess (Franz Kafka)\n\
- Ihre Notiz bei Position 100 | Hinzugefügt am Montag, 1. Januar 2024 10:00:00\n\
\n\
Wichtig für das Referat.\n\
==========\n";
        assert_eq!(parse_clippings(de)[0].kind, ClippingKind::Note);
    }

    #[test]
    fn book_without_page_numbers_still_parses() {
        let src = "A Book (An Author)\n\
- Your Highlight at location 500-510 | Added on Monday, 1 January 2024 12:00:00\n\
\n\
Some text.\n\
==========\n";
        let cs = parse_clippings(src);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].page, None);
        assert_eq!(cs[0].location_start, Some(500));
    }

    #[test]
    fn multi_paragraph_highlight_keeps_its_breaks() {
        let src = "A Book (An Author)\n\
- Your Highlight on page 1 | Location 1-2 | Added on X\n\
\n\
First paragraph.\n\
\n\
Second paragraph.\n\
==========\n";
        let cs = parse_clippings(src);
        assert!(cs[0].text.contains("First paragraph."));
        assert!(cs[0].text.contains("Second paragraph."));
    }

    #[test]
    fn truncated_tail_is_skipped_not_fatal() {
        // Devices routinely leave a partial record at the end of the file.
        let src = format!("{SAMPLE}A Truncated Title (Someone)\n");
        let cs = parse_clippings(&src);
        assert_eq!(cs.len(), 2, "the incomplete trailing record is dropped");
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(parse_clippings("").is_empty());
        assert!(parse_clippings("\n\n==========\n\n").is_empty());
    }

    #[test]
    fn bookmarks_survive_having_no_body() {
        let src = "A Book (An Author)\n\
- Your Bookmark on page 7 | Location 99 | Added on X\n\
\n\
==========\n";
        let cs = parse_clippings(src);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].kind, ClippingKind::Bookmark);
    }

    #[test]
    fn dedupe_keeps_the_longest_of_an_extended_highlight() {
        let src = "A Book (An Author)\n\
- Your Highlight on page 1 | Location 10-12 | Added on X\n\
\n\
The quick brown fox\n\
==========\n\
A Book (An Author)\n\
- Your Highlight on page 1 | Location 10-14 | Added on Y\n\
\n\
The quick brown fox jumps over the lazy dog\n\
==========\n";
        let cs = dedupe(parse_clippings(src));
        assert_eq!(cs.len(), 1, "the extended highlight supersedes the shorter one");
        assert!(cs[0].text.ends_with("lazy dog"));
    }

    #[test]
    fn dedupe_keeps_genuinely_distinct_passages() {
        let cs = dedupe(parse_clippings(SAMPLE));
        assert_eq!(cs.len(), 2, "a highlight and a note are not duplicates");
    }

    #[test]
    fn dedupe_does_not_merge_across_books() {
        let src = "Book One (A)\n\
- Your Highlight on page 1 | Location 1-2 | Added on X\n\
\n\
Same text\n\
==========\n\
Book Two (B)\n\
- Your Highlight on page 1 | Location 1-2 | Added on X\n\
\n\
Same text\n\
==========\n";
        assert_eq!(dedupe(parse_clippings(src)).len(), 2);
    }
}
