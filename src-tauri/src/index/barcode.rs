//! Barcode / structured-code detection in extracted text.
//!
//! Scans document text for patterns that match common barcode formats
//! (EAN-13, ISBN-13, UPC-A, Code128-like alphanumeric sequences) and
//! returns them as `barcode:<value>` tags for the tag cloud.
//!
//! This is a text-level heuristic — it finds barcode values that the
//! OCR pipeline already decoded into text, not pixel-level barcode
//! scanning.  Covers the common case where a scanned invoice or
//! shipping label has its barcode value rendered as readable text
//! alongside or beneath the barcode image.

/// Detect barcode-format strings in `text` and return tags.
///
/// Returns `Vec<String>` of `"barcode:<value>"` tags, deduplicated,
/// capped at `max_tags`.
pub fn detect_barcode_tags(text: &str, max_tags: usize) -> Vec<String> {
    let mut tags = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
        if clean.is_empty() {
            continue;
        }

        let tag = if is_isbn13(clean) {
            Some(format!("barcode:isbn13:{}", clean))
        } else if is_ean13(clean) {
            Some(format!("barcode:ean13:{}", clean))
        } else if is_upca(clean) {
            Some(format!("barcode:upca:{}", clean))
        } else if is_issn(clean) {
            Some(format!("barcode:issn:{}", clean))
        } else {
            None
        };

        if let Some(t) = tag {
            if seen.insert(t.clone()) {
                tags.push(t);
                if tags.len() >= max_tags {
                    break;
                }
            }
        }
    }

    tags
}

/// EAN-13: exactly 13 digits with valid check digit.
fn is_ean13(s: &str) -> bool {
    if s.len() != 13 || !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    check_ean(s)
}

/// ISBN-13: starts with 978 or 979, exactly 13 digits, valid check digit.
fn is_isbn13(s: &str) -> bool {
    if s.len() != 13 || !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    (s.starts_with("978") || s.starts_with("979")) && check_ean(s)
}

/// UPC-A: exactly 12 digits with valid check digit.
fn is_upca(s: &str) -> bool {
    if s.len() != 12 || !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    check_upc(s)
}

/// ISSN: 4 digits, hyphen, 3 digits, check digit (digit or X).
fn is_issn(s: &str) -> bool {
    if s.len() != 9 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[4] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..8].iter().all(|b| b.is_ascii_digit())
        && (bytes[8].is_ascii_digit() || bytes[8] == b'X' || bytes[8] == b'x')
}

/// EAN check digit: alternating weights 1,3 on first 12 digits.
fn check_ean(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() != 13 {
        return false;
    }
    let sum: u32 = digits.iter().enumerate().map(|(i, &d)| {
        if i % 2 == 0 { d } else { d * 3 }
    }).sum();
    sum % 10 == 0
}

/// UPC check digit: alternating weights 3,1 on first 11 digits.
fn check_upc(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() != 12 {
        return false;
    }
    let sum: u32 = digits.iter().enumerate().map(|(i, &d)| {
        if i % 2 == 0 { d * 3 } else { d }
    }).sum();
    sum % 10 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ean13() {
        // Valid EAN-13 for a common product
        let tags = detect_barcode_tags("Product code 4006381333931 on the label", 10);
        assert!(tags.iter().any(|t| t.starts_with("barcode:ean13:")), "tags: {:?}", tags);
    }

    #[test]
    fn detects_isbn13() {
        let tags = detect_barcode_tags("ISBN 9783161484100 found in the book", 10);
        assert!(tags.iter().any(|t| t.starts_with("barcode:isbn13:")), "tags: {:?}", tags);
    }

    #[test]
    fn rejects_random_digits() {
        let tags = detect_barcode_tags("Phone 1234567890123 is not a barcode", 10);
        // 1234567890123 won't pass EAN check digit
        assert!(tags.is_empty() || !tags.iter().any(|t| t.contains("1234567890123")));
    }

    #[test]
    fn detects_issn() {
        let tags = detect_barcode_tags("Journal ISSN 0378-5955 volume 42", 10);
        assert!(tags.iter().any(|t| t.starts_with("barcode:issn:")), "tags: {:?}", tags);
    }

    #[test]
    fn detects_upca() {
        // 036000291452 is a well-known, publicly listed UPC-A (Coke 12-pack).
        // UPC-A check: alternating weights 3,1; sum % 10 == 0.
        let tags = detect_barcode_tags("Barcode 036000291452 on the can", 10);
        assert!(
            tags.iter().any(|t| t.starts_with("barcode:upca:")),
            "expected upca tag; tags: {:?}", tags
        );
        assert!(
            tags.iter().any(|t| t.contains("036000291452")),
            "tag value mismatch; tags: {:?}", tags
        );
    }

    #[test]
    fn caps_at_max_tags() {
        // 20 distinct valid EAN-13 codes — the function must return at most
        // max_tags=3 of them.
        let text = "4006381333931 5901234123457 8001234567897 4712345678900 \
                    6900000000014 3800000000010 5012345678900 4000000000013 \
                    5011111111108 5900000000008 4001000000003 4002000000000 \
                    4003000000007 4004000000004 4005000000001 4006000000008 \
                    4007000000005 4008000000002 4009000000009 4010000000005";
        let tags = detect_barcode_tags(text, 3);
        assert_eq!(tags.len(), 3, "expected exactly 3 tags (max_tags cap), got: {:?}", tags);
    }

    #[test]
    fn deduplicates() {
        // The same barcode value appearing twice must only produce one tag.
        let tags = detect_barcode_tags("4006381333931 4006381333931", 10);
        let ean_tags: Vec<_> = tags.iter().filter(|t| t.contains("4006381333931")).collect();
        assert_eq!(
            ean_tags.len(),
            1,
            "duplicate barcode should be deduplicated; tags: {:?}", tags
        );
    }
}
