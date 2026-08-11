//! Platform-agnostic binary delta engine for application update artifacts.
//!
//! This crate knows how to turn one version of a file into another using a
//! small patch, and how to prove the result is byte-for-byte correct. It has no
//! knowledge of Tauri, of HTTP, or of how an update is installed — those live in
//! the plugin crate that wraps it.
//!
//! The two halves of the engine are:
//!
//! - [`hash`] — content hashing and verification, used to prove a reconstructed
//!   artifact is identical to the one the release process published.
//! - Patch backends (added alongside this module) — the pluggable diff/apply
//!   implementations.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
pub mod hash;

pub use error::{Error, Result};
pub use hash::FileHash;
