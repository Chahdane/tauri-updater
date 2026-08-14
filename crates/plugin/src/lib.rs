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
mod tauri_glue;

pub use flow::{run_update, Context, InstallHandoff, Outcome};
pub use http::{
    HttpFetch, HttpFetchBuilder, DEFAULT_CONNECT_TIMEOUT, DEFAULT_MAX_REDIRECTS,
    DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_REQUEST_TIMEOUT,
};
pub use tauri_glue::{Builder, UpdateExt, UpdateSession};

#[doc(no_inline)]
// Deliberately no `UpdateIdentity` re-export. The shipping flow is entered
// through `UpdateSession`, which only a checked `Update` can produce, so there
// is nothing here for a fabricated identity to be handed to. See
// `tauri_glue::UpdateSession` for the exact scope of that guarantee.
pub use tauri_updater_delta_core::{Limits, Refusal};

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
    /// Never recoverable by falling back. The release document is not signed,
    /// so this does not prove the document is forged — but the full-download
    /// path is described by that same document and checked against a signature
    /// from it, so retrying there grants another attempt rather than a safer
    /// one. See `docs/DECISIONS.md` #11.
    #[error("{0}")]
    Signature(String),

    /// The update was refused: a downgrade, a replay, or an identity mismatch.
    ///
    /// Distinct from every other variant, which describe something going wrong
    /// with *obtaining* a release. This one says the release we are being
    /// pointed at is not one to install at all, so there is no retry and no
    /// fallback that would help.
    #[error("update refused: {0}")]
    Refused(tauri_updater_delta_core::Refusal),

    /// The installer rejected the artifact.
    #[error("install failed: {0}")]
    Install(String),
}
