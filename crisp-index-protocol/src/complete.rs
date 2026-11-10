//! Complete lexical match sets, generation receipts and bounded export pages.
//! A ready session contains metadata only and is bound to its original query,
//! owner/filter scope and stored generation. Pages never change that scope.
#![allow(missing_docs)]
use super::SearchFilters;
use serde::{Deserialize, Serialize};

pub const COMPLETE_SEARCH_VERSION: u32 = 1;
pub const COMPLETE_SEARCH_VERSION_HEADER: &str = "x-crisp-complete-search-version";
pub const COMPLETE_SEARCH_SCOPE: &str = "remote_indexed_documents";
pub const COMPLETE_FACET_VALUE_LIMIT: usize = 25;
pub const MAX_COMPLETE_PAGE_ROWS: usize = 200;
pub const MAX_COMPLETE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const METADATA_SCHEMA_VERSION: u32 = 2;

pub fn valid_request_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_'))
}
fn valid_generation(value: &str) -> bool { value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompleteSearchScope {
    pub version: u32,
    pub query: String,
    pub filters: SearchFilters,
}
impl CompleteSearchScope {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != COMPLETE_SEARCH_VERSION { return Err("Unsupported complete search version; negotiate /v1/capabilities".into()); }
        if self.query.trim().is_empty() || self.query.len() > 8192 { return Err("Complete search requires a query of 1 to 8192 bytes".into()); }
        self.filters.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteSearchRequest {
    #[serde(flatten)] pub scope: CompleteSearchScope,
    pub request_id: String,
    #[serde(default)] pub expected_generation: Option<String>,
}
impl CompleteSearchRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.scope.validate()?;
        if !valid_request_id(&self.request_id) { return Err("Invalid complete search request ID".into()); }
        if self.expected_generation.as_ref().is_some_and(|v| !valid_generation(v)) { return Err("Invalid expected index generation".into()); }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteGeneration {
    pub version: u32, pub scope: String, pub query: String, pub filters: SearchFilters,
    pub generation: String,
}
impl CompleteGeneration {
    pub fn validate_for(&self, scope: &CompleteSearchScope) -> Result<(), String> {
        if self.version != COMPLETE_SEARCH_VERSION || self.scope != COMPLETE_SEARCH_SCOPE || self.query != scope.query
            || self.filters != scope.filters || !valid_generation(&self.generation) {
            return Err("The server returned a different complete search scope or invalid generation".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteFacetValue { pub value: String, pub documents: u64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteFacet { pub values: Vec<CompleteFacetValue>, pub distinct_values: u64, pub omitted_values: u64, pub missing_documents: u64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteFacets { pub extensions: CompleteFacet, pub languages: CompleteFacet, pub years: CompleteFacet, pub tags: CompleteFacet }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteSearchSummary {
    pub version: u32, pub scope: String, pub query: String, pub filters: SearchFilters,
    pub generation: String, pub request_id: String, pub session_id: String, pub expires_at_ms: i64,
    pub matching_documents: u64, pub scanned_rows: u64, pub facet_value_limit: usize, pub facets: CompleteFacets,
}
impl CompleteSearchSummary {
    pub fn validate_for(&self, request: &CompleteSearchRequest) -> Result<(), String> {
        CompleteGeneration { version:self.version,scope:self.scope.clone(),query:self.query.clone(),filters:self.filters.clone(),generation:self.generation.clone() }.validate_for(&request.scope)?;
        if self.request_id != request.request_id || !valid_request_id(&self.session_id) || self.expires_at_ms <= 0
            || self.scanned_rows < self.matching_documents || self.facet_value_limit != COMPLETE_FACET_VALUE_LIMIT
            || request.expected_generation.as_ref().is_some_and(|value| value != &self.generation) {
            return Err("The server returned an invalid complete search receipt".into());
        }
        for facet in [&self.facets.extensions,&self.facets.languages,&self.facets.years,&self.facets.tags] {
            let mut seen = std::collections::HashSet::new();
            if facet.values.len() > COMPLETE_FACET_VALUE_LIMIT || facet.distinct_values < facet.values.len() as u64
                || facet.omitted_values != facet.distinct_values - facet.values.len() as u64 || facet.missing_documents > self.matching_documents
                || facet.values.iter().any(|v| v.value.is_empty() || v.value.len() > 16384 || v.documents == 0 || v.documents > self.matching_documents || !seen.insert(&v.value)) {
                return Err("The server returned invalid complete facet counts".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="kebab-case")]
pub enum CompleteSort { Filename, Title, YearAsc, YearDesc, Location }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="camelCase")]
pub struct CompleteDocument {
    pub doc_id: String, pub owner_id: String, pub representative_chunk_index: i32,
    pub title: Option<String>, pub filename: Option<String>, pub author: Option<String>,
    pub location_uri: String, pub url: Option<String>, pub source_hash: String, pub indexed_at: i64,
    pub extension: Option<String>, pub language: Option<String>, pub year: Option<i32>, pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePageRequest {
    pub version: u32, pub session_id: String, pub generation: String, pub offset: u64,
    pub limit: usize, pub sort: CompleteSort,
}
impl CompletePageRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != COMPLETE_SEARCH_VERSION || !valid_request_id(&self.session_id) || !valid_generation(&self.generation)
            || !(1..=MAX_COMPLETE_PAGE_ROWS).contains(&self.limit) || self.offset > i64::MAX as u64 {
            return Err("Invalid complete result page request".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePage {
    pub version: u32, pub scope: String, pub query: String, pub filters: SearchFilters, pub generation: String,
    pub offset: u64, pub documents: Vec<CompleteDocument>, pub next_offset: Option<u64>, pub matching_documents: u64,
}
impl CompletePage {
    pub fn validate_for(&self, scope: &CompleteSearchScope, request: &CompletePageRequest) -> Result<(), String> {
        CompleteGeneration {version:self.version,scope:self.scope.clone(),query:self.query.clone(),filters:self.filters.clone(),generation:self.generation.clone()}.validate_for(scope)?;
        let end = self.offset.checked_add(self.documents.len() as u64).ok_or("Complete page offset overflow")?;
        let mut seen = std::collections::HashSet::new();
        if self.generation != request.generation || self.offset != request.offset || self.documents.len() > request.limit
            || end > self.matching_documents || self.next_offset != (end < self.matching_documents).then_some(end)
            || (self.next_offset.is_some() && self.documents.is_empty())
            || self.documents.iter().any(|d| d.doc_id.is_empty() || d.representative_chunk_index < 0
                || scope.filters.owner_id.as_ref().is_some_and(|owner| owner != &d.owner_id)
                || !seen.insert((&d.owner_id,&d.doc_id))) {
            return Err("The server returned an incomplete, out-of-scope or noncontiguous result page".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteCancelResponse { pub version: u32, pub request_id: String, pub cancelled: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteDestinationRequest {
    pub version: u32, pub request_id: String, pub generation: String, pub scope: CompleteSearchScope, pub destination_paths: Vec<String>,
}
impl CompleteDestinationRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.scope.validate()?;
        if self.version != COMPLETE_SEARCH_VERSION || !valid_request_id(&self.request_id) || !valid_generation(&self.generation) || self.destination_paths.is_empty()
            || self.destination_paths.len() > 16 || self.destination_paths.iter().any(|v| v.is_empty() || v.len() > 16384 || v.contains('\0')) {
            return Err("Invalid report destination validation request".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteDestinationResponse { pub version: u32, pub generation: String, pub protected: bool }
