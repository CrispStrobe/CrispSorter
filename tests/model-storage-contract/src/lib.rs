//! Executes production storage and atomic output without a WebView or model runtime.
//! Application command registration remains covered by paired integration CI.

#[path = "../../../src-tauri/src/model_storage_types.rs"]
mod storage_types;
pub use storage_types::{ModelStorageCleanup, ModelStorageEntry, ModelStorageReport};
#[path = "../../../src-tauri/src/model_storage.rs"]
mod storage;
#[path = "../../../src-tauri/src/ocr_atomic_output.rs"]
mod atomic_output;
