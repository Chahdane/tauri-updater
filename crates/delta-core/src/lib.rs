//! Platform-agnostic binary delta engine for application update artifacts.
//!
//! This crate knows how to turn one version of a file into another using a
//! small patch, and how to prove the result is byte-for-byte correct. It has no
//! knowledge of Tauri, of HTTP, or of how an update is installed — those live in
//! the plugin crate that wraps it.
//!
//! The two halves of the engine are:
//!
//! - [`backend`] — pluggable diff/apply implementations behind the
//!   [`PatchBackend`] trait, with zstd as the default.
//! - [`hash`] — content hashing and verification, used to prove a reconstructed
//!   artifact is identical to the one the release process published.
//!
//! The two are always used together. Applying a patch produces a *candidate*
//! file, never a trusted one: a patch is untrusted input, and only the hash
//! check establishes that reconstruction actually succeeded.
//!
//! ```no_run
//! use tauri_updater_delta_core::{backend::backend_for, hash::{verify_file, FileHash}};
//! # fn main() -> tauri_updater_delta_core::Result<()> {
//! # let (base, patch, rebuilt, expected) =
//! #     (std::path::Path::new("a"), std::path::Path::new("b"),
//! #      std::path::Path::new("c"), "00".repeat(32));
//! let backend = backend_for("zstd")?;
//! backend.apply(base, patch, rebuilt)?;
//! verify_file(rebuilt, &FileHash::from_hex(&expected)?)?;
//! # Ok(())
//! # }
//! ```
//!
//! Every error is recoverable by design: the caller discards the reconstruction
//! and falls back to downloading the full artifact.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod client;
mod error;
pub mod hash;
pub mod manifest;
mod reconstruct;

pub use backend::PatchBackend;
pub use client::{plan_update, Fetch, UpdateSource};
pub use error::{Error, Result};
pub use hash::FileHash;
pub use manifest::Manifest;
pub use reconstruct::{try_reconstruct, Reconstruction, TargetSpec};
