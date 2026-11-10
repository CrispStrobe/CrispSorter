//! Bounded RFC 5322 / MIME email extraction.
//!
//! Decode transfer encodings, encoded headers and declared character sets;
//! prefer the text alternative, retain text attachments and forwarded messages,
//! and keep binary attachment names explicitly separate from body coverage.
//! No attachment is executed and no remote content is fetched.

use anyhow::{ensure, Context, Result};
use mail_parser::{HeaderForm, HeaderName, MessageParser, PartFlags, PartKind};
use std::path::Path;

use super::ExtractedDocument;

const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MAILBOX_BYTES: usize = 256 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_MAILBOX_MESSAGES: usize = 10_000;
const MAX_MIME_PARTS: usize = 2048;
const MAX_MIME_DEPTH: usize = 32;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    let bytes = super::html::read_bounded(path, MAX_MESSAGE_BYTES)?;
    parse_message(&bytes)
}

fn parser() -> MessageParser {
    MessageParser::new()
        .header(HeaderName::From, HeaderForm::Text)
        .header(HeaderName::To, HeaderForm::Text)
        .header(HeaderName::Cc, HeaderForm::Text)
        .header(HeaderName::Date, HeaderForm::Text)
        .header(HeaderName::ListId, HeaderForm::Text)
        .max_depth(MAX_MIME_DEPTH)
        .max_parts(MAX_MIME_PARTS)
        .max_encoded_nesting(3)
}

fn append(output: &mut String, text: &str) -> Result<()> {
    ensure!(
        text.len() <= MAX_TEXT_BYTES.saturating_sub(output.len()),
        "Email extracted text exceeds the 16 MiB limit"
    );
    output.push_str(text);
    Ok(())
}

fn heading(doc: &mut ExtractedDocument, name: &str, value: &str) -> Result<()> {
    ensure!(
        value.len() <= MAX_HEADER_BYTES,
        "Email header exceeds the 64 KiB limit"
    );
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if !value.is_empty() {
        let line = format!("{name}: {value}");
        append(&mut doc.full_text, &line)?;
        append(&mut doc.full_text, "\n")?;
        doc.headings.push(line);
    }
    Ok(())
}

fn part_text(part: mail_parser::MessagePart<'_>) -> Result<String> {
    ensure!(
        part.decoded_len() <= MAX_TEXT_BYTES,
        "Decoded email part exceeds the 16 MiB limit"
    );
    let Some((text, problems)) = part.text_checked() else {
        return Ok(String::new());
    };
    ensure!(
        problems.is_empty(),
        "Malformed email text encoding: {}",
        problems.names().collect::<Vec<_>>().join(", ")
    );
    ensure!(
        text.len() <= MAX_TEXT_BYTES,
        "Decoded email text exceeds the 16 MiB limit"
    );
    if matches!(part.kind(), PartKind::Html) {
        super::html::visible_html(&text)
    } else {
        Ok(text.into_owned())
    }
}

fn parse_message(raw: &[u8]) -> Result<ExtractedDocument> {
    ensure!(
        raw.len() <= MAX_MESSAGE_BYTES,
        "Email source exceeds the 64 MiB limit"
    );
    let mut doc = ExtractedDocument {
        ext: "eml".into(),
        ..Default::default()
    };
    let Some(message) = parser().parse(raw) else {
        append(&mut doc.full_text, &String::from_utf8_lossy(raw))?;
        return Ok(doc);
    };
    // Headerless files remain useful plain text rather than an empty successful
    // extraction. A declared text/plain body retains literal markup verbatim.
    if message.headers().is_empty() {
        append(&mut doc.full_text, &String::from_utf8_lossy(raw))?;
        return Ok(doc);
    }
    for part in message.parts() {
        ensure!(
            !part
                .flags()
                .intersects(PartFlags::LIMIT_REACHED | PartFlags::NESTING_LIMIT),
            "Email MIME structure exceeds the depth, part-count or encoded-message limit"
        );
        if part.flags().intersects(
            PartFlags::MISSING_DELIMITER | PartFlags::UNTERMINATED | PartFlags::FALLBACK_DELIMITER,
        ) {
            doc.tags.push("extraction:email-structure-warning".into());
        }
    }

    // The parser exposes nested messages as a flat sequence. Traversal is
    // bounded by the parser's part/depth limits and does not recurse here.
    for (index, item) in message.messages().enumerate() {
        if index > 0 {
            append(&mut doc.full_text, "\nForwarded message\n")?;
        }
        let headers = item.headers();
        for (name, label) in [
            (HeaderName::Subject, "Subject"),
            (HeaderName::From, "From"),
            (HeaderName::To, "To"),
            (HeaderName::Cc, "Cc"),
            (HeaderName::Date, "Date"),
        ] {
            if let Some(value) = headers.value(name).and_then(|v| v.as_text()) {
                heading(&mut doc, label, value)?;
            }
        }
        if let Some(value) = headers.value(HeaderName::ListId).and_then(|v| v.as_text()) {
            ensure!(
                value.len() <= MAX_HEADER_BYTES,
                "Email list ID exceeds the header limit"
            );
            doc.tags
                .push(format!("list:{}", value.trim().trim_matches(['<', '>'])));
        }
        append(&mut doc.full_text, "\n")?;
        // RFC 8621 body lists prefer plain text for multipart/alternative and
        // include an HTML part only when that segment has no plain alternative.
        for part in item.text_body() {
            let text = part_text(part)?;
            append(&mut doc.full_text, &text)?;
            append(&mut doc.full_text, "\n")?;
        }
        for part in item.attachments() {
            if matches!(part.kind(), PartKind::Message(_)) {
                continue;
            }
            let name = part.attachment_name().unwrap_or("unnamed attachment");
            heading(&mut doc, "Attachment", name)?;
            if part.is_text() {
                let text = part_text(part)?;
                append(&mut doc.full_text, &text)?;
                append(&mut doc.full_text, "\n")?;
            } else {
                // Binary bodies are not decoded or represented as searchable
                // text. The tag makes partial attachment coverage inspectable.
                doc.tags.push("extraction:attachment-name-only".into());
            }
        }
    }
    doc.tags.sort();
    doc.tags.dedup();
    Ok(doc)
}

