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
