//! Portable Cathy/Catfish CAF encoding and metadata types.
//!
//! This crate performs no directory scan, content hashing, database access,
//! networking or UI work. See the crate README for format limits.

#![doc = include_str!("../README.md")]

pub mod caf;
pub mod index;
