use crate::{
    fts_index::{FtsIndex, TantivyInput},
    schema::SearchFilters,
};
use crisp_search_query::explanation::{PassageEdges, QueryEvidence};
const COMPLETE: PassageEdges = PassageEdges {
    start: true,
    end: true,
};

fn fixture() -> (FtsIndex, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let index = FtsIndex::open_or_create(dir.path()).unwrap();
    let mut writer = index.writer().unwrap();
    for (id, owner, title, body, translated) in [
        (
            "book",
            "reader",
            "Annual report",
            "café red fox jumps over silver moon. red agile fox. echo echo echo",
            Some("hello bright world"),
        ),
        (
            "book",
            "other",
            "Private report",
            "private violet words",
            None,
        ),
        ("other", "reader", "Other", "red blue fox", None),
    ] {
        index
            .add_document(
                &mut writer,
                TantivyInput {
                    doc_id: id,
                    owner_id: owner,
                    language: "en",
                    title,
                    headings: "Methods",
                    body,
                    body_translated: translated,
                },
            )
            .unwrap();
    }
    writer.commit().unwrap();
    (index, dir)
}
const BODY: &str = "café red fox jumps over silver moon. red agile fox. echo echo echo";
fn explain(index: &FtsIndex, query: &str, text: &str) -> QueryEvidence {
    index
        .explain_passage(query, "reader", "book", text, COMPLETE)
        .unwrap()
}
fn excerpts<'a>(result: &QueryEvidence, text: &'a str) -> Vec<&'a str> {
    result
        .spans
        .iter()
        .map(|span| &text[span.start..span.end])
        .collect()
}

#[test]
fn document_clause_truth_agrees_with_actual_query_execution() {
    let (index, _dir) = fixture();
    for query in [
        "red AND fox",
        "red NOT blue",
        "NOT red",
        "NOT NOT red",
        "(red AND blue) OR fox",
        "title:annual AND body:red",
        "translated:hello",
        "body:red pre/0 fox",
        "fox pre/0 red",
        "red w/0 fox",
        "\"red fox\"",
        "rde~1",
        "r?d",
        "*",
        "title:private",
        "echo pre/0 echo",
        "\"echo echo\" pre/0 echo",
        "red-fox pre/0 jumps",
    ] {
        let actual = index
            .search(
                query,
                &SearchFilters {
                    owner_id: Some("reader".into()),
                    ..Default::default()
                },
                100,
            )
            .unwrap()
            .iter()
            .any(|hit| hit.doc_id == "book");
        assert_eq!(explain(&index, query, BODY).matched, actual, "{query}");
    }
}

#[test]
fn only_positive_contributing_branches_receive_spans() {
    let (index, _dir) = fixture();
    let result = explain(&index, "(red AND blue) OR fox", BODY);
    assert!(result.matched);
    assert_eq!(excerpts(&result, BODY), ["fox", "fox"]);
    assert_eq!(result.clauses.kind, "or");
    assert!(!result.clauses.children[0].matched);
    assert!(explain(&index, "NOT blue", BODY).spans.is_empty());
    assert_eq!(
        excerpts(&explain(&index, "NOT NOT red", BODY), BODY),
        ["red", "red"]
    );
    assert!(explain(&index, "red AND blue", BODY).spans.is_empty());
    assert!(explain(&index, "title:annual OR translated:hello", BODY)
        .spans
        .is_empty());
    assert_eq!(
        explain(&index, "annual", BODY).clauses.matched_fields,
        ["title"]
    );
    assert_eq!(
        explain(&index, "hello", BODY).clauses.matched_fields,
        ["translated"]
    );
}

#[test]
fn fuzzy_wildcard_and_unicode_spans_preserve_actual_source_bytes() {
    let (index, _dir) = fixture();
    for (query, kind) in [("cfae~1", "fuzzy"), ("caf?", "wildcard"), ("CAFE", "term")] {
        let result = explain(&index, query, BODY);
        assert!(result.matched, "{query}");
        assert_eq!(excerpts(&result, BODY), ["café"]);
        assert_eq!(result.spans[0].kind, kind);
        assert_eq!((result.spans[0].start, result.spans[0].end), (0, 5));
    }
}

#[test]
fn phrases_and_proximity_highlight_complete_nonoverlapping_position_witnesses() {
    let (index, _dir) = fixture();
    assert_eq!(
        excerpts(&explain(&index, "\"red fox\"", BODY), BODY),
        ["red fox"]
    );
    assert_eq!(
        excerpts(&explain(&index, "red pre/1 fox", BODY), BODY),
        ["red fox", "red agile fox"]
    );
    assert_eq!(
        excerpts(&explain(&index, "\"echo echo\" pre/0 echo", BODY), BODY),
        ["echo echo echo"]
    );
    assert_eq!(
        excerpts(&explain(&index, "rde~1 pre/0 f?x", BODY), BODY),
        ["red fox"]
    );
    assert_eq!(
        excerpts(&explain(&index, "(red OR violet) pre/0 fox", BODY), BODY),
        ["red fox"]
    );
    assert!(explain(&index, "fox pre/0 red", BODY).spans.is_empty());
    assert!(explain(&index, "body:red pre/10 title:annual", BODY)
        .spans
        .is_empty());
}

