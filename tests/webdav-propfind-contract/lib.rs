#![cfg(test)]
//! Compile the production parser with the same quick-xml version as the app.
use anyhow::{anyhow, Result};
use quick_xml::events::Event;
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

#[test]
fn propfind_namespace_uri_guards_protocol_and_metadata() {
    for field in [
        "<v:status/>",
        "<unknown:status/>",
        "<propstat><unknown:prop><displayname>wrong</displayname></unknown:prop><unknown:status/></propstat>",
        "<propstat><prop><v:wrapper><d:displayname>wrong</d:displayname><d:getcontentlength>99</d:getcontentlength><d:href>/wrong</d:href></v:wrapper></prop></propstat>",
        "<propstat><prop><unknown:displayname>wrong</unknown:displayname><unknown:resourcetype><collection/></unknown:resourcetype></prop></propstat>",
        "<v:status>HTTP/1.1 403 Forbidden</v:status>",
        "<v:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:displayname>wrong</d:displayname></d:prop><d:status/></v:propstat>",
        "<d:prop><v:displayname>wrong</v:displayname><v:resourcetype><d:collection/></v:resourcetype></d:prop>",
        "<v:displayname><d:displayname>wrong</d:displayname></v:displayname>",
        "<v:wrapper><d:href>/wrong</d:href><d:displayname>wrong</d:displayname><d:getcontentlength>99</d:getcontentlength><d:getlastmodified>wrong</d:getlastmodified></v:wrapper>",
        "<d:propstat><d:prop><v:displayname>wrong</v:displayname><v:resourcetype/></d:prop><v:status/></d:propstat>",
        "<d:propstat><d:prop><d:resourcetype><v:collection/></d:resourcetype></d:prop><v:status>opaque</v:status></d:propstat>",
        "<d:prop xmlns:d='urn:vendor'><d:displayname>wrong</d:displayname><d:resourcetype><d:collection/></d:resourcetype></d:prop>",
    ] {
        let xml = format!("<d:multistatus xmlns:d='DAV:' xmlns:v='urn:vendor'><v:response><d:href>/fake</d:href></v:response><d:response><d:href>/real</d:href>{field}</d:response></d:multistatus>");
        let parsed = parse_propfind(&xml).unwrap_or_else(|e| panic!("{field}: {e}"));
        assert_eq!(parsed.len(), 1, "{field}");
        assert_eq!(parsed[0].href, "/real", "{field}");
        assert!(parsed[0].displayname.is_none(), "{field}");
        assert!(parsed[0].contentlength.is_none(), "{field}");
        assert!(parsed[0].lastmodified.is_none(), "{field}");
        assert!(!parsed[0].is_collection, "{field}");
    }
}

#[test]
fn propfind_namespace_root_and_legacy_compatibility() {
    for root in [
        "<v:multistatus xmlns:v='urn:vendor'/>",
        "<multistatus xmlns='urn:vendor'/>",
        "<missing:multistatus/>",
        "<v:multistatus xmlns:v='urn:vendor'><response><href>/fake</href></response></v:multistatus>",
    ] {
        assert!(parse_propfind(root).is_err(), "{root}");
    }
    for xml in [
        "<a:multistatus xmlns:a='DAV:'><a:response><href>/real</href><prop><displayname>real</displayname><resourcetype><collection/></resourcetype></prop></a:response></a:multistatus>",
        "<multistatus><response><href>/real</href><prop><displayname>real</displayname><resourcetype><collection/></resourcetype></prop></response></multistatus>",
        "<multistatus xmlns='DAV:'><response><href>/real</href><prop><displayname>real</displayname><resourcetype><collection/></resourcetype></prop></response></multistatus>",
        "<z:multistatus xmlns:z='DAV:' xmlns:v='urn:vendor'><z:response v:status='bad'><z:href>/real</z:href><z:prop><z:resourcetype xmlns:z='urn:vendor'/><v:wrapper xmlns:z='urn:vendor'><z:displayname>wrong</z:displayname></v:wrapper><z:displayname>real</z:displayname><z:resourcetype><z:collection/></z:resourcetype></z:prop></z:response></z:multistatus>",
        "<multistatus xmlns='DAV:'><response><href>/real</href><prop><wrapper xmlns='urn:vendor'><displayname>wrong</displayname></wrapper><displayname>real</displayname><resourcetype xmlns=''><collection/></resourcetype></prop></response></multistatus>",
    ] {
        let parsed = parse_propfind(xml).unwrap();
        assert_eq!(parsed.len(), 1, "{xml}");
        assert_eq!(parsed[0].href, "/real");
        assert_eq!(parsed[0].displayname.as_deref(), Some("real"));
        assert!(parsed[0].is_collection);
    }
}
