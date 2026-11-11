//! Pure wire adapter shared by the desktop and authenticated remote route.
use anyhow::{anyhow, ensure, Result};
use crisp_index_protocol::{
    EvidenceSpan, ExplainClause, QueryExplanation, QueryExplanationRequest, StoredPassage,
    QUERY_EXPLANATION_SCOPE, QUERY_EXPLANATION_VERSION,
};
use crisp_search_query::explanation::QueryEvidence;
use crisp_search_query::query_plan::{prepare_query, QueryIntent};

pub fn validate_request(request: &QueryExplanationRequest) -> Result<()> {
    request.validate().map_err(|error| anyhow!(error))?;
    if request.mode != "vector" {
        let plan = prepare_query(
            &request.query,
            QueryIntent::parse(&request.query_intent)?,
            request.fuzzy,
            request.synonyms,
        )?;
        ensure!(
            plan.lexical_query == request.lexical_query,
            "Query explanation's lexical plan differs from its executed-query receipt"
        );
    }
    Ok(())
}

pub fn response(
    request: &QueryExplanationRequest,
    passage: StoredPassage,
    evidence: Option<QueryEvidence>,
) -> Result<QueryExplanation> {
    validate_request(request)?;
    ensure!(
        (request.mode == "vector") == evidence.is_none(),
        "Explanation mode and evidence disagree"
    );
    let mut notes=vec![
        "Clause truth describes the current owner-scoped indexed document; source spans refer only to the selected stored passage.".into(),
        "A matching title, heading, translation, or another passage can satisfy a clause without a span in this passage.".into(),
        "This inspection does not recreate a past ranking score or prove the current source file still equals the indexed text.".into(),
        "Clause truth covers the lexical expression; structured metadata filters and ranking are outside this explanation.".into(),
        "Edge words are omitted when a passage boundary may cut through an indexed token.".into(),
    ];
    if request.mode != "text" {
        notes.push("Semantic retrieval provides passage-level evidence. It does not attribute the model score to individual words; hybrid spans describe only lexical matches.".into());
    }
    if passage.truncated {
        notes.push("The stored passage is truncated at its display byte limit; spans cover only the returned text.".into());
    }
    let (matched, clauses, spans, truncated) = match evidence {
        Some(evidence) => (
            Some(evidence.matched),
            Some(clause(evidence.clauses)),
            evidence
                .spans
                .into_iter()
                .map(|span| EvidenceSpan {
                    start: span.start,
                    end: span.end,
                    kind: span.kind,
                    clause: span.clause,
                })
                .collect(),
            evidence.spans_truncated,
        ),
        None => (None, None, Vec::new(), false),
    };
    if truncated {
        notes.push("Span generation is bounded to 256 returned witnesses; additional witnesses were omitted. Clause truth still covers the complete expression.".into());
    }
    let result = QueryExplanation {
        explanation_version: QUERY_EXPLANATION_VERSION,
        query: request.query.clone(),
        lexical_query: request.lexical_query.clone(),
        query_intent: request.query_intent.clone(),
        mode: request.mode.clone(),
        fuzzy: request.fuzzy,
        synonyms: request.synonyms,
        passage,
        lexical_document_match: matched,
        semantic_evidence: request.mode != "text",
        clauses,
        spans,
        spans_truncated: truncated,
        scope: QUERY_EXPLANATION_SCOPE.into(),
        notes,
    };
    result
        .validate_for(request)
        .map_err(|error| anyhow!(error))?;
    Ok(result)
}

fn clause(value: crisp_search_query::explanation::ExplainClause) -> ExplainClause {
    ExplainClause {
        kind: value.kind,
        label: value.label,
        matched: value.matched,
        field: value.field,
        matched_fields: value.matched_fields,
        children: value.children.into_iter().map(clause).collect(),
    }
}
