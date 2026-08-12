//! The delta path, with the fallback guarantee expressed in the type system.
//!
//! Everything about this project rests on one promise: *the delta path can make
//! an update faster, but it can never make one fail.* [`try_reconstruct`]
//! enforces that by not being fallible. It returns a [`Reconstruction`], and the
//! only two possibilities are "here is a verified artifact" and "download the
//! full one" — there is no error to accidentally propagate out of the delta path
//! and abort an install.
//!
//! A patch is untrusted input, so a successful decompression proves nothing on
//! its own. Three gates stand between a patch and an install:
//!
//! 1. **Size** — the manifest declares the target installer's length, which also
//!    bounds the memory zstd may allocate before decompression begins.
//! 2. **Digest** — the reconstruction must hash to the value the release process
//!    published.
//! 3. **Signature** — verified by the caller against the same minisign key
//!    Tauri's own updater uses, over that same artifact.
//!
//! Failing any of them costs a wasted download, never a broken install.

use std::path::{Path, PathBuf};

use crate::backend::{backend_for, PatchBackend, ZstdBackend};
use crate::hash::verify_file;
use crate::{Error, FileHash, Result};

/// What the manifest promises about the artifact being rebuilt.
#[derive(Debug, Clone)]
pub struct TargetSpec {
    /// Backend that produced the patch, from the manifest.
    pub backend_id: String,
    /// Exact size of the target installer in bytes.
    pub size: u64,
    /// Digest the reconstruction must match.
    pub digest: FileHash,
}

impl TargetSpec {
    /// Build a spec from the raw manifest fields.
    pub fn new(backend_id: impl Into<String>, size: u64, digest_hex: &str) -> Result<Self> {
        Ok(Self {
            backend_id: backend_id.into(),
            size,
            digest: FileHash::from_hex(digest_hex)?,
        })
    }
}

/// The outcome of attempting the delta path.
///
/// Deliberately not a `Result`: there is no failure mode here that a caller is
/// expected to propagate. Both variants are normal.
#[derive(Debug)]
#[must_use = "ignoring this discards the decision about whether to download the full artifact"]
pub enum Reconstruction {
    /// The artifact was rebuilt and passed every check. Ready for the installer.
    Verified(PathBuf),

    /// The delta path did not work out. Download the full artifact as usual.
    ///
    /// The reason is carried for logging, not for the caller to act on
    /// differently — every reason leads to the same recovery.
    FallBack(Error),
}

impl Reconstruction {
    /// The rebuilt artifact, if the delta path succeeded.
    pub fn verified_path(&self) -> Option<&Path> {
        match self {
            Self::Verified(path) => Some(path),
            Self::FallBack(_) => None,
        }
    }

    /// Why the delta path was abandoned, if it was.
    pub fn fallback_reason(&self) -> Option<&Error> {
        match self {
            Self::Verified(_) => None,
            Self::FallBack(reason) => Some(reason),
        }
    }
}

/// Rebuild an installer from `base` and `patch`, verifying it against `target`.
///
/// Never fails. Anything that goes wrong — an unknown backend, a corrupt or
/// truncated patch, the wrong base version, a size or digest mismatch, a full
/// disk — produces [`Reconstruction::FallBack`].
///
/// The caller is still responsible for the signature check, which needs the
/// public key and therefore belongs to the layer that holds configuration.
pub fn try_reconstruct(
    base: &Path,
    patch: &Path,
    out: &Path,
    target: &TargetSpec,
) -> Reconstruction {
    match reconstruct_inner(base, patch, out, target) {
        Ok(()) => Reconstruction::Verified(out.to_path_buf()),
        Err(reason) => {
            // Never leave a half-written artifact where something might mistake
            // it for a finished download.
            let _ = std::fs::remove_file(out);
            Reconstruction::FallBack(reason)
        }
    }
}

fn reconstruct_inner(base: &Path, patch: &Path, out: &Path, target: &TargetSpec) -> Result<()> {
    // Resolving by id means a client that meets a backend it does not implement
    // declines the patch instead of guessing at the format.
    let backend = backend_for(&target.backend_id)?;

    if backend.id() == ZstdBackend::ID {
        // Bound the window and the output by the size the manifest committed to,
        // rather than by what the patch's own header claims.
        ZstdBackend::new()
            .with_expected_output_bytes(target.size)
            .apply(base, patch, out)?;
    } else {
        backend.apply(base, patch, out)?;
    }

    verify_file(out, &target.digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(size: u64, digest: FileHash) -> TargetSpec {
        TargetSpec {
            backend_id: ZstdBackend::ID.to_owned(),
            size,
            digest,
        }
    }

    #[test]
    fn an_unknown_backend_falls_back_rather_than_erroring() {
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.bin");
        let target = TargetSpec {
            backend_id: "some-future-backend".to_owned(),
            size: 10,
            digest: FileHash::of_bytes(b"whatever"),
        };

        let outcome = try_reconstruct(
            &dir.path().join("base.bin"),
            &dir.path().join("patch.bin"),
            &out,
            &target,
        );

        assert!(matches!(
            outcome.fallback_reason(),
            Some(Error::UnknownBackend(_))
        ));
    }

    #[test]
    fn a_missing_base_falls_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.bin");
        let outcome = try_reconstruct(
            &dir.path().join("absent.bin"),
            &dir.path().join("absent.patch"),
            &out,
            &spec(10, FileHash::of_bytes(b"x")),
        );
        assert!(outcome.verified_path().is_none());
    }

    #[test]
    fn a_failed_reconstruction_leaves_no_artifact_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = dir.path().join("base.bin");
        let patch = dir.path().join("bad.patch");
        let out = dir.path().join("out.bin");
        std::fs::write(&base, b"the base artifact").expect("write base");
        std::fs::write(&patch, b"this is not a zstd frame").expect("write patch");

        let outcome = try_reconstruct(&base, &patch, &out, &spec(17, FileHash::of_bytes(b"x")));

        assert!(outcome.verified_path().is_none());
        assert!(
            !out.exists(),
            "a half-written artifact must not survive a failed reconstruction"
        );
    }
}
