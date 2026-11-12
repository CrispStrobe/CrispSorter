//! internxt-core: the Internxt Drive engine — auth, crypto, Drive REST, network
//! (bridge) transfers and the streaming upload/download primitives.
//!
//! This crate knows *how Internxt works* (endpoints, payloads, encryption) and
//! exposes it as a protocol-agnostic library, so a CLI, a WebDAV/FUSE server or a
//! future GUI can all be built on top without touching the API/crypto details.
//!
//! Everything that talks to a human (terminal output, progress bars, prompts,
//! opening a browser) lives in the front-end crate, not here. The seams:
//! * byte progress is reported through the [`ProgressSink`] trait;
//! * `auth::login`'s 2FA code and `sso::login`'s browser-open are injected callbacks;
//! * `auth::refresh_credentials` reports best-effort warnings through an `on_warn`
//!   callback and does no filesystem I/O — the front-end owns credential persistence.

pub mod api;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod models;
pub mod network;
pub mod progress;
pub mod sso;
#[cfg(feature = "thumbnails")]
pub mod thumbnail;
pub mod transfer;

pub use models::Credentials;
pub use progress::ProgressSink;

