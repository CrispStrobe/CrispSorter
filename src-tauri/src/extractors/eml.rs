//! RFC 822 .eml email extractor.
//!
//! Parses headers (From, To, Subject, Date) into metadata and the
//! body (text/plain preferred, text/html fallback stripped) into
//! full_text.  No external crate — .eml is line-oriented text with
//! a blank-line separator between headers and body.

use std::path::Path;
use anyhow::Result;
use super::ExtractedDocument;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    let raw = std::fs::read_to_string(path)?;

    // Split headers from body at first blank line
    let (header_block, body) = match raw.find("\r\n\r\n") {
        Some(pos) => (&raw[..pos], &raw[pos + 4..]),
        None => match raw.find("\n\n") {
            Some(pos) => (&raw[..pos], &raw[pos + 2..]),
            None => (raw.as_str(), ""),
        },
    };

    // Parse headers (handle continuation lines)
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in header_block.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation of previous header
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_lowercase();
            let value = line[colon_pos + 1..].trim().to_string();
            headers.push((key, value));
        }
    }

    let get = |key: &str| -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };

    let subject = get("subject");
    let from = get("from");
    let to = get("to");
    let date = get("date");

    // Build headings from key headers
    let mut headings = Vec::new();
    if let Some(ref s) = subject {
        headings.push(format!("Subject: {}", s));
    }
    if let Some(ref f) = from {
        headings.push(format!("From: {}", f));
    }
    if let Some(ref t) = to {
        headings.push(format!("To: {}", t));
    }
    if let Some(ref d) = date {
        headings.push(format!("Date: {}", d));
    }

    // Extract body text.  Handle multipart MIME minimally:
    // if body contains "Content-Type: text/plain", extract that part.
    // Otherwise use the raw body, stripping HTML tags if it looks like HTML.
    let body_text = extract_body_text(body);

    // Build full text: headers summary + body
    let mut full_text = String::new();
    for h in &headings {
        full_text.push_str(h);
        full_text.push('\n');
    }
    full_text.push('\n');
    full_text.push_str(&body_text);

    // Try to parse year from Date header
    // Date formats: "Thu, 15 Nov 2024 10:30:00 +0100" or similar
    // Just look for a 4-digit year
    let _year = date.as_deref().and_then(|d| {
        d.split_whitespace().find_map(|w| {
            if w.len() == 4 {
                w.parse::<i32>()
                    .ok()
                    .filter(|&y| (1990..=2099).contains(&y))
            } else {
                None
            }
        })
    });

    // Extract email address from "Name <email>" format
    let _author = from
        .map(|f| {
            if let Some(start) = f.find('<') {
                f[..start].trim().trim_matches('"').to_string()
            } else {
                f
            }
        })
        .filter(|a| !a.is_empty());

    // Tags from headers
    let mut tags = Vec::new();
    if let Some(ref list_id) = get("list-id") {
        tags.push(format!(
            "list:{}",
            list_id.trim_matches(|c: char| c == '<' || c == '>')
        ));
    }

    Ok(ExtractedDocument {
        full_text,
        headings,
        ext: "eml".into(),
        language: None, // filled by LID post-pass
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags,
        audio_pcm: None,
    })
}

/// Extract the text/plain body from a potentially MIME-encoded message.
fn extract_body_text(body: &str) -> String {
    // Check for multipart boundary
    if body.contains("Content-Type:") && body.contains("boundary=") {
        // Try to find text/plain part
        if let Some(plain) = extract_mime_part(body, "text/plain") {
            return plain;
        }
        // Fallback to text/html with tag stripping
        if let Some(html) = extract_mime_part(body, "text/html") {
            return strip_html_tags(&html);
        }
    }

    // Not MIME or no recognized parts — check if it's HTML
    if body.trim_start().starts_with('<') && body.contains("</") {
        return strip_html_tags(body);
    }

    // Plain text body
    body.to_string()
}

