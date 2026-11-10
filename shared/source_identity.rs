//! Stable identities for independently addressable source occurrences.
//!
//! A content fingerprint groups identical bytes. An occurrence identity also
//! scopes those bytes to an owner and source URI. Existing stored identities
//! can be retained when a source moves or an older catalog is refreshed.

use sha2::{Digest, Sha256};

pub const SOURCE_IDENTITY_VERSION: &str = "occurrence-v1";

pub fn occurrence_id(owner_id: &str, location_uri: &str, content_hash: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"CrispSorter/source-occurrence/v1\0");
    for field in [owner_id, location_uri] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    // Unlocated input has no independent occurrence locator; keep it scoped to
    // its owner and content until a concrete source is supplied.
    if location_uri.is_empty() {
        digest.update((content_hash.len() as u64).to_le_bytes());
        digest.update(content_hash.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_and_owners_are_independent_from_content_fingerprints() {
        let first = occurrence_id("owner", "file:///first.txt", "same bytes");
        assert_eq!(first, occurrence_id("owner", "file:///first.txt", "changed bytes"));
        assert_ne!(first, occurrence_id("owner", "file:///second.txt", "same bytes"));
        assert_ne!(first, occurrence_id("another owner", "file:///first.txt", "same bytes"));
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn identity_encoding_has_unambiguous_boundaries_and_unlocated_fallback() {
        assert_ne!(occurrence_id("a", "bc", "x"), occurrence_id("ab", "c", "x"));
        assert_ne!(occurrence_id("owner", "", "one"), occurrence_id("owner", "", "two"));
        assert_ne!(occurrence_id("one", "", "same"), occurrence_id("two", "", "same"));
        assert_eq!(occurrence_id("é", "crisp+local://host/ä.txt", "a"), occurrence_id("é", "crisp+local://host/ä.txt", "b"));
    }
}
