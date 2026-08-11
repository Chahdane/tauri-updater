//! Pluggable diff/apply implementations.
//!
//! Delta algorithms trade patch size against memory use against apply speed, and
//! the right balance depends on the artifact. Everything above this layer is
//! written against [`PatchBackend`] so a backend can be swapped without touching
//! the pipeline around it.
//!
//! Each patch records the [`PatchBackend::id`] that produced it, so a client
//! that meets a backend it does not have can decline the patch and fall back to
//! a full download instead of guessing.

use std::path::Path;

use crate::{Error, Result};

pub mod zstd;

pub use self::zstd::ZstdBackend;

/// A diff/apply implementation.
///
/// [`diff`](PatchBackend::diff) runs at release time on CI;
/// [`apply`](PatchBackend::apply) runs on the user's machine. Both sides live in
/// one trait because a backend's two halves must agree on a format, and keeping
/// them together makes that pairing impossible to get wrong.
pub trait PatchBackend: Send + Sync {
    /// Stable identifier recorded in the patch manifest, e.g. `"zstd"`.
    ///
    /// This is a wire format constant. Changing it for an existing backend
    /// breaks every published patch that names it.
    fn id(&self) -> &'static str;

    /// Produce a patch at `patch` that turns `old` into `new`.
    fn diff(&self, old: &Path, new: &Path, patch: &Path) -> Result<()>;

    /// Reconstruct `new` at `out` from `old` and `patch`.
    ///
    /// A successful return does **not** mean the output is correct — a patch is
    /// untrusted input. Callers must still verify the result against the hash
    /// the manifest promised, using [`crate::hash::verify_file`].
    fn apply(&self, old: &Path, patch: &Path, out: &Path) -> Result<()>;
}

/// Look up a backend by the identifier recorded in a patch manifest.
///
/// Returns [`Error::UnknownBackend`] for an id this build cannot apply, which
/// the caller treats like any other delta failure: fall back to a full download.
pub fn backend_for(id: &str) -> Result<Box<dyn PatchBackend>> {
    match id {
        ZstdBackend::ID => Ok(Box::new(ZstdBackend::new())),
        unknown => Err(Error::UnknownBackend(unknown.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_zstd_backend() {
        let backend = backend_for("zstd").expect("zstd should be available");
        assert_eq!(backend.id(), "zstd");
    }

    #[test]
    fn rejects_an_unknown_backend() {
        let Err(err) = backend_for("bsdiff") else {
            panic!("an unbuilt backend should not resolve");
        };
        assert!(matches!(err, Error::UnknownBackend(id) if id == "bsdiff"));
    }
}