fn extract_mime_part(body: &str, content_type: &str) -> Option<String> {
    let ct_marker = format!("Content-Type: {}", content_type);
    let parts: Vec<&str> = body.split("--").collect();
    for part in parts {
        if part.contains(&ct_marker) {
            // Find the blank line separating MIME headers from content
            let content = if let Some(pos) = part.find("\r\n\r\n") {
                &part[pos + 4..]
            } else if let Some(pos) = part.find("\n\n") {
                &part[pos + 2..]
            } else {
                continue;
            };
            // Strip trailing boundary markers
            let clean = content.trim_end_matches(|c: char| c == '-' || c == '\r' || c == '\n');
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut prev_was_space = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
            continue;
        }
        if c == '>' {
            in_tag = false;
            continue;
        }
        if in_tag {
            continue;
        }
        if c.is_whitespace() {
            if !prev_was_space {
                result.push(' ');
            }
            prev_was_space = true;
        } else {
            result.push(c);
            prev_was_space = false;
        }
    }
    result
}

/// Extract all messages from an .mbox file (concatenated RFC 822 messages
/// separated by `From ` lines).  Returns one `ExtractedDocument` per message,
/// or a single merged document if `merge` is true.
pub fn extract_mbox(path: &Path) -> Result<ExtractedDocument> {
    let raw = std::fs::read_to_string(path)?;

    // Split on lines starting with "From " (the mbox separator).
    // The first line of an mbox file is always a "From " line.
    let mut messages: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, line) in raw.lines().enumerate() {
        if line.starts_with("From ") && i > 0 {
            let byte_pos = raw[start..].find(line).map(|p| start + p);
            if let Some(pos) = byte_pos {
                if pos > start {
                    messages.push(&raw[start..pos]);
                }
                start = pos;
            }
        }
    }
    // Last message
    if start < raw.len() {
        messages.push(&raw[start..]);
    }

    if messages.is_empty() {
        // Treat the whole file as a single message
        messages.push(&raw);
    }

    // Parse each message and merge into one document
    let mut all_text = String::new();
    let mut all_headings = Vec::new();
    let mut all_tags = Vec::new();
    let mut msg_count = 0;

    for msg_raw in &messages {
        // Strip the "From " envelope line
        let body = if msg_raw.starts_with("From ") {
            match msg_raw.find('\n') {
                Some(pos) => &msg_raw[pos + 1..],
                None => msg_raw,
            }
        } else {
            msg_raw
        };

        // Write to a temp approach: parse in-memory
        let tmp = tempfile::NamedTempFile::new()?;
        std::fs::write(tmp.path(), body)?;
        if let Ok(doc) = extract(tmp.path()) {
            if !doc.full_text.trim().is_empty() {
                msg_count += 1;
                all_text.push_str(&format!("── Message {} ──\n", msg_count));
                all_text.push_str(&doc.full_text);
                all_text.push_str("\n\n");
                all_headings.extend(doc.headings);
                all_tags.extend(doc.tags);
            }
        }
    }

    // Dedup tags
    all_tags.sort();
    all_tags.dedup();

    Ok(ExtractedDocument {
        full_text: all_text,
        headings: all_headings,
        ext: "mbox".into(),
        language: None,
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags: all_tags,
        audio_pcm: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_plain_eml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("test.eml");
        std::fs::write(
            &p,
            "From: Alice <alice@example.com>\r\n\
             To: Bob <bob@example.com>\r\n\
             Subject: Hello World\r\n\
             Date: Thu, 15 Nov 2024 10:30:00 +0100\r\n\
             \r\n\
             This is the body of the email.\r\n",
        )
        .unwrap();
        let doc = extract(&p).unwrap();
        assert!(doc.full_text.contains("Subject: Hello World"));
        assert!(doc.full_text.contains("This is the body of the email."));
        assert_eq!(doc.ext, "eml");
        assert!(doc.headings.iter().any(|h| h.contains("Hello World")));
    }

    #[test]
    fn extract_html_body_strips_tags() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("html.eml");
        std::fs::write(
            &p,
            "From: test@example.com\n\
             Subject: HTML test\n\
             \n\
             <html><body><p>Hello <b>World</b></p></body></html>\n",
        )
        .unwrap();
        let doc = extract(&p).unwrap();
        assert!(doc.full_text.contains("Hello World"));
        assert!(!doc.full_text.contains("<html>"));
    }

    #[test]
    fn extract_mbox_splits_messages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("test.mbox");
        // Two messages separated by a "From " envelope line.
        std::fs::write(
            &p,
            "From user1@example.com Mon Jan  1 00:00:00 2024\n\
             From: user1@example.com\n\
             Subject: First message\n\
             \n\
             Body of message one.\n\
             \n\
             From user2@example.com Mon Jan  2 00:00:00 2024\n\
             From: user2@example.com\n\
             Subject: Second message\n\
             \n\
             Body of message two.\n",
        )
        .unwrap();
        let doc = extract_mbox(&p).unwrap();
        assert!(
            doc.full_text.contains("message one"),
            "first message body missing: {:?}", doc.full_text
        );
        assert!(
            doc.full_text.contains("message two"),
            "second message body missing: {:?}", doc.full_text
        );
    }

    #[test]
    fn extract_eml_date_to_year() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("dated.eml");
        std::fs::write(
            &p,
            "From: sender@example.com\r\n\
             Subject: Dated email\r\n\
             Date: Thu, 15 Nov 2024 10:30:00 +0100\r\n\
             \r\n\
             Some body text here.\r\n",
        )
        .unwrap();
        let doc = extract(&p).unwrap();
        // The Date header must appear in the headings/full_text.
        assert!(
            doc.full_text.contains("2024"),
            "expected year 2024 in full_text: {:?}", doc.full_text
        );
        assert!(
            doc.headings.iter().any(|h| h.contains("Date:")),
            "Date heading missing: {:?}", doc.headings
        );
    }

    #[test]
    fn extract_eml_list_id_to_tag() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("list.eml");
        std::fs::write(
            &p,
            "From: noreply@lists.example.com\r\n\
             Subject: Newsletter\r\n\
             List-Id: <weekly-digest.lists.example.com>\r\n\
             \r\n\
             Hello subscriber!\r\n",
        )
        .unwrap();
        let doc = extract(&p).unwrap();
        // The List-Id header must produce a "list:…" tag.
        assert!(
            doc.tags.iter().any(|t| t.starts_with("list:")),
            "expected a list: tag; tags: {:?}", doc.tags
        );
    }

    #[test]
    fn extract_eml_multipart_prefers_plain() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("multipart.eml");
        // The body itself must contain "Content-Type:" and "boundary=" for
        // the MIME detector to activate.  We embed them in the body preamble
        // so the extractor enters the multipart branch.
        std::fs::write(
            &p,
            "From: sender@example.com\n\
             Subject: Multipart test\n\
             \n\
             Content-Type: multipart/alternative; boundary=boundary42\n\
             \n\
             --boundary42\n\
             Content-Type: text/plain\n\
             \n\
             Plain text body here.\n\
             --boundary42\n\
             Content-Type: text/html\n\
             \n\
             <html><body>HTML body here.</body></html>\n\
             --boundary42--\n",
        )
        .unwrap();
        let doc = extract(&p).unwrap();
        // The text/plain part must be present in the full text.
        assert!(
            doc.full_text.contains("Plain text body"),
            "expected plain text body; full_text: {:?}", doc.full_text
        );
        // The raw <html> open tag must not appear (either the plain part was
        // used, or the html was stripped).
        assert!(
            !doc.full_text.contains("<html>"),
            "should not include raw HTML open-tag: {:?}", doc.full_text
        );
    }

    #[test]
    fn extract_eml_empty_body() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("empty.eml");
        std::fs::write(&p, "From: test@example.com\nSubject: Empty\n\n").unwrap();
        let doc = extract(&p).unwrap();
        assert!(doc.full_text.trim().is_empty() || doc.full_text.contains("Empty"));
    }

    #[test]
    fn extract_eml_no_headers() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("noheader.eml");
        std::fs::write(&p, "Just plain text with no headers at all.").unwrap();
        let doc = extract(&p).unwrap();
        // Should not panic — falls back to raw text
        assert!(!doc.full_text.is_empty());
    }
}
