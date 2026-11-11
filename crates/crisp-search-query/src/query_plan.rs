//! Explicit query intent keeps natural prose separate from the exact language.
use anyhow::{bail, ensure, Result};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QueryIntent {
    #[default]
    Advanced,
    Natural,
}

impl QueryIntent {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "advanced" => Ok(Self::Advanced),
            "natural" => Ok(Self::Natural),
            _ => bail!("Choose advanced syntax or natural language search"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self { Self::Advanced => "advanced", Self::Natural => "natural" }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryPlan {
    pub lexical_query: String,
    pub embedding_query: String,
    /// Explicit advanced conditions remain mandatory after rank fusion.
    pub requires_lexical_match: bool,
}

pub fn plan_query(query: &str, intent: QueryIntent) -> Result<QueryPlan> {
    ensure!(!query.trim().is_empty() && query.len() <= super::MAX_QUERY_BYTES,
        "Enter a search query of at most 8192 bytes");
    let (lexical_query, requires_lexical_match) = match intent {
        QueryIntent::Advanced => (query.to_owned(), super::has_explicit_syntax(query)?),
        QueryIntent::Natural => {
            let terms: BTreeSet<_> = super::simple_tokenize(query).into_iter().collect();
            ensure!(!terms.is_empty(), "The natural-language query contains no indexed words");
            // Bound the generated OR expression without dropping any words.
            ensure!(terms.len() <= 128, "Use at most 128 distinct indexed words in a natural-language query");
            let literal = terms.into_iter().map(|term| match term.as_str() {
                "and" | "or" | "not" => format!("\"{term}\""),
                _ => term,
            }).collect::<Vec<_>>().join(" OR ");
            ensure!(literal.len() <= super::MAX_QUERY_BYTES, "The natural-language query expands beyond the search limit; shorten it");
            (literal, false)
        }
    };
    Ok(QueryPlan { lexical_query, embedding_query: query.to_owned(), requires_lexical_match })
}

/// Expansion changes lexical evidence, never the text supplied to the model or
/// whether a natural question becomes a mandatory Boolean constraint.
pub fn prepare_query(query: &str, intent: QueryIntent, fuzzy: bool, synonyms: bool) -> Result<QueryPlan> {
    let mut plan = plan_query(query, intent)?;
    if synonyms { plan.lexical_query = crate::synonyms::synonym_expand_query(&plan.lexical_query); }
    if fuzzy { plan.lexical_query = crate::fuzzify_query(&plan.lexical_query); }
    ensure!(plan.lexical_query.len() <= super::MAX_QUERY_BYTES, "The expanded query exceeds 8192 bytes; shorten it or disable expansion");
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_punctuation_and_language_operators_are_literal() {
        let raw = "CD4+ cells: alpha/beta, not \"gamma\" (AND delta).";
        let plan = plan_query(raw, QueryIntent::Natural).unwrap();
        assert_eq!(plan.embedding_query, raw);
        assert_eq!(plan.lexical_query, "alpha OR \"and\" OR beta OR cd4 OR cells OR delta OR gamma OR \"not\"");
        assert!(!plan.requires_lexical_match);
        assert!(crate::has_explicit_syntax(&plan.lexical_query).unwrap());
    }

    #[test]
    fn advanced_semantics_and_errors_are_preserved() {
        for raw in ["body:alpha AND NOT beta", "alpha pre/2 beta", "\"alpha beta\"", "alpha~1"] {
            let plan = plan_query(raw, QueryIntent::Advanced).unwrap();
            assert_eq!(plan.lexical_query, raw);
            assert!(plan.requires_lexical_match);
        }
        assert!(plan_query("alpha AND", QueryIntent::Advanced).is_err());
        assert!(!plan_query("alpha beta", QueryIntent::Advanced).unwrap().requires_lexical_match);
    }

    #[test]
    fn natural_expansion_does_not_change_model_input_or_constraint_policy() {
        let raw = "Important contract?";
        let plan = prepare_query(raw, QueryIntent::Natural, true, true).unwrap();
        assert_eq!(plan.embedding_query, raw);
        assert!(!plan.requires_lexical_match);
        assert!(plan.lexical_query.contains("agreement~1"));
        assert!(plan.lexical_query.contains("important~1"));
    }

    #[test]
    fn natural_tokens_use_the_index_analyzer_and_never_silently_truncate() {
        let plan = plan_query("café CAFE 東京", QueryIntent::Natural).unwrap();
        assert_eq!(plan.lexical_query, "cafe OR 東京");
        assert!(plan_query("... + / :", QueryIntent::Natural).is_err());
        assert!(plan_query(&"x".repeat(8193), QueryIntent::Natural).is_err());
        let many = (0..129).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ");
        assert!(plan_query(&many, QueryIntent::Natural).is_err());
        assert!(QueryIntent::parse("guess").is_err());
    }
}
