use crisp_search_query::query_plan::{plan_query, prepare_query, QueryIntent};
use crate::{fts_index::{FtsIndex, TantivyInput}, schema::SearchFilters};

#[test]
fn natural_language_is_literal_while_advanced_syntax_remains_binding() {
    let directory = tempfile::tempdir().unwrap();
    let index = FtsIndex::open_or_create(directory.path()).unwrap();
    let mut writer = index.writer().unwrap();
    for (id, body) in [("alpha", "alpha only"), ("beta", "beta only"), ("both", "alpha beta"), ("other", "unrelated")] {
        index.add_document(&mut writer, TantivyInput { doc_id: id, owner_id: "one", language: "en",
            title: "", headings: "", body, body_translated: None }).unwrap();
    }
    writer.commit().unwrap(); drop(writer);
    let ids = |plan: &crisp_search_query::query_plan::QueryPlan| {
        let mut result: Vec<_> = index.search(&plan.lexical_query, &SearchFilters::default(), 20)
            .unwrap().into_iter().map(|hit| hit.doc_id).collect();
        result.sort(); result
    };
    let raw = "alpha AND beta";
    let exact = plan_query(raw, QueryIntent::Advanced).unwrap();
    let natural = plan_query(raw, QueryIntent::Natural).unwrap();
    assert_eq!(ids(&exact), ["both"]);
    assert_eq!(ids(&natural), ["alpha", "beta", "both"]);
    assert!(exact.requires_lexical_match);
    assert!(!natural.requires_lexical_match);
    assert_eq!(natural.embedding_query, raw);
    let prose = plan_query("alpha: / beta+ \"(NOT)!", QueryIntent::Natural).unwrap();
    assert_eq!(ids(&prose), ["alpha", "beta", "both"]);
}

#[test]
fn expansion_preserves_original_model_text_and_bounds_every_word() {
    let plan = prepare_query("Important contract?", QueryIntent::Natural, true, true).unwrap();
    assert!(plan.lexical_query.contains("agreement~1"));
    assert_eq!(plan.embedding_query, "Important contract?");
    assert!(!plan.requires_lexical_match);
    assert_eq!(plan_query("café CAFE 東京", QueryIntent::Natural).unwrap().lexical_query, "cafe OR 東京");
    for query in ["... + / :".to_owned(), "x".repeat(8193), (0..129).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ")] {
        assert!(plan_query(&query, QueryIntent::Natural).is_err());
    }
    assert!(QueryIntent::parse("automatic").is_err());
    assert!(plan_query("alpha AND", QueryIntent::Advanced).is_err());
}
