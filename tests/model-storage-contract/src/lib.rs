//! Executes the production storage module without a WebView or model runtime.
//! Application command registration remains covered by paired integration CI.

#[path = "../../../src-tauri/src/model_storage_types.rs"]
mod storage_types;
pub use storage_types::{ModelStorageCleanup, ModelStorageEntry, ModelStorageReport};
#[path = "../../../src-tauri/src/model_storage.rs"]
mod storage;
