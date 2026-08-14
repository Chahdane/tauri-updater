//! Differential updates for Tauri v2, using the official updater for the check
//! and install boundaries.
//!
//! The normal integration is deliberately small:
//!
//! ```text
//! register both plugins -> app.delta_updater().check() -> update.install()
//! ```
//!
//! The returned update owns the exact `tauri_plugin_updater::Update` produced by
//! that one check. Internally the plugin chooses TarDelta, DirectDelta, or Full,
//! verifies the final official artifact, and installs through the same Tauri
//! update. Applications never construct release identities, verified artifacts,
//! cache state, or install handoffs.
//!
//! # First update
//!
//! A fresh installation has no verified official artifact in its cache. The
//! first update after adopting this plugin is therefore normally a **Full**
//! download. It is staged, and after the updated application launches it becomes
//! the base that a later compatible release may use for **TarDelta**. Cache
//! misses, corruption, and unavailable patches safely degrade to Full where
//! policy allows; differential updates are an optimisation, not a promise for
//! every release.
//!
//! # Minimal Rust flow
//!
//! ```no_run
//! use tauri_plugin_updater_delta::DeltaUpdaterExt;
//!
//! async fn update(app: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
//!     if let Some(update) = app.delta_updater().check().await? {
//!         let outcome = update.install().await?;
//!         println!("installed via {}", outcome.path_name());
//!     }
//!     Ok(())
//! }
//! ```
//!
//! Register `tauri_plugin_updater` and a [`Builder::new`] plugin on the Tauri
//! application builder. Configure the updater endpoint and public key in
//! `tauri.conf.json`; this plugin reads those same application facts and does
//! not fetch a second manifest.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod flow;
mod http;
mod tauri_glue;

pub use tauri_glue::{Builder, DeltaUpdater, DeltaUpdaterExt, Update};

#[doc(no_inline)]
pub use tauri_updater_delta_core::cache::CacheLimits;
#[doc(no_inline)]
pub use tauri_updater_delta_core::{Limits, Refusal};

/// Internal engine and transport seams used only by this repository's tests.
///
/// This module is absent from normal builds. It is not a supported application
/// API; enable `test-support` only for controlled integration tests.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    pub use crate::flow::{run_update, Context, InstallHandoff, Outcome};
    pub use crate::http::{
        HttpFetch, HttpFetchBuilder, DEFAULT_CONNECT_TIMEOUT, DEFAULT_MAX_REDIRECTS,
        DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_REQUEST_TIMEOUT,
    };

    /// Run the internal flow while retaining its non-fatal cache diagnostic.
    pub fn run_update_with_cache_diagnostic(
        identity: &tauri_updater_delta_core::UpdateIdentity,
        ctx: &Context<'_>,
        fetch: &dyn tauri_updater_delta_core::client::Fetch,
        handoff: &dyn InstallHandoff,
    ) -> crate::Result<(Outcome, Option<String>)> {
        crate::flow::run_update_detailed(identity, ctx, fetch, handoff, &|_| {})
            .map(|report| (report.outcome, report.cache_write_error))
    }
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong checking or installing an update.
///
/// Delta reconstruction failures are intentionally absent: they safely fall
/// back to a full download. Security or version-policy refusals remain distinct
/// through [`Error::Refused`], and signature failures never retry through the
/// same unauthenticated release document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The official Tauri updater could not be configured or checked.
    #[error("updater failed: {0}")]
    Updater(String),

    /// Required plugin or application configuration is missing or invalid.
    #[error("delta updater configuration failed: {0}")]
    Configuration(String),

    /// A download failed.
    #[error("{0}")]
    Fetch(String),

    /// A filesystem operation failed.
    #[error("{0}")]
    Io(String),

    /// The updater document was unusable.
    #[error("{0}")]
    Manifest(String),

    /// Final artifact signature verification failed.
    ///
    /// This never falls back. The full path is described by the same release
    /// document, so retrying there would grant a second attempt rather than
    /// provide an independent source of trust.
    #[error("{0}")]
    Signature(String),

    /// The update was refused by authenticated identity or version policy.
    ///
    /// A refusal says the release itself must not be installed. It is distinct
    /// from an unavailable delta, which quietly falls back to Full.
    #[error("update refused: {0}")]
    Refused(Refusal),

    /// The official installer rejected the verified artifact.
    #[error("install failed: {0}")]
    Install(String),

    /// The blocking update task could not complete.
    #[error("update task failed: {0}")]
    Task(String),
}

/// Which source successfully supplied the installed official artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateSource {
    /// The complete official artifact was downloaded.
    Full,
    /// A patch against the compressed official artifact was applied.
    DirectDelta,
    /// A cached official artifact was expanded, patched at the tar layer, and
    /// recompressed byte-for-byte.
    TarDelta,
}

impl UpdateSource {
    /// A short stable label suitable for logs and telemetry owned by the app.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::DirectDelta => "direct-delta",
            Self::TarDelta => "tar-delta",
        }
    }
}

