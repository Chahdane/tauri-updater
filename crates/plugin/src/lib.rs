//! Binary delta updates for Tauri v2 — a wrapper around `tauri-plugin-updater`.
//!
//! This plugin does not install anything itself. It makes the *download* smaller
//! by rebuilding the installer artifact from a patch, verifies it, and hands the
//! result to the official updater's own install step.
//!
//! # Layering
//!
//! The interesting logic is in [`flow`], which has no Tauri dependency at all.
//! Its two boundaries — the network and the installer — are traits, so the whole
//! update path is testable without a running app. This module is the thin Tauri
//! glue: an HTTP implementation of `Fetch`, an [`InstallHandoff`] over
//! `tauri_plugin_updater::Update`, and the plugin registration.
//!
//! # Safety
//!
//! Tauri verifies signatures inside `Update::download()`, which the delta path
//! bypasses by definition, and `Update::install()` verifies nothing. This
//! plugin's own check is therefore the only one — see `docs/DECISIONS.md` #10 —
//! and [`InstallHandoff::install`] takes a
//! [`VerifiedArtifact`](tauri_updater_delta_core::VerifiedArtifact), which only
//! successful verification can produce. An unverified install is not
//! expressible.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod flow;
mod http;

pub use flow::{run_update, Context, InstallHandoff, Outcome};
pub use http::HttpFetch;

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong running an update.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A download failed.
    #[error("{0}")]
    Fetch(String),

    /// A filesystem operation failed.
    #[error("{0}")]
    Io(String),

    /// The manifest was unusable.
    #[error("{0}")]
    Manifest(String),

    /// Signature verification failed.
    ///
    /// Never recoverable by falling back: a manifest whose signature does not
    /// check out cannot be trusted to describe a safe full download either.
    #[error("{0}")]
    Signature(String),

    /// The installer rejected the artifact.
    #[error("install failed: {0}")]
    Install(String),
}
