//! Independently addressable documents inside bounded source containers.
//!
//! Physical indexes and byte receipts locate one captured source generation.
//! Stable identity uses the root occurrence and named-entry occurrence chain,
//! so repacking or changing a member's bytes does not change its document ID.
#![allow(missing_docs)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONTAINER_LOCATOR_VERSION: u32 = 1;
pub const MAX_CONTAINER_DEPTH: usize = 4;
pub const MAX_CONTAINER_MEMBERS: usize = 2048;
pub const MAX_CONTAINER_NAME_BYTES: usize = 2048;
pub const MAX_CONTAINER_MEMBER_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_CONTAINER_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_CONTAINER_MAILBOX_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_CONTAINER_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_CONTAINER_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CONTAINER_OMISSION_DETAILS: usize = 64;
pub const MAX_CONTAINER_ROOT_URI_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerKind { Zip, Tar, Gzip, Sevenz, Email, Mailbox }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerEntryKind { Zip, Tar, Gzip, Sevenz, MailboxMessage, MimePart }

impl ContainerEntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip", Self::Tar => "tar", Self::Gzip => "gzip", Self::Sevenz => "sevenz",
            Self::MailboxMessage => "mailbox-message", Self::MimePart => "mime-part",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerEntry {
    pub kind: ContainerEntryKind,
    /// Zero-based physical entry position, including entries not extracted.
    pub index: u32,
    /// Zero-based occurrence of this kind and normalized name in its container.
    pub occurrence: u32,
    /// Validated logical name. Never pass this value to filesystem joins.
    pub name: String,
    /// SHA-256 of the exact decoded bytes represented by this step.
    pub sha256: String,
    pub size: u64,
}