/// A successful check/install result.
///
/// The source variants are intentionally distinct. Correct installed bytes do
/// not prove a differential path ran; callers and the real-app harness can
/// assert the mechanism directly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// No newer update was offered by Tauri's authoritative check.
    UpToDate {
        /// Non-fatal cache observations made during setup.
        diagnostics: Vec<Diagnostic>,
    },
    /// Installed after downloading the complete official artifact.
    ///
    /// This is the expected first-update outcome while the cache has no ACTIVE
    /// base, and the safe fallback when a delta is unavailable or corrupt.
    InstalledFromFullDownload {
        /// Bytes downloaded for the full artifact.
        downloaded: u64,
        /// Non-fatal cache observations. Installation still succeeded.
        diagnostics: Vec<Diagnostic>,
    },
    /// Installed after applying a patch to the compressed official artifact.
    InstalledFromDirectDelta {
        /// Patch bytes downloaded.
        downloaded: u64,
        /// Bytes the corresponding full download would have used.
        full_download_size: u64,
        /// Non-fatal cache observations. Installation still succeeded.
        diagnostics: Vec<Diagnostic>,
    },
    /// Installed after patching the tar layer and exactly recompressing it.
    InstalledFromTarDelta {
        /// Patch bytes downloaded.
        downloaded: u64,
        /// Bytes the corresponding full download would have used.
        full_download_size: u64,
        /// Non-fatal cache observations. Installation still succeeded.
        diagnostics: Vec<Diagnostic>,
    },
}

impl Outcome {
    /// Which install source succeeded, or `None` when already up to date.
    pub fn source(&self) -> Option<UpdateSource> {
        match self {
            Self::UpToDate { .. } => None,
            Self::InstalledFromFullDownload { .. } => Some(UpdateSource::Full),
            Self::InstalledFromDirectDelta { .. } => Some(UpdateSource::DirectDelta),
            Self::InstalledFromTarDelta { .. } => Some(UpdateSource::TarDelta),
        }
    }

    /// A short stable name for the result.
    pub fn path_name(&self) -> &'static str {
        self.source().map_or("up-to-date", UpdateSource::as_str)
    }

    /// Non-fatal diagnostics collected while preserving a successful update.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::UpToDate { diagnostics }
            | Self::InstalledFromFullDownload { diagnostics, .. }
            | Self::InstalledFromDirectDelta { diagnostics, .. }
            | Self::InstalledFromTarDelta { diagnostics, .. } => diagnostics,
        }
    }

    /// Bytes downloaded for the successful source, when an install occurred.
    pub fn downloaded_bytes(&self) -> Option<u64> {
        match self {
            Self::UpToDate { .. } => None,
            Self::InstalledFromFullDownload { downloaded, .. }
            | Self::InstalledFromDirectDelta { downloaded, .. }
            | Self::InstalledFromTarDelta { downloaded, .. } => Some(*downloaded),
        }
    }

    /// Full artifact size used as the delta comparison, when a delta installed.
    pub fn full_download_size(&self) -> Option<u64> {
        match self {
            Self::InstalledFromDirectDelta {
                full_download_size, ..
            }
            | Self::InstalledFromTarDelta {
                full_download_size, ..
            } => Some(*full_download_size),
            Self::UpToDate { .. } | Self::InstalledFromFullDownload { .. } => None,
        }
    }
}

/// A non-fatal condition that may reduce future delta availability.
///
/// Diagnostics never replace a successful outcome. An unwritable or corrupt
/// cache costs a later update its fast path, not this update its correctness.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Diagnostic {
    /// The cache could not be opened or reconciled, so this run proceeded
    /// without a reusable base.
    CacheUnavailable {
        /// Human-readable filesystem or state error.
        message: String,
    },
    /// The installed artifact could not be persisted for a future delta.
    CacheNotPersisted {
        /// Human-readable filesystem or state error.
        message: String,
    },
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheUnavailable { message } => {
                write!(
                    f,
                    "delta cache unavailable; using full fallback when needed: {message}"
                )
            }
            Self::CacheNotPersisted { message } => write!(
                f,
                "update installed, but its artifact was not persisted for a future delta: {message}"
            ),
        }
    }
}

/// Coarse update phases for a progress indicator.
///
/// Percentages are deliberately absent because patch application, exact
/// recompression, and installation do not expose reliable totals. Downloading
/// and reconstruction may repeat when a delta attempt falls back safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgressEvent {
    /// Tauri is fetching and evaluating the updater document.
    Checking,
    /// An artifact or patch is being downloaded.
    Downloading,
    /// A downloaded patch is being validated and applied; for TarDelta this
    /// includes exact recompression.
    Reconstructing,
    /// The final official artifact is being signature- and identity-verified.
    Verifying,
    /// The verified bytes are being handed to Tauri's installer.
    Installing,
    /// The operation completed successfully or no update was available.
    Finished,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_sources_remain_explicit_and_distinguishable() {
        let full = Outcome::InstalledFromFullDownload {
            downloaded: 300,
            diagnostics: Vec::new(),
        };
        let direct = Outcome::InstalledFromDirectDelta {
            downloaded: 80,
            full_download_size: 300,
            diagnostics: Vec::new(),
        };
        let tar = Outcome::InstalledFromTarDelta {
            downloaded: 40,
            full_download_size: 300,
            diagnostics: Vec::new(),
        };

        assert_eq!(full.source(), Some(UpdateSource::Full));
        assert_eq!(direct.source(), Some(UpdateSource::DirectDelta));
        assert_eq!(tar.source(), Some(UpdateSource::TarDelta));
        assert_eq!(full.path_name(), "full");
        assert_eq!(direct.path_name(), "direct-delta");
        assert_eq!(tar.path_name(), "tar-delta");
    }

    #[test]
    fn cache_diagnostics_do_not_replace_a_successful_outcome() {
        let outcome = Outcome::InstalledFromFullDownload {
            downloaded: 300,
            diagnostics: vec![Diagnostic::CacheNotPersisted {
                message: "read-only filesystem".to_owned(),
            }],
        };

        assert_eq!(outcome.source(), Some(UpdateSource::Full));
        assert_eq!(outcome.diagnostics().len(), 1);
    }
}
