//! RSS/Atom feed parser → document extraction (P24.5).
//!
//! Parses an RSS or Atom feed (from raw bytes), yields one
//! `FeedEntry` per item with title, author, date, body text,
//! and source URL.  The caller can ingest each entry as a document.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FeedEntry {
    pub title: String,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub date: Option<String>,
    pub body: String,
    pub url: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParsedFeed {
    pub feed_title: Option<String>,
    pub feed_url: Option<String>,
    pub entries: Vec<FeedEntry>,
}

/// Parse raw XML bytes (RSS 2.0 / Atom / JSON Feed) into a structured
/// feed with entries.
pub fn parse_feed(data: &[u8]) -> Result<ParsedFeed, String> {
    let feed = feed_rs::parser::parse(data)
        .map_err(|e| format!("feed parse error: {e}"))?;

    let feed_title = feed.title.map(|t| t.content);
    let feed_url = feed.links.first().map(|l| l.href.clone());

    let mut entries = Vec::with_capacity(feed.entries.len());
    for entry in &feed.entries {
        let title = entry.title.as_ref().map(|t| t.content.clone()).unwrap_or_default();

        let author = entry.authors.first().map(|a| a.name.clone())
            .or_else(|| feed.authors.first().map(|a| a.name.clone()));

        let (year, date) = entry.published
            .or(entry.updated)
            .map(|dt| {
                let d = dt.to_rfc3339();
                // Extract year from RFC 3339 string (first 4 chars)
                let y = d[..4].parse::<i32>().ok();
                (y, Some(d))
            })
            .unwrap_or((None, None));

        // Body: prefer content, then summary, then description
        let body = entry.content.as_ref()
            .and_then(|c| c.body.as_ref().or(c.src.as_ref().map(|s| &s.href)))
            .cloned()
            .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()))
            .unwrap_or_default();

        // Strip HTML tags for plain text
        let body_text = strip_html(&body);

        let url = entry.links.first().map(|l| l.href.clone())
            .or_else(|| entry.id.clone().into());

        let tags: Vec<String> = entry.categories.iter()
            .filter_map(|c| {
                let label = c.label.as_deref().or(Some(&c.term));
                label.map(|s| s.to_string())
            })
            .collect();

        entries.push(FeedEntry {
            title,
            author,
            year,
            date,
            body: body_text,
            url: Some(url.unwrap_or_default()).filter(|s| !s.is_empty()),
            tags,
        });
    }

    Ok(ParsedFeed { feed_title, feed_url, entries })
}

/// Parse a feed from a URL by fetching it first.
pub async fn fetch_and_parse(url: &str) -> Result<ParsedFeed, String> {
    let resp = reqwest::get(url).await.map_err(|e| format!("fetch {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} from {url}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read body: {e}"))?;
    parse_feed(&bytes)
}

/// Rough HTML tag stripper (no external dep — pure regex replacement).
fn strip_html(html: &str) -> String {
    // Remove <script> and <style> blocks
    let no_script = html
        .split("<script")
        .enumerate()
        .map(|(i, part)| {
            if i == 0 { part.to_string() }
            else { part.splitn(2, "</script>").last().unwrap_or("").to_string() }
        })
        .collect::<String>();
    let no_style = no_script
        .split("<style")
        .enumerate()
        .map(|(i, part)| {
            if i == 0 { part.to_string() }
            else { part.splitn(2, "</style>").last().unwrap_or("").to_string() }
        })
        .collect::<String>();

    // Strip remaining tags
    let mut result = String::with_capacity(no_style.len());
    let mut in_tag = false;
    for ch in no_style.chars() {
        if ch == '<' { in_tag = true; continue; }
        if ch == '>' { in_tag = false; result.push(' '); continue; }
        if !in_tag { result.push(ch); }
    }

    // Decode common entities
    result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Collapse whitespace
    let mut prev_space = false;
    result.retain(|c| {
        if c.is_whitespace() {
            if prev_space { return false; }
            prev_space = true;
        } else {
            prev_space = false;
        }
        true
    });
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn strip_html_entities() {
        assert_eq!(strip_html("foo &amp; bar"), "foo & bar");
    }

    #[test]
    fn parse_rss2() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0">
          <channel>
            <title>Test Feed</title>
            <item>
              <title>Article One</title>
              <link>https://example.com/1</link>
              <description>Hello world</description>
              <category>tech</category>
            </item>
          </channel>
        </rss>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(feed.feed_title.as_deref(), Some("Test Feed"));
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].title, "Article One");
        assert_eq!(feed.entries[0].body, "Hello world");
        assert_eq!(feed.entries[0].tags, vec!["tech"]);
    }

    #[test]
    fn parse_atom() {
        let xml = r#"<?xml version="1.0"?>
        <feed xmlns="http://www.w3.org/2005/Atom">
          <title>Atom Feed</title>
          <entry>
            <title>Entry</title>
            <link href="https://example.com/e"/>
            <summary>Summary text</summary>
          </entry>
        </feed>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(feed.feed_title.as_deref(), Some("Atom Feed"));
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].body, "Summary text");
    }

    #[test]
    fn parse_rss2_multiple_entries() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>T</title>
          <item><title>A</title><description>aaa</description></item>
          <item><title>B</title><description>bbb</description></item>
          <item><title>C</title><description>ccc</description></item>
        </channel></rss>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(feed.entries.len(), 3);
        assert_eq!(feed.entries[0].title, "A");
        assert_eq!(feed.entries[2].title, "C");
    }

    #[test]
    fn parse_rss2_with_html_body() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>T</title>
          <item><title>Article</title>
            <description>&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;</description>
          </item>
        </channel></rss>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(feed.entries[0].body, "Hello world");
    }

    #[test]
    fn parse_rss2_missing_optional_fields() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>T</title>
          <item><title>Minimal</title></item>
        </channel></rss>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(feed.entries.len(), 1);
        assert!(feed.entries[0].author.is_none());
        assert!(feed.entries[0].year.is_none());
        assert!(feed.entries[0].body.is_empty());
    }

    #[test]
    fn parse_malformed_xml_fails() {
        let result = parse_feed(b"not xml at all {{{");
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_feed() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>Empty</title></channel></rss>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert!(feed.entries.is_empty());
        assert_eq!(feed.feed_title.as_deref(), Some("Empty"));
    }

    #[test]
    fn strip_html_script_tags() {
        let html = "before<script>evil();</script>after";
        assert_eq!(strip_html(html), "beforeafter");
    }

    #[test]
    fn strip_html_style_tags() {
        let html = "before<style>.red{color:red}</style>after";
        assert_eq!(strip_html(html), "beforeafter");
    }

    #[test]
    fn strip_html_collapses_whitespace() {
        assert_eq!(strip_html("hello    world   foo"), "hello world foo");
    }

    #[test]
    fn strip_html_numeric_entities() {
        // We don't handle &#123; yet — just verify it doesn't crash
        let result = strip_html("&#65;&#66;");
        assert!(!result.is_empty());
    }
}
