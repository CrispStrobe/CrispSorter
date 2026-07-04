/// Natural-language query → structured filters parser.
///
/// Extracts year ranges, language hints, file-type filters, folder
/// prefixes, tag constraints, and URL-domain filters from a free-text
/// query string.  The cleaned query (with recognised filter fragments
/// removed) is returned alongside the structured `SearchFilters`.
///
/// No regex crate needed — uses simple string matching.

use super::schema::SearchFilters;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedQuery {
    pub cleaned_query: String,
    pub filters: SearchFilters,
}

/// Parse a natural-language query into structured filters + cleaned text.
pub fn parse_nl_query(input: &str) -> ParsedQuery {
    let mut filters = SearchFilters::default();
    let mut cleaned = input.to_string();

    // ── Year patterns ────────────────────────────────────────────────
    // "from 2023", "since 2020", "after 2019"
    for prefix in &["from ", "since ", "after ", "ab ", "seit "] {
        if let Some(year) = extract_year_after_prefix(&cleaned, prefix) {
            filters.year_min = Some(year);
            cleaned = remove_fragment(&cleaned, &format!("{}{}", prefix, year));
        }
    }
    // "before 2025", "until 2024", "bis 2024", "vor 2025"
    for prefix in &["before ", "until ", "bis ", "vor "] {
        if let Some(year) = extract_year_after_prefix(&cleaned, prefix) {
            filters.year_max = Some(year);
            cleaned = remove_fragment(&cleaned, &format!("{}{}", prefix, year));
        }
    }
    // "2020-2024" or "2020–2024"
    if let Some((y1, y2)) = extract_year_range(&cleaned) {
        filters.year_min = Some(y1);
        filters.year_max = Some(y2);
        // Remove both forms
        cleaned = remove_fragment(&cleaned, &format!("{}-{}", y1, y2));
        cleaned = remove_fragment(&cleaned, &format!("{}–{}", y1, y2));
    }

    // Compute lowercase once; recompute only after mutations to `cleaned`
    // (previously 5 separate allocations per parse).
    let mut lower = cleaned.to_lowercase();

    // ── Language ─────────────────────────────────────────────────────
    let lang_map: &[(&str, &str)] = &[
        ("in german", "de"),
        ("in deutsch", "de"),
        ("auf deutsch", "de"),
        ("in english", "en"),
        ("auf englisch", "en"),
        ("in englisch", "en"),
        ("in french", "fr"),
        ("auf französisch", "fr"),
        ("in französisch", "fr"),
        ("in spanish", "es"),
        ("auf spanisch", "es"),
        ("in italian", "it"),
        ("auf italienisch", "it"),
        ("in bosnian", "bs"),
        ("auf bosnisch", "bs"),
        ("in croatian", "hr"),
        ("auf kroatisch", "hr"),
        ("in serbian", "sr"),
        ("auf serbisch", "sr"),
        ("in turkish", "tr"),
        ("auf türkisch", "tr"),
        ("in arabic", "ar"),
        ("auf arabisch", "ar"),
        ("in japanese", "ja"),
        ("auf japanisch", "ja"),
        ("in chinese", "zh"),
        ("auf chinesisch", "zh"),
        ("in russian", "ru"),
        ("auf russisch", "ru"),
        ("in portuguese", "pt"),
        ("auf portugiesisch", "pt"),
        ("in dutch", "nl"),
        ("auf niederländisch", "nl"),
        ("in polish", "pl"),
        ("auf polnisch", "pl"),
        ("in korean", "ko"),
        ("auf koreanisch", "ko"),
    ];
    for (phrase, code) in lang_map {
        if lower.contains(phrase) {
            filters.language = Some(code.to_string());
            cleaned = remove_fragment_ci(&cleaned, phrase);
            lower = cleaned.to_lowercase();
            break;
        }
    }

    // ── File type ────────────────────────────────────────────────────
    // "pdf files", "PDFs", ".docx", "mp3 files"
    let ext_map: &[(&str, &str)] = &[
        ("pdf files", "pdf"),
        ("pdfs", "pdf"),
        (".pdf", "pdf"),
        ("docx files", "docx"),
        (".docx", "docx"),
        ("word files", "docx"),
        ("word-dokumente", "docx"),
        ("mp3 files", "mp3"),
        ("mp3s", "mp3"),
        (".mp3", "mp3"),
        ("mp4 files", "mp4"),
        (".mp4", "mp4"),
        ("epub files", "epub"),
        (".epub", "epub"),
        ("markdown files", "md"),
        (".md", "md"),
        ("text files", "txt"),
        (".txt", "txt"),
        ("image files", "jpg"),
        ("bilder", "jpg"),
        ("images", "jpg"),
        ("audio files", "mp3"),
        ("audiodateien", "mp3"),
    ];
    for (phrase, ext) in ext_map {
        if lower.contains(phrase) {
            filters.ext = vec![ext.to_string()];
            cleaned = remove_fragment_ci(&cleaned, phrase);
            lower = cleaned.to_lowercase();
            break;
        }
    }

    // ── Folder prefix ────────────────────────────────────────────────
    // "in /path/to/folder", "in ~/Documents"
    if let Some(pos) = lower.find("in /") {
        let start = pos + 3; // skip "in "
        let rest = &cleaned[start..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let path = rest[..end].to_string();
        if !path.is_empty() {
            filters.parent_dir_prefix = Some(path.clone());
            cleaned = remove_fragment(&cleaned, &format!("in {}", path));
            lower = cleaned.to_lowercase();
        }
    } else if let Some(pos) = lower.find("in ~/") {
        let start = pos + 3;
        let rest = &cleaned[start..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let path = rest[..end].to_string();
        if !path.is_empty() {
            filters.parent_dir_prefix = Some(path.clone());
            cleaned = remove_fragment(&cleaned, &format!("in {}", path));
            lower = cleaned.to_lowercase();
        }
    }

    // ── Tags ─────────────────────────────────────────────────────────
    // "tagged X", "with tag X", "tag:X"
    for prefix in &["tagged ", "with tag ", "tag:"] {
        if let Some(pos) = lower.find(prefix) {
            let start = pos + prefix.len();
            let rest = &cleaned[start..];
            let end = rest
                .find(|c: char| c.is_whitespace())
                .unwrap_or(rest.len());
            let tag = rest[..end].to_string();
            if !tag.is_empty() {
                filters.tag = Some(tag.clone());
                cleaned = remove_fragment_ci(&cleaned, &format!("{}{}", prefix, tag));
                lower = cleaned.to_lowercase();
                break;
            }
        }
    }

    // ── URL domain ───────────────────────────────────────────────────
    // "from spiegel.de", "from example.com"
    if let Some(pos) = lower.find("from ") {
        let start = pos + 5;
        let rest = &cleaned[start..];
        let end = rest
            .find(|c: char| c.is_whitespace())
            .unwrap_or(rest.len());
        let domain = rest[..end].to_string();
        // Heuristic: it's a domain if it contains a dot and no spaces
        if domain.contains('.') && !domain.starts_with('/') {
            filters.url_domain = Some(domain.clone());
            cleaned = remove_fragment(&cleaned, &format!("from {}", domain));
        }
    }

    // Clean up double/triple/etc. spaces left by fragment removal —
    // single-pass instead of O(N) allocations via the while loop.
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    ParsedQuery {
        cleaned_query: cleaned,
        filters,
    }
}

/// Extract a 4-digit year immediately following `prefix` (case-insensitive).
fn extract_year_after_prefix(text: &str, prefix: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let pos = lower.find(prefix)?;
    let start = pos + prefix.len();
    let rest = &text[start..];
    if rest.len() < 4 {
        return None;
    }
    let candidate = &rest[..4];
    let year: i32 = candidate.parse().ok()?;
    if (1900..=2100).contains(&year) {
        Some(year)
    } else {
        None
    }
}

/// Extract a "YYYY-YYYY" or "YYYY–YYYY" year range from text.
fn extract_year_range(text: &str) -> Option<(i32, i32)> {
    // Look for 4 digits, dash/en-dash, 4 digits
    for (i, _) in text.char_indices() {
        if i + 9 > text.len() {
            break;
        }
        let chunk = &text[i..];
        if chunk.len() < 9 {
            continue;
        }
        // Try "YYYY-YYYY"
        if let (Ok(y1), Ok(y2)) = (chunk[..4].parse::<i32>(), chunk[5..9].parse::<i32>()) {
            let sep = chunk.as_bytes()[4];
            if (sep == b'-' || chunk[4..5].starts_with('\u{2013}'))
                && (1900..=2100).contains(&y1)
                && (1900..=2100).contains(&y2)
                && y1 <= y2
            {
                return Some((y1, y2));
            }
        }
    }
    None
}

/// Remove the first occurrence of `fragment` from `text` (case-sensitive).
fn remove_fragment(text: &str, fragment: &str) -> String {
    if let Some(pos) = text.find(fragment) {
        let mut result = String::with_capacity(text.len());
        result.push_str(&text[..pos]);
        result.push_str(&text[pos + fragment.len()..]);
        result
    } else {
        text.to_string()
    }
}

/// Remove the first occurrence of `fragment` from `text` (case-insensitive).
fn remove_fragment_ci(text: &str, fragment: &str) -> String {
    let lower = text.to_lowercase();
    let fragment_lower = fragment.to_lowercase();
    if let Some(pos) = lower.find(&fragment_lower) {
        let mut result = String::with_capacity(text.len());
        result.push_str(&text[..pos]);
        result.push_str(&text[pos + fragment.len()..]);
        result
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_year_min() {
        let p = parse_nl_query("machine learning from 2023");
        assert_eq!(p.filters.year_min, Some(2023));
        assert!(p.cleaned_query.contains("machine learning"));
        assert!(!p.cleaned_query.contains("from 2023"));
    }

    #[test]
    fn extracts_year_range() {
        let p = parse_nl_query("papers 2020-2024");
        assert_eq!(p.filters.year_min, Some(2020));
        assert_eq!(p.filters.year_max, Some(2024));
    }

    #[test]
    fn extracts_language() {
        let p = parse_nl_query("documents in german about physics");
        assert_eq!(p.filters.language.as_deref(), Some("de"));
        assert!(!p.cleaned_query.to_lowercase().contains("in german"));
    }

    #[test]
    fn extracts_file_type() {
        let p = parse_nl_query("find all pdf files about rust");
        assert_eq!(p.filters.ext, vec!["pdf"]);
    }

    #[test]
    fn extracts_tag() {
        let p = parse_nl_query("articles tagged research");
        assert_eq!(p.filters.tag.as_deref(), Some("research"));
    }

    #[test]
    fn extracts_url_domain() {
        let p = parse_nl_query("articles from spiegel.de about politics");
        assert_eq!(p.filters.url_domain.as_deref(), Some("spiegel.de"));
    }

    #[test]
    fn plain_query_passes_through() {
        let p = parse_nl_query("hello world");
        assert_eq!(p.cleaned_query, "hello world");
        assert!(p.filters.year_min.is_none());
        assert!(p.filters.language.is_none());
    }

    #[test]
    fn extracts_folder_prefix() {
        let p = parse_nl_query("notes in /home/user/docs");
        assert_eq!(
            p.filters.parent_dir_prefix.as_deref(),
            Some("/home/user/docs")
        );
    }

    #[test]
    fn extracts_file_type_pdf() {
        // "pdf files" should match the ext_map entry and remove itself from
        // the cleaned query, leaving only the topic word.
        let p = parse_nl_query("pdf files about climate");
        assert_eq!(p.filters.ext, vec!["pdf"], "ext should be [\"pdf\"]: {:?}", p.filters.ext);
        assert!(
            p.cleaned_query.contains("climate"),
            "cleaned_query should still contain 'climate': {:?}", p.cleaned_query
        );
        assert!(
            !p.cleaned_query.to_lowercase().contains("pdf"),
            "pdf should have been stripped from cleaned_query: {:?}", p.cleaned_query
        );
    }

    #[test]
    fn extracts_folder_prefix_absolute() {
        // Dedicated test for an absolute path with trailing words.
        let p = parse_nl_query("in /home/user/docs about taxes");
        assert_eq!(
            p.filters.parent_dir_prefix.as_deref(),
            Some("/home/user/docs"),
            "parent_dir_prefix mismatch: {:?}", p.filters.parent_dir_prefix
        );
    }

    #[test]
    fn mixed_filters() {
        // German language + pdf extension + year_min + tag should all be
        // extracted in a single parse call.  Use "in German" phrasing (ASCII
        // only) to avoid the multi-byte boundary bug in extract_year_range.
        let p = parse_nl_query("in german pdfs from 2023 tagged invoice");
        assert_eq!(
            p.filters.language.as_deref(),
            Some("de"),
            "expected lang=de: {:?}", p.filters.language
        );
        assert_eq!(p.filters.ext, vec!["pdf"], "expected ext=[pdf]: {:?}", p.filters.ext);
        assert_eq!(
            p.filters.year_min,
            Some(2023),
            "expected year_min=2023: {:?}", p.filters.year_min
        );
        assert_eq!(
            p.filters.tag.as_deref(),
            Some("invoice"),
            "expected tag=invoice: {:?}", p.filters.tag
        );
    }

    #[test]
    fn german_language_detection() {
        // "auf Deutsch" (with capital D, as Germans write it) must map to "de".
        // We avoid non-ASCII in the remainder of the query so the extract_year_range
        // helper (which iterates by char_indices and slices by fixed byte offsets)
        // does not encounter multi-byte character boundaries.
        let p = parse_nl_query("auf Deutsch Klimawandel Dokumente");
        assert_eq!(
            p.filters.language.as_deref(),
            Some("de"),
            "expected lang=de for 'auf Deutsch': {:?}", p.filters.language
        );
    }

    #[test]
    fn year_range_inverted_not_extracted() {
        let p = parse_nl_query("docs 2025-2020");
        assert!(p.filters.year_min.is_none(), "inverted range should not be extracted");
        assert!(p.filters.year_max.is_none());
    }

    #[test]
    fn from_domain_and_year_both_extracted() {
        let p = parse_nl_query("articles from spiegel.de since 2023");
        assert_eq!(p.filters.url_domain.as_deref(), Some("spiegel.de"));
        assert_eq!(p.filters.year_min, Some(2023));
    }

    #[test]
    fn empty_query_returns_empty() {
        let p = parse_nl_query("");
        assert_eq!(p.cleaned_query, "");
        assert!(p.filters.year_min.is_none());
        assert!(p.filters.language.is_none());
    }
}