#[test]
fn document_truth_is_independent_of_the_selected_passage_and_owner() {
    let (index, _dir) = fixture();
    let selected = "silver moon";
    let result = explain(&index, "red AND moon", selected);
    assert!(result.matched);
    assert_eq!(excerpts(&result, selected), ["moon"]);
    let other = index
        .explain_passage("private", "other", "book", "private violet words", COMPLETE)
        .unwrap();
    assert!(other.matched);
    assert!(!explain(&index, "private", BODY).matched);
    assert!(index
        .explain_passage("private", "missing", "book", BODY, COMPLETE)
        .is_err());
}

#[test]
fn span_limit_does_not_change_clause_truth_or_invent_completeness() {
    let dir = tempfile::tempdir().unwrap();
    let index = FtsIndex::open_or_create(dir.path()).unwrap();
    let mut writer = index.writer().unwrap();
    let body = "echo ".repeat(300);
    index
        .add_document(
            &mut writer,
            TantivyInput {
                doc_id: "book",
                owner_id: "reader",
                language: "",
                title: "",
                headings: "",
                body: &body,
                body_translated: None,
            },
        )
        .unwrap();
    writer.commit().unwrap();
    let result = explain(&index, "echo", &body);
    assert!(result.matched && result.spans_truncated);
    assert_eq!(result.spans.len(), 256);
    assert!(index
        .explain_passage(
            "echo",
            "reader",
            "book",
            &"x".repeat(256 * 1024 + 1),
            COMPLETE
        )
        .is_err());
}

#[test]
fn analyzer_position_gaps_and_unknown_chunk_boundaries_are_not_phrases() {
    let dir = tempfile::tempdir().unwrap();
    let index = FtsIndex::open_or_create(dir.path()).unwrap();
    let mut writer = index.writer().unwrap();
    let body = format!("red {} fox", "a".repeat(40));
    index
        .add_document(
            &mut writer,
            TantivyInput {
                doc_id: "book",
                owner_id: "reader",
                language: "",
                title: "",
                headings: "",
                body: &body,
                body_translated: None,
            },
        )
        .unwrap();
    writer.commit().unwrap();
    assert!(!explain(&index, "\"red fox\"", &body).matched);
    assert!(!explain(&index, "red pre/0 fox", &body).matched);
    assert_eq!(
        excerpts(&explain(&index, "red pre/1 fox", &body), &body),
        [body.as_str()]
    );
}

#[test]
fn clipped_passage_edges_cannot_manufacture_whole_word_witnesses() {
    let dir = tempfile::tempdir().unwrap();
    let index = FtsIndex::open_or_create(dir.path()).unwrap();
    let mut writer = index.writer().unwrap();
    for (id, body) in [
        ("leading", "malpha middle alpha"),
        ("trailing", "alpha prefix alphabet"),
    ] {
        index
            .add_document(
                &mut writer,
                TantivyInput {
                    doc_id: id,
                    owner_id: "reader",
                    language: "",
                    title: "",
                    headings: "",
                    body,
                    body_translated: None,
                },
            )
            .unwrap();
    }
    writer.commit().unwrap();
    let leading = "alpha middle alpha";
    let result = index
        .explain_passage(
            "alpha",
            "reader",
            "leading",
            leading,
            PassageEdges {
                start: false,
                end: true,
            },
        )
        .unwrap();
    assert!(result.matched);
    assert_eq!(result.spans.len(), 1);
    assert_eq!(result.spans[0].start, 13);
    let trailing = "alpha prefix alpha";
    let result = index
        .explain_passage(
            "alpha",
            "reader",
            "trailing",
            trailing,
            PassageEdges {
                start: true,
                end: false,
            },
        )
        .unwrap();
    assert!(result.matched);
    assert_eq!(result.spans.len(), 1);
    assert_eq!(result.spans[0].start, 0);
    let result = index
        .explain_passage(
            "\"prefix alpha\" OR alpha",
            "reader",
            "trailing",
            trailing,
            PassageEdges {
                start: true,
                end: false,
            },
        )
        .unwrap();
    assert_eq!(excerpts(&result, trailing), ["alpha"]);
}

#[test]
fn long_wildcard_evidence_has_a_real_character_work_bound() {
    let dir = tempfile::tempdir().unwrap();
    let index = FtsIndex::open_or_create(dir.path()).unwrap();
    let mut writer = index.writer().unwrap();
    let body = "alpha ".repeat(50_000);
    let body = &body[..256 * 1024];
    index
        .add_document(
            &mut writer,
            TantivyInput {
                doc_id: "book",
                owner_id: "reader",
                language: "",
                title: "",
                headings: "",
                body,
                body_translated: None,
            },
        )
        .unwrap();
    writer.commit().unwrap();
    // Repeated stars are legal but cannot multiply an uncharged token scan.
    let query = format!("{}alpha{}", "*".repeat(16), "*".repeat(16));
    assert!(index
        .explain_passage(&query, "reader", "book", body, COMPLETE)
        .unwrap_err()
        .to_string()
        .contains("work limit"));
}
