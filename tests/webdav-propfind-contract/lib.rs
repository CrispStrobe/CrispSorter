#![cfg(test)]
//! Compile the production parser with the same quick-xml version as the app.
use anyhow::{anyhow, Result};
use quick_xml::{events::Event, Reader};
include!(concat!(env!("OUT_DIR"), "/webdav_propfind.rs"));
include!(concat!(env!("OUT_DIR"), "/server_fixtures.rs"));

fn response(href: &str, displayname: &str) -> String {
    format!(
        r#"<d:multistatus xmlns:d="DAV:"><d:response>
      <d:href>{href}</d:href><d:propstat><d:prop>
      <d:displayname>{displayname}</d:displayname>
      <d:getcontentlength> 42 </d:getcontentlength>
      <d:resourcetype><d:collection/></d:resourcetype>
      </d:prop></d:propstat></d:response></d:multistatus>"#
    )
}

#[test]
fn propfind_decodes_entities_without_losing_text_or_spaces() {
    for amp in ["&amp;", "&#38;", "&#x26;"] {
        let xml = response(
            &format!("/dav/Sales{amp}Marketing.txt"),
            &format!("Sales {amp} Marketing &lt;Q4&gt; &quot;plan&quot; &apos;26"),
        );
        let parsed = parse_propfind(&xml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].href, "/dav/Sales&Marketing.txt");
        assert_eq!(
            parsed[0].displayname.as_deref(),
            Some("Sales & Marketing <Q4> \"plan\" '26")
        );
        assert_eq!(parsed[0].contentlength, Some(42));
        assert!(parsed[0].is_collection);
    }
}

#[test]
fn propfind_decodes_references_once_and_preserves_unicode() {
    let parsed =
        parse_propfind(&response("/dav/caf%C3%A9", "caf&#233; &amp;amp; &#x1F4C4;")).unwrap();
    assert_eq!(parsed[0].displayname.as_deref(), Some("café &amp; 📄"));
}

#[test]
fn propfind_preserves_cdata_and_comment_split_text() {
    let parsed = parse_propfind(&response(
        "/dav/report",
        "Sales <![CDATA[& <Q4>]]> <!-- split -->Marketing",
    ))
    .unwrap();
    assert_eq!(
        parsed[0].displayname.as_deref(),
        Some("Sales & <Q4> Marketing")
    );
}

#[test]
fn propfind_preserves_xml10_nel_and_line_separator_in_text_and_cdata() {
    let name = "A\u{85}B\u{2028}C";
    for displayname in [name.to_owned(), format!("<![CDATA[{name}]]>")] {
        let parsed = parse_propfind(&response("/dav/report", &displayname)).unwrap();
        assert_eq!(parsed[0].displayname.as_deref(), Some(name));
    }
}

#[test]
fn propfind_rejects_unresolved_or_invalid_references() {
    for entity in [
        "&unknown;",
        "&#xZZ;",
        "&#0;",
        "&#1;",
        "&#xFFFE;",
        "&#xFFFF;",
        "&#x110000;",
    ] {
        assert!(
            parse_propfind(&response("/dav/report", entity)).is_err(),
            "{entity}"
        );
    }
    let xml = format!(
        "<!DOCTYPE multistatus [<!ENTITY custom SYSTEM \"file:///etc/passwd\">]>{}",
        response("/dav/report", "&custom;")
    );
    assert!(parse_propfind(&xml).is_err());
}

#[test]
fn propfind_keeps_plain_unprefixed_dav_compatible() {
    let xml = response(" /dav/file%20name.txt ", "file name.txt").replace("d:", "");
    let parsed = parse_propfind(&xml).unwrap();
    assert_eq!(parsed[0].href, "/dav/file%20name.txt");
    assert_eq!(parsed[0].displayname.as_deref(), Some("file name.txt"));
}

#[test]
fn propfind_preserves_significant_displayname_edge_spaces() {
    let parsed = parse_propfind(&response("/dav/%20report%20", " report ")).unwrap();
    assert_eq!(parsed[0].displayname.as_deref(), Some(" report "));
}

#[test]
fn propfind_ignores_unknown_entities_in_unrequested_properties() {
    let xml = response("/dav/report", "report").replace(
        "</d:prop>",
        "<d:vendor-property>&vendor;</d:vendor-property></d:prop>",
    );
    let parsed = parse_propfind(&xml).unwrap();
    assert_eq!(parsed[0].displayname.as_deref(), Some("report"));
}

#[test]
fn propfind_keeps_xml_legal_reference_whitespace() {
    let parsed = parse_propfind(&response("/dav/report", "&#9;A&#10;B&#13;")).unwrap();
    assert_eq!(parsed[0].displayname.as_deref(), Some("\tA\nB\r"));
}

#[test]
fn propfind_rejects_truncated_listings_instead_of_returning_partial_entries() {
    let complete = response("/dav/first.txt", "first.txt");
    let first_entry = complete.strip_suffix("</d:multistatus>").unwrap();
    for truncated in [
        first_entry.to_owned(),
        r#"<d:multistatus xmlns:d="DAV:">"#.to_owned(),
        format!("{first_entry}<d:response><d:href>/dav/second.txt"),
        format!("{first_entry}<d:response><d:propstat><d:prop><vendor>"),
    ] {
        assert!(
            parse_propfind(&truncated).is_err(),
            "truncated listing accepted: {truncated}"
        );
    }
    assert_eq!(parse_propfind(&complete).unwrap().len(), 1);
}

#[test]
fn propfind_accepts_complete_empty_multistatus() {
    for xml in [
        r#"<d:multistatus xmlns:d="DAV:"/>"#,
        r#"<d:multistatus xmlns:d="DAV:"></d:multistatus>"#,
    ] {
        assert!(parse_propfind(xml).unwrap().is_empty());
    }
}

#[test]
fn propfind_rejects_mismatched_closing_tags() {
    let xml = response("/dav/first.txt", "first.txt").replace("</d:multistatus>", "</d:response>");
    assert!(parse_propfind(&xml).is_err());
}

#[test]
fn propfind_rejects_unrelated_roots_even_with_nested_dav_responses() {
    let valid = response("/dav/report", "report");
    for xml in [
        format!("<html>{valid}</html>"),
        valid.replace("d:multistatus", "d:error"),
        format!("{valid}<error/>"),
        format!("<error/>{valid}"),
    ] {
        assert!(parse_propfind(&xml).is_err(), "accepted non-listing: {xml}");
    }
    assert_eq!(parse_propfind(&valid).unwrap().len(), 1);
}

#[test]
fn propfind_accepts_default_namespace_and_unprefixed_empty_documents() {
    for xml in [
        r#"<?xml version="1.0"?><!-- before --><multistatus xmlns="DAV:"/><!-- after -->"#,
        "<multistatus></multistatus>",
        "<multistatus/>",
    ] {
        assert!(parse_propfind(xml).unwrap().is_empty());
    }
}
