//! Versioned metadata inspection for owner-bound container member materialization.
use serde::{Deserialize, Serialize};
use super::ContainerMemberLocator;

/// Group publication and stored member metadata semantics.
pub const CONTAINER_MEMBERS_VERSION: u32 = 1;
/// Receipt required on member lookup and container queue submission.
pub const CONTAINER_MEMBERS_VERSION_HEADER: &str = "x-crisp-container-members-version";
/// Maximum encoded metadata response, including the bounded locator chain.
pub const MAX_CONTAINER_MEMBER_RESPONSE_BYTES: usize = 32 * 1024;

/// Select a stored document without assuming a first chunk index.
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq,Eq)]
pub struct ContainerMemberRequest {
    /// Negotiated member metadata semantics.
    pub version:u32,
    /// Exact owner of the selected hit.
    pub owner_id:String,
    /// Exact document identity of the selected hit.
    pub doc_id:String,
}

impl ContainerMemberRequest {
    /// Bound and validate the exact selected identity before storage access.
    pub fn validate(&self)->Result<(),String> {
        if self.version != CONTAINER_MEMBERS_VERSION || [&self.owner_id,&self.doc_id].iter()
            .any(|value|value.trim().is_empty()||value.len()>512||value.chars().any(char::is_control)) {
            return Err("Choose a stored member with its exact owner and document identity".into());
        }
        Ok(())
    }
}

/// Typed locator read from the actual stored rows. Ordinary documents have none.
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq,Eq)]
pub struct ContainerMemberResponse {
    /// Negotiated member metadata semantics.
    pub version:u32,
    /// Exact owner selected by the request.
    pub owner_id:String,
    /// Exact document selected by the request.
    pub doc_id:String,
    /// Validated locator, identical across every stored chunk of this member.
    pub locator:Option<ContainerMemberLocator>,
}

impl ContainerMemberResponse {
    /// Reject response substitution and malformed member receipts.
    pub fn validate_for(&self,request:&ContainerMemberRequest)->Result<(),String> {
        request.validate()?;
        if self.version!=request.version || self.owner_id!=request.owner_id || self.doc_id!=request.doc_id {
            return Err("The server returned a different container member identity".into());
        }
        if let Some(locator)=&self.locator {
            locator.validate_identity(&self.owner_id,&self.doc_id,&locator.member_uri()?)?;
        }
        Ok(())
    }
}
