//! Error type for the delta engine.

use std::path::{Path, PathBuf};

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong while producing, applying or verifying a patch.
///
/// Every variant is recoverable from the caller's point of view: the plugin
/// responds to any of them by discarding the delta and falling back to a full
/// download, so no error here can break an install.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed. `operation` names what was being
    /// attempted so the message stays useful without a backtrace.
    #[error("failed to {operation} `{}`", path.display())]
    Io {
        /// What was being attempted, e.g. `"read"` or `"create"`.
        operation: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A hash was not a valid lowercase hex digest of the expected length.
    #[error("invalid hash `{value}`: {reason}")]
    InvalidHash {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A file's contents did not match the digest the manifest promised.
    ///
    /// On the delta path this means the reconstruction is wrong and the result
    /// must be discarded.
    #[error("checksum mismatch for `{}`: expected {expected}, got {actual}", path.display())]
    ChecksumMismatch {
        /// The file that was verified.
        path: PathBuf,
        /// The digest the manifest promised.
        expected: String,
        /// The digest the file actually has.
        actual: String,
    },

    /// A patch backend failed while producing or applying a patch.
    #[error("{backend} backend failed to {operation}: {message}")]
    Backend {
        /// Identifier of the backend that failed.
        backend: &'static str,
        /// What it was doing.
        operation: &'static str,
        /// The backend's own description of the failure.
        message: String,
    },

    /// The manifest named a backend this build cannot apply.
    ///
    /// Expected whenever a client is older than the release tooling, so it is
    /// handled the same way as any other delta failure rather than surfaced.
    #[error("unknown patch backend `{0}`")]
    UnknownBackend(String),

    /// The patch ended mid-frame — truncated download or corrupt file.
    #[error("patch is truncated")]
    TruncatedPatch,

    /// Applying the patch would write more than the configured ceiling.
    ///
    /// Guards against a hostile patch crafted to fill the user's disk.
    #[error("patch output exceeds the {limit} byte limit")]
    OutputTooLarge {
        /// The ceiling that was hit, in bytes.
        limit: u64,
    },

    /// Reconstruction finished but produced the wrong number of bytes.
    ///
    /// The manifest states exactly how large the target installer is, so a
    /// mismatch is caught before the digest is even computed.
    #[error("reconstructed {actual} bytes, manifest declares {expected}")]
    UnexpectedOutputSize {
        /// Size the manifest promised.
        expected: u64,
        /// Size actually produced.
        actual: u64,
    },

    /// A manifest was malformed, unsupported, or internally inconsistent.
    #[error("invalid manifest: {0}")]
    Manifest(String),

    /// The minisign signature over the target installer did not verify.
    #[error("signature verification failed: {0}")]
    Signature(String),

    /// A download failed.
    ///
    /// Carries the host transport's own message, since this layer does nothing
    /// with a network failure except fall back.
    #[error("fetch failed: {0}")]
    Fetch(String),
}

impl Error {
    /// Build an [`Error::Io`] with the path and operation attached.
    pub(crate) fn io(
        operation: &'static str,
        path: impl AsRef<Path>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}