/// Merge an mbox into one document while preserving message boundaries and
/// header provenance. Standard escaped From-lines are unquoted once. Errors
/// abort the extraction rather than silently skipping a message.
pub fn extract_mbox(path: &Path) -> Result<ExtractedDocument> {
    let raw = super::html::read_bounded(path, MAX_MAILBOX_BYTES)?;
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    if !raw.starts_with(b"From ") {
        let mut doc = parse_message(raw)?;
        doc.ext = "mbox".into();
        return Ok(doc);
    }
    let mut doc = ExtractedDocument {
        ext: "mbox".into(),
        ..Default::default()
    };
    for (index, message) in mail_parser::mailbox::mbox::MessageIterator::new(raw).enumerate() {
        ensure!(
            index < MAX_MAILBOX_MESSAGES,
            "Mailbox exceeds the 10,000 message limit"
        );
        let message = message.with_context(|| format!("reading mailbox message {}", index + 1))?;
        let parsed = parse_message(message.contents())
            .with_context(|| format!("extracting mailbox message {}", index + 1))?;
        append(
            &mut doc.full_text,
            &format!("\n── Message {} ──\n", index + 1),
        )?;
        append(&mut doc.full_text, &parsed.full_text)?;
        doc.headings.extend(parsed.headings);
        doc.tags.extend(parsed.tags);
    }
    doc.tags.sort();
    doc.tags.dedup();
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_headers_and_plain_text() {
        let doc = parse_message(b"From: Alice <a@example.test>\r\nTo: Bob <b@example.test>\r\nSubject: Hello\r\nDate: Thu, 15 Nov 2024 10:30:00 +0100\r\n\r\nLiteral <mark>source</mark> text.").unwrap();
        assert!(doc.full_text.contains("Literal <mark>source</mark> text."));
        assert!(doc.headings.contains(&"Subject: Hello".into()));
        assert!(doc.headings.iter().any(|s| s.contains("2024")));
    }

    #[test]
    fn decodes_folded_encoded_headers_and_quoted_printable_charset() {
        let doc = parse_message(b"Subject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\n =?UTF-8?Q?_aus_Berlin?=\r\nFrom: =?UTF-8?Q?J=C3=B6rg?= <a@example.test>\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\ncaf=E9 =\r\ncontinued").unwrap();
        assert!(doc.headings.contains(&"Subject: Grüße aus Berlin".into()));
        assert!(doc.full_text.contains("Jörg"));
        assert!(doc.full_text.contains("café continued"));
    }

    #[test]
    fn decodes_base64_multilingual_body() {
        let doc = parse_message(b"Subject: Unicode\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n5p2x5LqsIGNhZsOp").unwrap();
        assert!(doc.full_text.contains("東京 café"));
        assert!(!doc.full_text.contains("5p2x"));
    }

    #[test]
    fn prefers_plain_alternative_using_the_outer_header_boundary() {
        let doc = parse_message(b"Subject: Alternative\nContent-Type: multipart/alternative; boundary=\"boundary--42\"\n\nPreamble\n--boundary--42\nContent-Type: text/plain\n\nplain evidence -- stays\n--boundary--42\nContent-Type: text/html\n\n<p>html duplicate</p>\n--boundary--42--\n").unwrap();
        assert!(doc.full_text.contains("plain evidence -- stays"));
        assert!(!doc.full_text.contains("html duplicate"));
        assert!(!doc.full_text.contains("Preamble"));
        assert!(!doc.full_text.contains("boundary--42"));
    }

    #[test]
    fn html_only_email_uses_visible_decoded_text() {
        let doc = parse_message(b"Subject: HTML\nContent-Type: text/html; charset=utf-8\n\n<p>A &amp; B</p><script>invisible()</script><style>hidden</style><p>C</p>").unwrap();
        assert!(doc.full_text.contains("A & B C"));
        assert!(!doc.full_text.contains("invisible"));
        assert!(!doc.full_text.contains("<p>"));
    }

    #[test]
    fn indexes_text_attachments_and_names_binary_coverage_explicitly() {
        let doc = parse_message(b"Subject: Mixed\nContent-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nBody\n--b\nContent-Type: text/plain; charset=utf-8\nContent-Disposition: attachment; filename*=utf-8''notes%20caf%C3%A9.txt\nContent-Transfer-Encoding: base64\n\nYXR0YWNobWVudCBldmlkZW5jZQ==\n--b\nContent-Type: application/octet-stream\nContent-Disposition: attachment; filename=\"report.bin\"\nContent-Transfer-Encoding: base64\n\nbm90IGJvZHkgdGV4dA==\n--b--\n").unwrap();
        assert!(doc.full_text.contains("Attachment: notes café.txt"));
        assert!(doc.full_text.contains("attachment evidence"));
        assert!(doc.full_text.contains("Attachment: report.bin"));
        assert!(!doc.full_text.contains("not body text"));
        assert!(!doc.full_text.contains("bm90"));
        assert!(doc.tags.contains(&"extraction:attachment-name-only".into()));
    }

    #[test]
    fn includes_forwarded_message_headers_and_body_once() {
        let doc = parse_message(b"Subject: Outer\nContent-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nouter body\n--b\nContent-Type: message/rfc822\n\nSubject: Forwarded\nFrom: nested@example.test\n\nunique forwarded evidence\n--b--\n").unwrap();
        assert!(doc.full_text.contains("Subject: Forwarded"));
        assert_eq!(
            doc.full_text.matches("unique forwarded evidence").count(),
            1
        );
        assert!(doc.full_text.contains("outer body"));
    }

    #[test]
    fn rejects_malformed_body_encoding_instead_of_reporting_complete_text() {
        let result = parse_message(
            b"Subject: Broken\nContent-Type: text/plain\nContent-Transfer-Encoding: base64\n\n!!!!",
        );
        assert!(result.unwrap_err().to_string().contains("Malformed email"));
    }

    #[test]
    fn preserves_headerless_text_and_empty_mail() {
        assert!(parse_message(b"Just plain text.")
            .unwrap()
            .full_text
            .contains("Just plain text."));
        assert!(parse_message(b"Subject: Empty\n\n")
            .unwrap()
            .full_text
            .contains("Subject: Empty"));
        assert!(parse_message(b"").unwrap().full_text.is_empty());
    }

    #[test]
    fn list_id_remains_a_searchable_tag() {
        let doc = parse_message(b"Subject: Digest\nList-Id: <news.example.test>\n\nHello").unwrap();
        assert!(doc.tags.contains(&"list:news.example.test".into()));
    }

    #[test]
    fn malformed_boundary_is_visible_in_extraction_tags() {
        let doc = parse_message(b"Subject: Incomplete\nContent-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nreadable body").unwrap();
        assert!(doc.full_text.contains("readable body"));
        assert!(doc
            .tags
            .contains(&"extraction:email-structure-warning".into()));
    }

    #[test]
    fn enforces_mime_depth_before_accepting_a_partial_document() {
        let mut raw = String::from("Subject: Nested\n");
        for depth in 0..40 {
            raw.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=b{depth}\n\n--b{depth}\n"
            ));
        }
        raw.push_str("Content-Type: text/plain\n\nDeep\n");
        for depth in (0..40).rev() {
            raw.push_str(&format!("--b{depth}--\n"));
        }
        assert!(parse_message(raw.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("limit"));
    }

    #[test]
    fn mbox_preserves_escaped_from_lines_and_repeated_message_headers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.mbox");
        std::fs::write(&path, "From a@example.test Mon Jan 1 00:00:00 2024\nSubject: Repeated\n\n>From the start\n>>From quoted\n\nFrom a@example.test Mon Jan 1 00:00:00 2024\nSubject: Repeated\n\nSecond body\n").unwrap();
        let doc = extract_mbox(&path).unwrap();
        assert!(doc.full_text.contains("From the start\n>From quoted"));
        assert!(doc.full_text.contains("Second body"));
        assert_eq!(doc.full_text.matches("Subject: Repeated").count(), 2);
        assert_eq!(doc.full_text.matches("── Message").count(), 2);
    }

    #[test]
    fn mbox_reports_the_failed_message_instead_of_skipping_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.mbox");
        std::fs::write(&path, "From a@example.test Mon Jan 1 00:00:00 2024\nSubject: Good\n\nOne\nFrom b@example.test Mon Jan 1 00:00:00 2024\nSubject: Bad\nContent-Transfer-Encoding: base64\n\n!!!!").unwrap();
        assert!(extract_mbox(&path)
            .unwrap_err()
            .to_string()
            .contains("message 2"));
    }
}