impl ContainerEntry {
    pub fn validate(&self) -> Result<(), String> {
        if self.index as usize >= MAX_CONTAINER_MEMBERS || self.occurrence as usize >= MAX_CONTAINER_MEMBERS {
            return Err("Container entry index exceeds the shared member limit".into());
        }
        if normalize_member_name(&self.name)? != self.name {
            return Err("Container member name is not canonical".into());
        }
        if !valid_sha256(&self.sha256) || self.size > MAX_CONTAINER_MEMBER_BYTES {
            return Err("Invalid container member byte receipt".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerSourceReceipt {
    pub kind: ContainerKind,
    pub sha256: String,
    pub size: u64,
}

impl ContainerSourceReceipt {
    pub fn validate(&self) -> Result<(), String> {
        let limit = if self.kind == ContainerKind::Mailbox { MAX_CONTAINER_MAILBOX_BYTES } else { MAX_CONTAINER_ARCHIVE_BYTES };
        if !valid_sha256(&self.sha256) || self.size > limit {
            return Err("Invalid container source byte receipt".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerOmissionReason { Unsupported, Encrypted, UnsafeName, Link, NoText }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerOmission {
    /// Bounded display provenance; an unread entry has no decoded byte receipt.
    pub path: String,
    pub reason: ContainerOmissionReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerCoverage {
    pub discovered: u32,
    pub extracted: u32,
    pub skipped: u32,
    pub omissions: Vec<ContainerOmission>,
    /// Skipped entries beyond the bounded detailed omission list.
    pub omitted_details: u32,
}

impl ContainerCoverage {
    pub fn validate(&self) -> Result<(), String> {
        if self.discovered as usize > MAX_CONTAINER_MEMBERS
            || self.extracted.checked_add(self.skipped).is_none_or(|count| count > self.discovered)
            || self.omissions.len() > MAX_CONTAINER_OMISSION_DETAILS
            || self.omitted_details.checked_add(self.omissions.len() as u32) != Some(self.skipped)
            || self.omissions.iter().any(|item| item.path.is_empty() || item.path.len() > MAX_CONTAINER_NAME_BYTES * MAX_CONTAINER_DEPTH
                || item.path.chars().any(char::is_control)) {
            return Err("Invalid container extraction coverage".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerMemberLocator {
    pub version: u32,
    pub root_owner_id: String,
    pub root_doc_id: String,
    pub root_location_uri: String,
    pub root_sha256: String,
    /// Outer-to-inner chain; every step refers to decoded bytes in its parent.
    pub entries: Vec<ContainerEntry>,
}

impl ContainerMemberLocator {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != CONTAINER_LOCATOR_VERSION { return Err("Unsupported container locator version".into()); }
        validate_root_identity(&self.root_owner_id, &self.root_doc_id)?;
        if self.root_location_uri.is_empty() || self.root_location_uri.len() > MAX_CONTAINER_ROOT_URI_BYTES
            || self.root_location_uri.chars().any(char::is_control)
            || self.root_location_uri.starts_with("crisp+member:") || !valid_sha256(&self.root_sha256) {
            return Err("Invalid container root source receipt".into());
        }
        validate_entry_chain(&self.entries)
    }

    pub fn member_id(&self) -> Result<String, String> {
        self.validate()?;
        member_id(&self.root_owner_id, &self.root_doc_id, &self.entries)
    }

    pub fn member_uri(&self) -> Result<String, String> {
        self.validate()?;
        member_uri(&self.root_owner_id, &self.root_doc_id, &self.entries)
    }

    /// Validate the locator against the exact selected, owner-scoped index row.
    pub fn validate_identity(&self, owner_id: &str, doc_id: &str, location_uri: &str) -> Result<(), String> {
        if owner_id != self.root_owner_id || doc_id != self.member_id()? || location_uri != self.member_uri()? {
            return Err("The stored member locator does not belong to the selected document".into());
        }
        Ok(())
    }
}

pub fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_root_identity(owner_id: &str, root_doc_id: &str) -> Result<(), String> {
    if [owner_id, root_doc_id].iter().any(|value| value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control)) {
        return Err("Container roots require bounded explicit owner and document identities".into());
    }
    Ok(())
}

pub fn validate_entry_chain(entries: &[ContainerEntry]) -> Result<(), String> {
    if entries.is_empty() || entries.len() > MAX_CONTAINER_DEPTH { return Err("Container member nesting exceeds the shared depth limit".into()); }
    for entry in entries { entry.validate()?; }
    Ok(())
}

/// Normalize logical archive paths once. Names remain provenance, never paths
/// used to create output files. Unsafe names are omitted with visible coverage.
pub fn normalize_member_name(name: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > MAX_CONTAINER_NAME_BYTES
        || name.chars().any(|ch| ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
        return Err("Container member name is empty, too long or contains control characters".into());
    }
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains(':') || normalized.split('/').any(|part| part == "..") {
        return Err("Container member name is absolute or traverses its parent".into());
    }
    let normalized = normalized.split('/').filter(|part| !part.is_empty() && *part != ".").collect::<Vec<_>>().join("/");
    if normalized.is_empty() { return Err("Container member name is empty".into()); }
    Ok(normalized)
}

fn field(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}

/// Prefix selects all descendants of one root without exposing a source path
/// or requiring callers to interpolate arbitrary identifiers into a URI.
pub fn root_member_uri_prefix(owner_id: &str, root_doc_id: &str) -> Result<String, String> {
    validate_root_identity(owner_id, root_doc_id)?;
    let mut hash = Sha256::new(); hash.update(b"CrispSorter/container-root/v1\0");
    field(&mut hash, owner_id); field(&mut hash, root_doc_id);
    Ok(format!("crisp+member://v1/{:x}/", hash.finalize()))
}

pub fn member_uri_prefix(owner_id: &str, root_doc_id: &str) -> Result<String, String> {
    root_member_uri_prefix(owner_id, root_doc_id)
}

/// Stable across changed bytes, root moves and physical member reordering.
pub fn member_id(owner_id: &str, root_doc_id: &str, entries: &[ContainerEntry]) -> Result<String, String> {
    validate_root_identity(owner_id, root_doc_id)?; validate_entry_chain(entries)?;
    let mut hash = Sha256::new(); hash.update(b"CrispSorter/container-member/v1\0");
    field(&mut hash, owner_id); field(&mut hash, root_doc_id);
    for entry in entries {
        field(&mut hash, entry.kind.as_str()); field(&mut hash, &entry.name);
        hash.update(entry.occurrence.to_le_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn member_uri(owner_id: &str, root_doc_id: &str, entries: &[ContainerEntry]) -> Result<String, String> {
    Ok(format!("{}{}", root_member_uri_prefix(owner_id, root_doc_id)?, member_id(owner_id, root_doc_id, entries)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locator() -> ContainerMemberLocator {
        ContainerMemberLocator { version: 1, root_owner_id: "owner".into(), root_doc_id: "existing-root-occurrence".into(),
            root_location_uri: "/records/mail.zip".into(), root_sha256: "a".repeat(64), entries: vec![
                ContainerEntry { kind: ContainerEntryKind::Zip, index: 3, occurrence: 0, name: "mail/message.eml".into(), sha256: "b".repeat(64), size: 100 },
                ContainerEntry { kind: ContainerEntryKind::MimePart, index: 1, occurrence: 0, name: "receipt.pdf".into(), sha256: "c".repeat(64), size: 20 },
            ] }
    }

    #[test]
    fn identities_survive_repacking_content_refresh_and_root_moves() {
        let original = locator(); let id = original.member_id().unwrap(); let uri = original.member_uri().unwrap();
        let mut changed = original.clone(); changed.root_location_uri = "/moved/mail.zip".into(); changed.root_sha256 = "d".repeat(64);
        changed.entries[0].index = 17; changed.entries[0].sha256 = "e".repeat(64); changed.entries[0].size = 201;
        changed.entries[1].index = 6; changed.entries[1].sha256 = "f".repeat(64); changed.entries[1].size = 37;
        assert_eq!(changed.member_id().unwrap(), id); assert_eq!(changed.member_uri().unwrap(), uri);
        assert!(uri.starts_with(&root_member_uri_prefix("owner", "existing-root-occurrence").unwrap()));
        original.validate_identity("owner", &id, &uri).unwrap();
        assert!(original.validate_identity("another owner", &id, &uri).is_err());
    }

    #[test]
    fn owners_roots_duplicate_names_and_chain_boundaries_remain_distinct() {
        let original = locator(); let id = original.member_id().unwrap();
        for kind in 0..5 { let mut changed = original.clone(); match kind {
            0 => changed.root_owner_id = "other".into(), 1 => changed.root_doc_id = "other-root".into(),
            2 => changed.entries[1].occurrence = 1, 3 => changed.entries[1].name = "renamed.pdf".into(),
            _ => changed.entries[1].kind = ContainerEntryKind::Zip,
        } assert_ne!(changed.member_id().unwrap(), id); }
        assert_ne!(root_member_uri_prefix("a", "bc").unwrap(), root_member_uri_prefix("ab", "c").unwrap());
    }

    #[test]
    fn wire_receipts_are_strict_bounded_and_round_trip() {
        let value = locator(); let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["rootDocId"], "existing-root-occurrence"); assert_eq!(json["entries"][1]["kind"], "mime-part");
        let decoded: ContainerMemberLocator = serde_json::from_value(json.clone()).unwrap(); decoded.validate().unwrap(); assert_eq!(decoded, value);
        let mut unknown = json; unknown["uncheckedPath"] = "/tmp/arbitrary".into();
        assert!(serde_json::from_value::<ContainerMemberLocator>(unknown).is_err());
        for mode in 0..6 { let mut invalid = value.clone(); match mode {
            0 => invalid.version = 2, 1 => invalid.root_sha256 = "short".into(),
            2 => invalid.entries[0].size = MAX_CONTAINER_MEMBER_BYTES + 1,
            3 => invalid.entries[0].index = MAX_CONTAINER_MEMBERS as u32,
            4 => invalid.entries = vec![invalid.entries[0].clone(); MAX_CONTAINER_DEPTH + 1],
            _ => invalid.root_location_uri = value.member_uri().unwrap(),
        } assert!(invalid.validate().is_err()); }
    }

    #[test]
    fn unsafe_names_never_become_valid_member_locators() {
        for name in ["../escape.txt", "/absolute.txt", "C:\\drive.txt", "a\0b", "a\nb", "\u{202e}hidden.txt", "."] {
            assert!(normalize_member_name(name).is_err(), "{name:?}");
        }
        assert_eq!(normalize_member_name("notes\\./section//résumé.txt").unwrap(), "notes/section/résumé.txt");
        let mut invalid = locator(); invalid.entries[0].name = "notes/./body.txt".into(); assert!(invalid.validate().is_err());
    }

    #[test]
    fn coverage_requires_complete_omission_accounting() {
        let coverage = ContainerCoverage { discovered: 5, extracted: 2, skipped: 2,
            omissions: vec![ContainerOmission { path: "mail.zip ! locked.pdf".into(), reason: ContainerOmissionReason::Encrypted }], omitted_details: 1 };
        coverage.validate().unwrap();
        let mut invalid = coverage; invalid.omitted_details = 0; assert!(invalid.validate().is_err());
        assert!(ContainerSourceReceipt { kind: ContainerKind::Zip, sha256: "a".repeat(64), size: MAX_CONTAINER_ARCHIVE_BYTES + 1 }.validate().is_err());
    }
}
