//! P13 Tier 2 — CrispLens HTTP client + settings + secret storage.
//!
//! This module is the **Tier 2** entry point for the images vertical.
//! Slice B1 ships the foundation: settings persistence, keychain-
//! backed credential storage, login/logout commands.  Subsequent
//! slices add functionality:
//!
//! * B2 — semantic search wired against `/api/search`.
//! * B3 — Faces subtab wired against `/api/people` + `/api/faces`.
//! * B4 — health monitor + degradation banner.
//! * B5 — open-in-CrispLens deep-link + watchfolder cross-reference.
//!
//! All Tauri commands prefixed `images_crisplens_*` (the `images_`
//! family identifies the vertical; the `crisplens_` infix identifies
//! the backend so we don't clash with future Tier 3 servers).
//!
//! ## Why a separate module from `crate::images`
//!
//! The Tier 1 (`local`, `phash`, `exif`, `thumbnail`) modules are
//! the always-on path and have no notion of authentication or remote
//! state.  Tier 2 (`crisplens`) is opt-in, can be offline / disabled,
//! and owns the auth + transport machinery.  Keeping the two
//! subtrees separated keeps the always-on code immune to network
//! failures and keeps the Tier 2 surface easy to mock for testing.

pub mod secret;
pub mod settings;
pub mod tauri_commands;
