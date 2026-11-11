//! Desktop adapter for the shared lexical query implementation.
pub use crisp_search_query::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{
        fts_index::{FtsIndex, TantivyInput},
        schema::SearchFilters,
    };
    use tempfile::TempDir;

    fn fixture() -> (FtsIndex, TempDir) {
        let dir = TempDir::new().unwrap();
        let index = FtsIndex::open_or_create(dir.path()).unwrap();
        let mut writer = index.writer().unwrap();
        for (id, title, headings, body, translation, owner) in [
            ("a", "Alpha beta handbook", "Methods", "red fox jumps over silver moon", Some("hello bright world"), "one"),
            ("b", "Beta alpha compendium", "Results", "silver moon greets red fox", None, "one"),
            ("c", "Gamma", "Appendix", "red agile fox rests outside near silver dark moon", None, "two"),
            ("d", "Only", "", "lonely zebra", None, "two"),
            ("e", "Unicode", "", "München übermäßig Москва 京都 東京 中文 Ελληνικά العربية İstanbul Straße café cafe\u{301}", None, "two"),
        ] {
            index.add_document(&mut writer, TantivyInput { doc_id: id, owner_id: owner, language: "", title, headings, body, body_translated: translation }).unwrap();
        }
        writer.commit().unwrap();
        (index, dir)
    }
    fn ids(index: &FtsIndex, query: &str) -> Vec<String> {
        let mut ids: Vec<_> = index
            .search(query, &SearchFilters::default(), 100)
            .unwrap_or_else(|e| panic!("{query}: {e:#}"))
            .into_iter()
            .map(|hit| hit.doc_id)
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn passage_terms_follow_query_structure_without_claiming_expanded_matches() {
        assert_eq!(literal_body_terms("café AND 東京 NOT draft").unwrap(), ["cafe", "東京"]);
        assert_eq!(literal_body_terms("title:invoice AND body:(paid OR due) AND NOT headings:draft").unwrap(), ["due", "paid"]);
        assert_eq!(literal_body_terms("NOT NOT body:paid").unwrap(), ["paid"]);
        assert_eq!(literal_body_terms(r#"body:("and not" pre/2 x)"#).unwrap(), ["and", "not", "x"]);
        assert!(literal_body_terms("title:invoice OR headings:renewal OR translated:paid").unwrap().is_empty());
        assert!(literal_body_terms("pay* OR invoice~1 OR NOT excluded").unwrap().is_empty());
        assert!(literal_body_terms("paid AND").is_err());
    }

    #[test]
    fn boolean_complements_and_precedence_match_real_documents() {
        let (index, _dir) = fixture();
        assert_eq!(ids(&index, "red NOT agile"), ["a", "b"]);
        assert_eq!(ids(&index, "red AND NOT agile"), ["a", "b"]);
        assert_eq!(ids(&index, "NOT red"), ["d", "e"]);
        assert_eq!(ids(&index, "NOT NOT red"), ["a", "b", "c"]);
        assert_eq!(ids(&index, "red AND NOT (agile OR jumps)"), ["b"]);
        assert_eq!(ids(&index, "red OR lonely AND zebra"), ["a", "b", "c", "d"]);
        assert_eq!(ids(&index, "(red OR lonely) AND zebra"), ["d"]);
        assert_eq!(ids(&index, "NOT (red OR lonely)"), ["e"]);
    }

    #[test]
    fn fields_accept_terms_phrases_and_groups() {
        let (index, _dir) = fixture();
        assert_eq!(ids(&index, "TITLE:\"alpha beta\""), ["a"]);
        assert_eq!(ids(&index, "title:(alpha AND NOT handbook)"), ["b"]);
        assert_eq!(ids(&index, "headings:(Methods OR Results)"), ["a", "b"]);
        assert_eq!(ids(&index, "body:alpha"), Vec::<String>::new());
        assert_eq!(ids(&index, "title:alpha AND body:\"red fox\""), ["a", "b"]);
        assert_eq!(ids(&index, "translated:\"hello bright\""), ["a"]);
        assert_eq!(ids(&index, "translated:hel*"), ["a"]);
        assert_eq!(ids(&index, "translated:helo~1"), ["a"]);
        assert_eq!(ids(&index, "\"hello bright\""), ["a"]);
    }

    #[test]
    fn tokenization_preserves_scripts_and_index_punctuation() {
        let (index, _dir) = fixture();
        for query in [
            "Москва",
            "京都",
            "中文",
            "Ελληνικά",
            "العربية",
            "München",
            "Munchen",
            "übermäßig",
            "ubermassig",
            "İstanbul",
            "Straße",
            "STRASSE",
            "café",
            "cafe\u{301}",
            "\"München übermäßig\"",
        ] {
            assert_eq!(ids(&index, query), ["e"], "{query}");
        }
        assert_eq!(ids(&index, "\"red,fox\""), ["a", "b"]);
        assert_eq!(ids(&index, "red-fox"), ["a", "b", "c"]);
        assert_eq!(ids(&index, "red\u{2003}fox"), ["a", "b", "c"]);
        assert_eq!(
            simple_tokenize("alpha-beta, Γάμμα!"),
            ["alpha", "beta", "γάμμα"]
        );
    }

    #[test]
    fn ordered_proximity_checks_real_positions_and_distance_boundaries() {
        let (index, _dir) = fixture();
        assert_eq!(ids(&index, "alpha pre/0 beta"), ["a"]);
        assert_eq!(ids(&index, "alpha pre/100 beta"), ["a"]); // reversed title must never match
        assert_eq!(ids(&index, "alpha w/0 beta"), ["a", "b"]);
        assert_eq!(ids(&index, "red pre/0 fox"), ["a", "b"]);
        assert_eq!(ids(&index, "red pre/1 fox"), ["a", "b", "c"]);
        assert_eq!(ids(&index, "red pre/2 silver"), Vec::<String>::new());
        assert_eq!(ids(&index, "red pre/3 silver"), ["a"]);
        assert_eq!(ids(&index, "red w/3 silver"), ["a", "b"]);
        assert_eq!(ids(&index, "body:alpha pre/10 beta"), Vec::<String>::new());
        assert_eq!(
            ids(&index, "title:alpha pre/10 body:beta"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn proximity_preserves_whole_phrases_or_choices_and_modifiers() {
        let (index, _dir) = fixture();
        assert_eq!(ids(&index, "\"red fox\" pre/2 \"silver moon\""), ["a"]);
        assert_eq!(ids(&index, "\"red fox\" w/2 \"silver moon\""), ["a", "b"]);
        assert_eq!(
            ids(&index, "(red OR lonely) pre/0 (fox OR zebra)"),
            ["a", "b", "d"]
        );
        assert_eq!(
            ids(&index, "(red OR lonely) pre/1 (fox OR zebra)"),
            ["a", "b", "c", "d"]
        );
        assert_eq!(ids(&index, "re* pre/0 f?x"), ["a", "b"]);
        assert_eq!(ids(&index, "rde~1 pre/0 fox"), ["a", "b"]);
        assert_eq!(ids(&index, "title:(alp* pre/0 beta)"), ["a"]);
        assert_eq!(ids(&index, "translated:(hello pre/1 world)"), ["a"]);
        assert_eq!(ids(&index, "nevermatch* pre/0 fox"), Vec::<String>::new());
    }

    #[test]
    fn repeated_terms_require_distinct_nonoverlapping_occurrences() {
        let dir = TempDir::new().unwrap();
        let index = FtsIndex::open_or_create(dir.path()).unwrap();
        let mut writer = index.writer().unwrap();
        for (id, body) in [
            ("one", "echo"),
            ("two", "echo echo"),
            ("three", "echo small echo"),
            ("four", "echo echo echo"),
        ] {
            index
                .add_document(
                    &mut writer,
                    TantivyInput {
                        doc_id: id,
                        owner_id: "",
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
        assert_eq!(ids(&index, "echo w/0 echo"), ["four", "two"]);
        assert_eq!(ids(&index, "echo pre/1 echo"), ["four", "three", "two"]);
        assert_eq!(ids(&index, "\"echo echo\" pre/0 echo"), ["four"]);
    }

    #[test]
    fn malformed_and_unsupported_queries_fail_instead_of_broadening() {
        let (index, _dir) = fixture();
        for query in [
            "",
            " ",
            "\"red",
            "(red",
            "red)",
            "red) OR zebra",
            "()",
            "red AND",
            "OR red",
            "red OR OR fox",
            "NOT",
            "title:",
            "title:()",
            "title:(red OR)",
            "title:\"",
            "title:headings:",
            "author:red",
            "\"\"",
            "red w/no fox",
            "red w/1025 fox",
            "red~3",
            "red~bogus",
            "red~~1",
            "~1",
            "red*~1",
            "(red AND fox) w/2 moon",
            "(NOT red) w/2 moon",
            "(red w/2 fox) w/2 moon",
            "red pre/2 fox pre/2 moon",
        ] {
            assert!(
                index.search(query, &SearchFilters::default(), 10).is_err(),
                "must reject {query:?}"
            );
        }
        assert!(has_explicit_syntax(&format!("{}red{}", "(".repeat(33), ")".repeat(33))).is_err());
        assert!(has_explicit_syntax(&"r ".repeat(513)).is_err());
        assert!(has_explicit_syntax(&"a".repeat(8193)).is_err());
    }

    #[test]
    fn modifier_expansion_has_explicit_limits_and_no_truncated_matches() {
        let dir = TempDir::new().unwrap();
        let index = FtsIndex::open_or_create(dir.path()).unwrap();
        let mut writer = index.writer().unwrap();
        let body = (0..140)
            .map(|n| format!("word{n} anchor"))
            .collect::<Vec<_>>()
            .join(" ");
        index
            .add_document(
                &mut writer,
                TantivyInput {
                    doc_id: "wide",
                    owner_id: "",
                    language: "",
                    title: "",
                    headings: "",
                    body: &body,
                    body_translated: None,
                },
            )
            .unwrap();
        writer.commit().unwrap();
        let error = index
            .search("word* pre/0 anchor", &SearchFilters::default(), 10)
            .unwrap_err()
            .to_string();
        assert!(error.contains("too many words"), "{error}");
        assert_eq!(ids(&index, "body:word13* pre/0 anchor"), ["wide"]);
    }

    #[test]
    fn explicit_syntax_detection_and_fuzzification_preserve_query_intent() {
        assert!(!has_explicit_syntax("climate change").unwrap());
        for query in [
            "climate AND change",
            "NOT change",
            "title:climate",
            "\"climate change\"",
            "clim*",
            "climate~1",
            "climate pre/2 change",
            "(climate OR weather)",
        ] {
            assert!(has_explicit_syntax(query).unwrap(), "{query}");
        }
        assert_eq!(
            fuzzify_query("climate change 2024"),
            "climate~1 change~1 2024"
        );
        assert_eq!(
            fuzzify_query("title:\"exact phrase\" AND climate"),
            "title:\"exact phrase\" AND climate~1"
        );
        assert_eq!(
            fuzzify_query("climate PRE/2 weather"),
            "climate~1 PRE/2 weather~1"
        );
        assert_eq!(fuzzify_query("title:karl body:foo"), "title:karl body:foo");
        assert_eq!(fuzzify_query("\"unclosed phrase"), "\"unclosed phrase");
        assert_eq!(
            fuzzify_query("(climate OR weather)"),
            "(climate~1 OR weather~1)"
        );
        assert_eq!(fuzzify_query(""), "");
    }

    #[test]
    fn synonyms_and_fuzzy_rewrites_preserve_scoped_quoted_phrases() {
        let dir = TempDir::new().unwrap();
        let index = FtsIndex::open_or_create(dir.path()).unwrap();
        let mut writer = index.writer().unwrap();
        for (id, title, body) in [
            ("literal", "big contract help", "support"),
            ("alternative", "large agreement aid", "assist"),
        ] {
            index
                .add_document(
                    &mut writer,
                    TantivyInput {
                        doc_id: id,
                        owner_id: "",
                        language: "",
                        title,
                        headings: "",
                        body,
                        body_translated: None,
                    },
                )
                .unwrap();
        }
        writer.commit().unwrap();
        let expand = crate::index::synonyms::synonym_expand_query;
        assert_eq!(
            expand("title:\"big contract help\""),
            "title:\"big contract help\""
        );
        assert_eq!(
            ids(&index, &expand("title:\"big contract help\"")),
            ["literal"]
        );
        assert_eq!(
            ids(&index, &expand("title:(big AND contract)")),
            ["alternative", "literal"]
        );
        assert_eq!(
            ids(&index, &expand("\"big contract\" OR assist")),
            ["alternative", "literal"]
        );
        assert_eq!(
            ids(&index, &expand("big PRE/0 contract")),
            ["alternative", "literal"]
        );
        assert_eq!(
            ids(
                &index,
                &fuzzify_query(&expand("title:\"big contract help\""))
            ),
            ["literal"]
        );
        let escaped = r#"title:"big \"contract\" help""#;
        assert_eq!(expand(escaped), escaped);
        assert_eq!(expand("title:\"unclosed help"), "title:\"unclosed help");
    }

    #[test]
    fn glossary_counts_live_documents_once_and_respects_owner() {
        let (index, _dir) = fixture();
        let terms = index
            .vocabulary("al", 10, &SearchFilters::default())
            .unwrap();
        assert_eq!(
            terms
                .iter()
                .map(|t| (t.term.as_str(), t.doc_freq))
                .collect::<Vec<_>>(),
            [("alpha", 2)]
        );
        let owner = SearchFilters {
            owner_id: Some("two".into()),
            ..Default::default()
        };
        assert!(index.vocabulary("al", 10, &owner).unwrap().is_empty());
        assert_eq!(
            index
                .vocabulary("MÜ", 10, &SearchFilters::default())
                .unwrap()[0]
                .term,
            "munchen"
        );
        assert!(index
            .vocabulary("red*", 10, &SearchFilters::default())
            .is_err());
        let mut writer = index.writer().unwrap();
        index.delete_document(&mut writer, "a").unwrap();
        writer.commit().unwrap();
        assert_eq!(
            index
                .vocabulary("al", 10, &SearchFilters::default())
                .unwrap()[0]
                .doc_freq,
            1
        );
    }

    #[test]
    fn candidate_verification_and_pages_share_owner_and_snapshot() {
        let (index, _dir) = fixture();
        let owner = SearchFilters {
            owner_id: Some("one".into()),
            ..Default::default()
        };
        let prepared = index.prepare_search("red", &owner).unwrap();
        let candidates = ["a", "b", "c"].map(str::to_owned);
        let expected = std::collections::HashSet::from(["a".to_owned(), "b".to_owned()]);
        assert_eq!(prepared.matching_doc_ids(&candidates).unwrap(), expected);
        let mut writer = index.writer().unwrap();
        index.delete_document(&mut writer, "a").unwrap();
        writer.commit().unwrap();
        // Even after deletion, both operations use the same captured version.
        assert_eq!(prepared.matching_doc_ids(&candidates).unwrap(), expected);
        assert_eq!(
            prepared
                .search_page(10, 0)
                .unwrap()
                .into_iter()
                .map(|hit| hit.doc_id)
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        let fresh = index.prepare_search("red", &owner).unwrap();
        assert_eq!(
            fresh.matching_doc_ids(&candidates).unwrap(),
            std::collections::HashSet::from(["b".to_owned()])
        );
    }

    #[test]
    fn candidate_verification_cannot_bypass_boolean_or_field_constraints() {
        let (index, _dir) = fixture();
        let candidates = ["a", "b", "c", "d", "missing"].map(str::to_owned);
        let matching = index
            .matching_doc_ids("title:alpha AND NOT handbook", &candidates)
            .unwrap();
        assert_eq!(matching, std::collections::HashSet::from(["b".to_owned()]));
        assert_eq!(
            index
                .matching_doc_ids("red pre/0 fox", &candidates)
                .unwrap(),
            std::collections::HashSet::from(["a".to_owned(), "b".to_owned()])
        );
        assert!(index.matching_doc_ids("red)", &[]).is_err());
        assert!(index
            .matching_doc_ids("red", &vec!["a".into(); 1025])
            .is_err());
    }
}
