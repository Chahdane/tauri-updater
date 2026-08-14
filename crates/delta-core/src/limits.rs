//! Local ceilings on work the update server can ask for.
//!
//! # Why a second limit exists
//!
//! The manifest already declares how large the target installer is, and
//! [`TargetSpec`](crate::TargetSpec) uses that number to bound both the
//! decompression window and the bytes written. That is a useful bound — it stops
//! a patch expanding past what the release committed to — but it is **not a
//! safety limit**, because the number comes from the same unauthenticated
//! document as everything else (`docs/DECISIONS.md` #13).
//!
//! A manifest saying `target_installer_size: 500000000000` is not describing a
//! release. It is asking us to reserve, write and hash 500 GB. Obeying it because
//! it said so is the whole vulnerability, and no digest or signature check helps:
//! the resources are spent long before either runs.
//!
//! So there are two ceilings, and the outer one is ours:
//!
//! ```text
//! bytes actually written  ≤  declared target size  ≤  max_target_bytes
//!         (enforced by zstd)      (from the manifest)     (from here)
//! ```
//!
//! The left inequality stops a patch exceeding what the manifest promised. The
//! right one stops the manifest promising something absurd. Only the second is
//! outside the server's control, which is what makes it the one that matters.
//!
//! # Why one ceiling was not enough
//!
//! Gate B established that pattern against a single quantity: the compressed
//! installer. The tar layer then added a pipeline behind it, and each stage has
//! its own declared size from the same unauthenticated document:
//!
//! ```text
//! cached .app.tar.gz  ->  base tar  ->  + patch  ->  target tar  ->  installer
//!   max_blob_bytes      max_tar_bytes              max_tar_bytes    max_target_bytes
//!    (CacheLimits)       (CacheLimits)               (HERE)           (here)
//! ```
//!
//! The stages are capped **independently** rather than by one number and a
//! ratio, because every arrow above can expand without bound: gzip's ratio is
//! unbounded in principle, and so is a patch's. A cap derived from an earlier
//! stage inherits exactly the unboundedness it was supposed to remove.
//!
//! [`Limits::max_tar_bytes`] is the one this module owns: the **reconstructed
//! target** tar, the only stage whose declared size used to reach zstd's window
//! and output bound with nothing local above it.
//!
//! It is deliberately a separate dial from
//! [`CacheLimits::max_tar_bytes`](crate::cache::CacheLimits::max_tar_bytes),
//! which bounds *expanding a blob this client already verified and cached*. Same
//! unit, different question and different provenance, so collapsing them would
//! make one host policy answer two.

/// Largest target installer this build will attempt to reconstruct, in bytes.
///
/// Chosen to sit above any plausible desktop installer while staying far below
/// "fills the disk". A release genuinely larger than this needs the limit raised
/// deliberately, which is the right amount of friction for a number whose whole
/// job is to be a backstop.
pub const DEFAULT_MAX_TARGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Largest uncompressed tar this build will reconstruct, in bytes.
///
/// Four times [`DEFAULT_MAX_TARGET_BYTES`], matching the cache's own default,
/// because an uncompressed `.app` is legitimately several times its `.tar.gz`.
/// Not derived from it at runtime: see the module docs on why a ratio-derived
/// cap inherits the unboundedness it exists to remove.
pub const DEFAULT_MAX_TAR_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Ceilings the host imposes, regardless of what a manifest asks for.
///
/// Deliberately a plain value type with public fields: these are policy dials a
/// host sets once, not a capability, and nothing is enforced by their existence.
/// The enforcement is at each use site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest declared target installer size that will be attempted.
    ///
    /// A manifest declaring more than this is refused before a patch is
    /// downloaded, let alone applied.
    pub max_target_bytes: u64,

    /// Largest declared **uncompressed target tar** that will be reconstructed.
    ///
    /// Only the tar layer reaches this. The declared value bounds zstd's window
    /// and its output, so without a local ceiling above it a manifest could ask
    /// this client to reserve and write an arbitrary amount — the same attack
    /// `max_target_bytes` exists to stop, one representation further in.
    pub max_tar_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_target_bytes: DEFAULT_MAX_TARGET_BYTES,
            max_tar_bytes: DEFAULT_MAX_TAR_BYTES,
        }
    }
}

