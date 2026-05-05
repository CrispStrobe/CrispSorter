//! Slim Catfish-`.caf`-compatible file-catalog module.
//!
//! Stripped-down restore from the previous `catalog/` subsystem
//! (commit 90ef6d8 on `main`) — just the two leaf modules we need to
//! round-trip the binary `.caf` format.

pub mod caf;
pub mod index;
