//! Versioned, owner-scoped access to stored indexed text.
use serde::{Deserialize, Serialize};

/// Stored-passage request, response, and navigation semantics.
pub const STORED_PASSAGE_VERSION: u32 = 1;
/// Receipt required before accepting a negotiated passage response.
pub const STORED_PASSAGE_VERSION_HEADER: &str = "x-crisp-passage-version";
/// Maximum UTF-8 text bytes in one inspected passage.
pub const MAX_STORED_PASSAGE_BYTES: usize = 256 * 1024;
/// Maximum rows examined to determine one document's stored navigation.
pub const MAX_STORED_DOCUMENT_CHUNKS: usize = 65_536;
/// JSON can encode one source byte as a six-byte escape sequence.
pub const MAX_STORED_PASSAGE_RESPONSE_BYTES: usize = MAX_STORED_PASSAGE_BYTES * 6 + 32 * 1024;

/// Exact stored text selection for `POST /v1/passages/read`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPassageRequest {
    /// Negotiated stored-passage semantics.
    pub passage_version: u32,
    /// Stable document identifier from a search result.
    pub doc_id: String,
    /// Exact owner from the same search result; never a wildcard.
    pub owner_id: String,
    /// Nonnegative stored chunk index, which need not be contiguous.
    pub chunk_index: i32,
}

impl StoredPassageRequest {
    /// Reject malformed identifiers or unsupported versions before storage work.
    pub fn validate(&self) -> Result<(), String> {
        if self.passage_version != STORED_PASSAGE_VERSION {
            return Err("Unsupported stored passage version; negotiate /v1/capabilities".into());
        }
        if self.doc_id.trim().is_empty() || self.doc_id.len() > 4096 || self.doc_id.contains('\0')
            || self.owner_id.len() > 1024 || self.owner_id.contains('\0') || self.chunk_index < 0 {
            return Err("Choose an indexed text passage with its exact owner and document identity".into());
        }
        Ok(())
    }
}

/// A bounded excerpt of one actual indexed chunk, with complete stored navigation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPassage {
    /// Version of this response's semantics.
    pub passage_version: u32,
    /// Exact document selected by the request.
    pub doc_id: String,
    /// Exact owner selected by the request.
    pub owner_id: String,
    /// Stored source text, truncated only at a UTF-8 character boundary.
    pub text: String,
    /// Inclusive original source byte offset, when retained by the extractor.
    pub start_char: Option<i32>,
    /// Exclusive original source byte offset for the complete stored chunk.
    pub end_char: Option<i32>,
    /// Always `utf8_bytes`; these offsets are not page numbers.
    pub offset_unit: String,
    /// Whether the returned text omits the stored chunk's tail.
    pub truncated: bool,
    /// Actual stored chunk index.
    pub chunk_index: i32,
    /// One-based position among distinct stored nonnegative chunk indices.
    pub passage_number: usize,
    /// Actual number of distinct stored nonnegative chunk indices.
    pub chunk_count: usize,
    /// Closest lower stored chunk index, when present.
    pub previous_chunk_index: Option<i32>,
    /// Closest higher stored chunk index, when present.
    pub next_chunk_index: Option<i32>,
    /// Actual stored source fingerprint, not a copy of a search result.
    pub source_hash: Option<String>,
    /// Actual timestamp of the selected stored row, in Unix milliseconds.
    pub indexed_at: Option<i64>,
    /// Typed provenance of an independently indexed member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_member: Option<super::ContainerMemberLocator>,
    /// Captured root container bytes, when this is a root document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_source: Option<super::ContainerSourceReceipt>,
    /// Actual stored extraction coverage and omissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_coverage: Option<super::ContainerCoverage>,
}

impl StoredPassage {
    /// Check identity, navigation, source anchors, and bounds before displaying text.
    pub fn validate_for(&self, request: &StoredPassageRequest) -> Result<(), String> {
        request.validate()?;
        if self.passage_version != request.passage_version || self.doc_id != request.doc_id
            || self.owner_id != request.owner_id || self.chunk_index != request.chunk_index {
            return Err("The server returned a different passage identity; its text was not accepted".into());
        }
        if self.text.len() > MAX_STORED_PASSAGE_BYTES || self.offset_unit != "utf8_bytes"
            || self.chunk_count == 0 || self.chunk_count > MAX_STORED_DOCUMENT_CHUNKS
            || self.passage_number == 0 || self.passage_number > self.chunk_count {
            return Err("The stored passage response has invalid text or navigation bounds".into());
        }
        if self.previous_chunk_index.is_some_and(|index| index < 0 || index >= self.chunk_index)
            || self.next_chunk_index.is_some_and(|index| index <= self.chunk_index)
            || self.previous_chunk_index.is_some() != (self.passage_number > 1)
            || self.next_chunk_index.is_some() != (self.passage_number < self.chunk_count) {
            return Err("The stored passage response has inconsistent navigation".into());
        }
        match (self.start_char, self.end_char) {
            (Some(start), Some(end)) if start >= 0 && end >= start => {
                let length = (end - start) as usize;
                if (!self.truncated && length != self.text.len()) || length < self.text.len() {
                    return Err("The stored passage offsets disagree with its text".into());
                }
            }
            (None, None) => {}
            _ => return Err("The stored passage has incomplete or invalid source offsets".into()),
        }
        if self.source_hash.as_ref().is_some_and(|hash| hash.len() > 4096) {
            return Err("The stored source fingerprint exceeds the response limit".into());
        }
        if let Some(locator)=&self.container_member {
            locator.validate_identity(&self.owner_id,&self.doc_id,&locator.member_uri()?)?;
            if locator.entries.last().map(|entry|entry.sha256.to_ascii_lowercase()) != self.source_hash.as_ref().map(|hash|hash.to_ascii_lowercase()) {
                return Err("Stored passage fingerprint differs from its member receipt".into());
            }
        }
        if let Some(source)=&self.container_source { source.validate()?; }
        if let Some(coverage)=&self.container_coverage { coverage.validate()?; }
        Ok(())
    }
}
