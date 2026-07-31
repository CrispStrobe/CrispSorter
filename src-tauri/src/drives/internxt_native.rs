//! Compatibility re-exports for the reusable native Internxt crate.
//!
//! Keychain storage and the `CloudDrive` adapter remain Tauri-owned; all wire
//! protocol, authentication, and file crypto live in `crisp-internxt`
//! so the standalone CLI exercises exactly the same implementation.

pub use crisp_internxt::*;
