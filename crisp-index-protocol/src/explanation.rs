//! Explicit query evidence bound to one actual stored passage and query plan.
use super::{StoredPassage, StoredPassageRequest, STORED_PASSAGE_VERSION};
use serde::{Deserialize, Serialize};

/// Current clause truth and passage span semantics.
pub const QUERY_EXPLANATION_VERSION: u32 = 1;
/// Response header confirming the negotiated explanation version.
pub const QUERY_EXPLANATION_VERSION_HEADER: &str = "x-crisp-query-explanation-version";
/// Bounded JSON response, including escaped passage text and the clause tree.
pub const MAX_QUERY_EXPLANATION_RESPONSE_BYTES: usize =
    super::MAX_STORED_PASSAGE_RESPONSE_BYTES + 1024 * 1024;
/// Clause truth covers a document; byte ranges cover the selected passage.
pub const QUERY_EXPLANATION_SCOPE: &str = "document_query_with_passage_spans";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Exact owner-scoped request with the original and effective lexical queries.
pub struct QueryExplanationRequest {
    /// Version of the explanation semantics.
    pub explanation_version: u32,
    /// Exact indexed document identity.
    pub doc_id: String,
    /// Exact indexed owner identity.
    pub owner_id: String,
    /// Actual selected stored chunk index.
    pub chunk_index: i32,
    /// Original executed query before planning and lexical expansion.
    pub query: String,
    /// Actual planned/expanded expression sent to the lexical index. Empty in vector mode.
    pub lexical_query: String,
    /// Explicit advanced or natural query interpretation.
    pub query_intent: String,
    /// `text`, `vector`, or `hybrid`, matching desktop search mode receipts.
    pub mode: String,
    /// Whether the executed lexical query used fuzzy expansion.
    pub fuzzy: bool,
    /// Whether the executed lexical query used synonym expansion.
    pub synonyms: bool,
}

