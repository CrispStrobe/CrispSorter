//! Document translation pipeline.
//!
//! Wraps the `crisp-docx-core` + `crisp-docx-llm` workspace crates and
//! exposes them as Tauri commands so the frontend can drive a
//! "translate this .docx" workflow without needing to ship a separate
//! sidecar binary.
//!
//! See [`tauri_commands`] for the public surface.

pub mod tauri_commands;
