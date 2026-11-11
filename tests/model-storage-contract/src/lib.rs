//! Executes production storage and atomic output without a WebView or model runtime.
//! Application command registration remains covered by paired integration CI.

#[path = "../../../src-tauri/src/model_storage_types.rs"]
mod storage_types;
pub use storage_types::{ModelStorageCleanup, ModelStorageEntry, ModelStorageReport};
#[path = "../../../src-tauri/src/model_storage.rs"]
mod storage;
#[path = "../../../src-tauri/src/ocr_atomic_output.rs"]
mod atomic_output;
#[path = "../../../src-tauri/src/ocr_export_control.rs"]
mod export_control;

#[path = "../../../src-tauri/src/operation_log.rs"]
pub mod operation_log;

// The real logger's injectable/capture tests exercise receipts; this contract
// deliberately has no application event bus or diagnostic console.
pub fn app_log(_level: &str, _message: String) {}