impl QueryExplanationRequest {
    /// Reuse the independent stored-passage identity and navigation validator.
    pub fn passage_request(&self) -> StoredPassageRequest {
        StoredPassageRequest {
            passage_version: STORED_PASSAGE_VERSION,
            doc_id: self.doc_id.clone(),
            owner_id: self.owner_id.clone(),
            chunk_index: self.chunk_index,
        }
    }
    /// Reject malformed identities, versions, query plans, and unsupported modes.
    pub fn validate(&self) -> Result<(), String> {
        self.passage_request().validate()?;
        if self.explanation_version != QUERY_EXPLANATION_VERSION {
            return Err("Unsupported query explanation version; negotiate /v1/capabilities".into());
        }
        if self.query.trim().is_empty()
            || self.query.len() > 8192
            || self.lexical_query.len() > 8192
            || !matches!(self.query_intent.as_str(), "advanced" | "natural")
            || !matches!(self.mode.as_str(), "text" | "vector" | "hybrid")
            || (self.mode == "vector") != self.lexical_query.is_empty()
        {
            return Err("Query explanation requires a bounded executed query, query intent, mode and lexical plan".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Actual indexed truth for one parsed clause, including Boolean structure.
pub struct ExplainClause {
    /// Clause or source witness category.
    pub kind: String,
    /// Bounded readable clause label; the full query remains in the receipt.
    pub label: String,
    /// Whether the actual indexed document satisfies this clause.
    pub matched: bool,
    /// Explicit canonical field scope, when present.
    pub field: Option<String>,
    /// Fields with actual positive postings for a matched leaf clause.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_fields: Vec<String>,
    /// Parsed child clauses in their original logical order.
    pub children: Vec<ExplainClause>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// A contributing lexical witness within the response passage.
pub struct EvidenceSpan {
    /// Inclusive UTF-8 byte offset relative to `passage.text`.
    pub start: usize,
    /// Exclusive UTF-8 byte offset relative to `passage.text`.
    pub end: usize,
    /// Clause or source witness category.
    pub kind: String,
    /// Readable label of the clause supported by this source range.
    pub clause: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Bounded evidence receipt for the exact requested query and stored passage.
pub struct QueryExplanation {
    /// Version of the explanation semantics.
    pub explanation_version: u32,
    /// Original executed query before planning and lexical expansion.
    pub query: String,
    /// Actual planned and expanded lexical expression; empty for vector mode.
    pub lexical_query: String,
    /// Explicit advanced or natural query interpretation.
    pub query_intent: String,
    /// Executed retrieval mode.
    pub mode: String,
    /// Whether the executed lexical query used fuzzy expansion.
    pub fuzzy: bool,
    /// Whether the executed lexical query used synonym expansion.
    pub synonyms: bool,
    /// Current exact stored source text, navigation and provenance.
    pub passage: StoredPassage,
    /// Complete lexical expression truth, absent for vector-only evidence.
    pub lexical_document_match: Option<bool>,
    /// Indicates the retrieval mode can use semantic evidence, not token attribution.
    pub semantic_evidence: bool,
    /// Full parsed clause tree, absent for vector-only evidence.
    pub clauses: Option<ExplainClause>,
    /// Contributing positive lexical witnesses in the selected passage.
    pub spans: Vec<EvidenceSpan>,
    /// Whether additional source witnesses were omitted at an evidence bound.
    pub spans_truncated: bool,
    /// Constant describing document truth with passage-relative spans.
    pub scope: String,
    /// Explicit evidence scope and completeness qualifications.
    pub notes: Vec<String>,
}

impl QueryExplanation {
    /// Validate the exact receipts, Boolean truth, Unicode ranges and bounds.
    pub fn validate_for(&self, request: &QueryExplanationRequest) -> Result<(), String> {
        request.validate()?;
        self.passage.validate_for(&request.passage_request())?;
        if self.explanation_version != request.explanation_version
            || self.query != request.query
            || self.lexical_query != request.lexical_query
            || self.query_intent != request.query_intent
            || self.mode != request.mode
            || self.fuzzy != request.fuzzy
            || self.synonyms != request.synonyms
            || self.scope != QUERY_EXPLANATION_SCOPE
            || self.semantic_evidence != (request.mode != "text")
        {
            return Err("The server returned different query explanation receipts; its evidence was not accepted".into());
        }
        if self.spans.len() > 256
            || self.notes.len() > 32
            || self.notes.iter().any(|note| note.len() > 8192)
        {
            return Err("Query explanation exceeds its evidence bounds".into());
        }
        if request.mode == "vector" {
            if self.lexical_document_match.is_some()
                || self.clauses.is_some()
                || !self.spans.is_empty()
                || self.spans_truncated
            {
                return Err(
                    "Semantic passage evidence cannot claim lexical clause or token attribution"
                        .into(),
                );
            }
        } else {
            let clauses = self
                .clauses
                .as_ref()
                .ok_or("Lexical explanation is missing its clause tree")?;
            if Some(clauses.matched) != self.lexical_document_match {
                return Err("Query explanation has inconsistent clause truth".into());
            }
            validate_clause(clauses, 0, &mut 0)?;
            if self.lexical_document_match != Some(true)
                && (!self.spans.is_empty() || self.spans_truncated)
            {
                return Err(
                    "A nonmatching expression cannot claim contributing source spans".into(),
                );
            }
        }
        for span in &self.spans {
            if span.start >= span.end
                || span.end > self.passage.text.len()
                || !self.passage.text.is_char_boundary(span.start)
                || !self.passage.text.is_char_boundary(span.end)
                || span.clause.len() > 512
                || !matches!(
                    span.kind.as_str(),
                    "term" | "fuzzy" | "wildcard" | "phrase" | "proximity"
                )
            {
                return Err("Query explanation contains invalid source byte spans".into());
            }
        }
        Ok(())
    }
}

fn validate_clause(node: &ExplainClause, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if depth > 64
        || *nodes > 1024
        || node.label.len() > 512
        || node.matched_fields.len() > 4
        || node
            .matched_fields
            .iter()
            .any(|field| !matches!(field.as_str(), "title" | "body" | "headings" | "translated"))
        || node
            .matched_fields
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != node.matched_fields.len()
        || (!node.matched && !node.matched_fields.is_empty())
        || node
            .field
            .as_deref()
            .is_some_and(|field| !matches!(field, "title" | "body" | "headings" | "translated"))
    {
        return Err("Query explanation has invalid clause bounds".into());
    }
    let valid = match node.kind.as_str() {
        "and" => {
            !node.children.is_empty()
                && node.matched == node.children.iter().all(|child| child.matched)
        }
        "or" => {
            !node.children.is_empty()
                && node.matched == node.children.iter().any(|child| child.matched)
        }
        "not" => node.children.len() == 1 && node.matched != node.children[0].matched,
        "field" => {
            node.children.len() == 1
                && node.field.is_some()
                && node.matched == node.children[0].matched
        }
        "term" | "phrase" | "proximity" => node.children.is_empty(),
        "all" => node.children.is_empty() && node.matched,
        _ => false,
    };
    if !valid {
        return Err("Query explanation has inconsistent clause semantics".into());
    }
    for child in &node.children {
        validate_clause(child, depth + 1, nodes)?;
    }
    Ok(())
}
