// Standalone CPU-only tests for the exact helper used by Paddle recognition.
// Compile with rustc --test; no Cargo, models, ORT or GPU required.
#[path = "../../src-tauri/src/extractors/paddle_resize.rs"]
mod paddle_resize;