impl Limits {
    /// Check a manifest-declared target size against the local ceiling.
    pub fn check_target_size(&self, declared: u64) -> crate::Result<()> {
        if declared > self.max_target_bytes {
            return Err(crate::Error::DeclaredSizeTooLarge {
                declared,
                limit: self.max_target_bytes,
            });
        }
        Ok(())
    }

    /// Check a manifest-declared uncompressed tar size against the local
    /// ceiling.
    ///
    /// Called before the base is expanded and before the patch is fetched, so an
    /// absurd declaration costs one comparison rather than a download.
    pub fn check_tar_size(&self, declared: u64) -> crate::Result<()> {
        if declared > self.max_tar_bytes {
            return Err(crate::Error::DeclaredSizeTooLarge {
                declared,
                limit: self.max_tar_bytes,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn an_ordinary_installer_is_within_the_default() {
        // ~85 MB, a normal desktop bundle.
        Limits::default()
            .check_target_size(85 * 1024 * 1024)
            .expect("a normal installer must not be refused");
    }

    #[test]
    fn a_size_at_the_limit_is_allowed() {
        let limits = Limits {
            max_target_bytes: 1000,
            ..Limits::default()
        };
        limits
            .check_target_size(1000)
            .expect("the limit is inclusive");
    }

    #[test]
    fn an_absurd_declared_size_is_refused() {
        // The attack this exists for: the manifest asks for half a terabyte.
        let err = Limits::default()
            .check_target_size(500 * 1024 * 1024 * 1024)
            .expect_err("500 GB must be refused");

        match err {
            Error::DeclaredSizeTooLarge { declared, limit } => {
                assert_eq!(declared, 500 * 1024 * 1024 * 1024);
                assert_eq!(limit, DEFAULT_MAX_TARGET_BYTES);
            }
            other => panic!("expected DeclaredSizeTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn the_ceiling_is_configurable() {
        let limits = Limits {
            max_target_bytes: 10,
            ..Limits::default()
        };
        assert!(limits.check_target_size(11).is_err());
        assert!(limits.check_target_size(10).is_ok());
    }

    #[test]
    fn the_tar_ceiling_is_separate_from_the_installer_ceiling() {
        // The two dials must not shadow each other. A host that raised the
        // installer ceiling has said nothing about how large an uncompressed
        // tar it is willing to reconstruct, and vice versa — which is the whole
        // reason there are two.
        let limits = Limits {
            max_target_bytes: u64::MAX,
            max_tar_bytes: 1000,
        };
        limits
            .check_target_size(500 * 1024 * 1024 * 1024)
            .expect("the installer ceiling was raised deliberately");
        assert!(
            limits.check_tar_size(1001).is_err(),
            "raising the installer ceiling must not raise the tar ceiling"
        );
        limits.check_tar_size(1000).expect("the limit is inclusive");
    }

    #[test]
    fn an_absurd_declared_tar_is_refused() {
        // The tar-layer form of the same attack: 900 GB of uncompressed tar,
        // which would otherwise become zstd's window and output bound.
        let err = Limits::default()
            .check_tar_size(900 * 1024 * 1024 * 1024)
            .expect_err("900 GB must be refused");

        match err {
            Error::DeclaredSizeTooLarge { declared, limit } => {
                assert_eq!(declared, 900 * 1024 * 1024 * 1024);
                assert_eq!(limit, DEFAULT_MAX_TAR_BYTES);
            }
            other => panic!("expected DeclaredSizeTooLarge, got {other:?}"),
        }
    }
}
