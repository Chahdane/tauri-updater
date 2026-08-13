//! The persistent artifact cache.
//!
//! Two kinds of state, deliberately separated because they have different
//! concurrency properties:
//!
//! - **Blobs** are content-addressed and immutable. Two processes that compute
//!   the same artifact write identical bytes, so races over them cost duplicated
//!   work and nothing else. See [`blob`].
//! - **State** — which entry is ACTIVE and which is PENDING — is mutable, and is
//!   the only place a race can lose information. It lives in [`state`], behind a
//!   compare-and-set.
//!
//! See `docs/DECISIONS.md` #23 and #24.
//!
//! # Why there is a PENDING state at all
//!
//! The cache exists to hold *the artifact the user is currently running*, because
//! that is the only correct base for the next patch. So the question the state
//! machine answers is not "did the update succeed?" but "which version is
//! actually running now?", and those are different questions:
//!
//! ```text
//! Update::install() == Ok    ->  the installer reported success
//! the app relaunched as v'   ->  v' is what is running
//! ```
//!
//! Only the second licenses a promotion. `install()` returning `Ok` means bytes
//! were written; the user may never restart, the launch may fail, an OS update
//! may roll the bundle back, or another updater may install something else
//! entirely. Promoting on `Ok` would leave ACTIVE naming an artifact the user is
//! not running, and every subsequent patch would be computed against the wrong
//! base — silently, because the manifest's base digest would match the *cache*
//! and the real installation would simply never receive an update it could
//! apply.
//!
//! So a freshly verified artifact is staged as PENDING, and promotion happens on
//! the next launch, from the version the running process reports about itself.
//!
//! ```text
//! EMPTY ────────────────── verified v' ─────────────────▶ PENDING(v')
//! ACTIVE(v) ───────────── verified v' ─────────────────▶ ACTIVE(v) + PENDING(v')
//!
//! next launch, running == v'   ─▶  ACTIVE(v')            (promote)
//! next launch, running != v'   ─▶  ACTIVE(v) unchanged   (discard PENDING)
//! ```
//!
//! A failed install therefore needs no special handling at all: PENDING survives
//! until the next launch, that launch is still running the old version, and the
//! reconciliation discards it. There is no install-result signal to plumb
//! through, which means there is no install-result signal to get wrong.

pub mod blob;
pub mod state;

use std::path::{Path, PathBuf};

pub use blob::BlobStore;
pub use state::{
    CacheEntry, CacheState, CasError, Namespace, StateStore, Versioned, CACHE_FORMAT_VERSION,
};

use crate::recompress::decompress_bytes_bounded;
use crate::signature::VerifiedArtifact;
use crate::{Error, FileHash, Result};

/// Ceilings the host imposes on cached artifacts, regardless of what a manifest
/// asks for.
///
/// Separate from [`crate::Limits`] because they bound different things. `Limits`
/// bounds what a *server* may ask this host to reconstruct. These bound what a
/// **local file** may cost to read and expand — and the local file is untrusted
/// too, because the cache directory is writable by the user, by other processes
/// and by anything that has ever run as this user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    /// Largest cached compressed artifact that will be read at all.
    pub max_blob_bytes: u64,
    /// Largest tar that will be written when expanding one.
    ///
    /// Independent of `max_blob_bytes` rather than a multiple of it, because
    /// gzip's expansion ratio is unbounded: a small blob can describe an
    /// arbitrarily large tar, and a ratio-derived limit would inherit that.
    pub max_tar_bytes: u64,

    /// How old a blob must be before garbage collection will remove it.
    ///
    /// # The race this closes, and the one it does not
    ///
    /// Staging is two steps: write the blob, then commit the entry naming it.
    /// Between them the blob is referenced by nothing, so a collection pass in
    /// another process is entitled to delete it — and the stager then commits an
    /// entry pointing at a file that is gone.
    ///
    /// A lock would close that, and would be the wrong trade: it would serialise
    /// every update against every other one to prevent an outcome whose cost is
    /// a wasted download (`docs/DECISIONS.md` #18). An age floor closes it for
    /// any staging that completes within the window, which is all of them —
    /// the gap is one `hard_link` — while keeping collection lock-free.
    ///
    /// What it does **not** close is the accepted reconciliation race: a launch
    /// may discard a PENDING another process staged moments earlier. That one is
    /// about *state*, not blobs, and its consequence is the same cache miss.
    pub blob_grace: std::time::Duration,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 2 * 1024 * 1024 * 1024,
            max_tar_bytes: 8 * 1024 * 1024 * 1024,
            blob_grace: std::time::Duration::from_secs(60),
        }
    }
}

/// What a reconciliation pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The application is running the version that was staged, so it is now the
    /// base for future patches.
    Promoted {
        /// Version promoted to ACTIVE.
        version: String,
    },
    /// A staged artifact was not what came back up. It is dropped and the
    /// previous ACTIVE is left exactly as it was.
    DiscardedPending {
        /// Version that had been staged.
        staged: String,
        /// Version actually running.
        running: String,
    },
    /// Nothing was staged. The ordinary case on most launches.
    NothingStaged,
    /// The cache described a different installation and was emptied.
    ///
    /// A mismatch means cache *miss*, never repair: the artifacts in it may be
    /// perfectly valid for something that is not this application, this
    /// architecture, or this signing key.
    NamespaceReset,
}

/// The verified ACTIVE artifact and the state entry describing it.
#[derive(Debug)]
pub struct ActiveBase {
    /// What the state records about it.
    pub entry: CacheEntry,
    /// The bytes, re-hashed and re-verified against the current public key.
    pub artifact: VerifiedArtifact,
}

/// Blobs, state and scratch space for one installation.
pub struct ArtifactCache {
    root: PathBuf,
    blobs: BlobStore,
    state: StateStore,
    namespace: Namespace,
    limits: CacheLimits,
}

/// How many times a compare-and-set will be retried before giving up.
///
/// A bound rather than an unbounded loop: contention here is a handful of
/// processes, so exceeding this means something is wrong, and the correct
/// response to "something is wrong with the cache" is a cache miss and a full
/// download — not spinning.
const MAX_CAS_ATTEMPTS: usize = 64;

impl ArtifactCache {
    /// Open (or create) a cache under `root`.
    ///
    /// Layout:
    ///
    /// ```text
    /// <root>/blobs/<blake3>.tar.gz     immutable, content-addressed
    /// <root>/state.<generation>.json   mutable, compare-and-set
    /// <root>/tmp/<transaction>/        per-update scratch
    /// ```
    pub fn open(root: impl AsRef<Path>, namespace: Namespace, limits: CacheLimits) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let blobs = BlobStore::open(root.join("blobs"))?;
        let state = StateStore::open(&root)?;
        std::fs::create_dir_all(root.join("tmp"))
            .map_err(|e| Error::io("create", root.join("tmp"), e))?;
        Ok(Self {
            root,
            blobs,
            state,
            namespace,
            limits,
        })
    }

    /// The blob store, for reporting and cleanup.
    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    /// A scratch directory for one update, removed when the value is dropped.
    pub fn scratch(&self) -> Result<tempfile::TempDir> {
        let tmp = self.root.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| Error::io("create", &tmp, e))?;
        tempfile::Builder::new()
            .prefix("update-")
            .tempdir_in(&tmp)
            .map_err(|e| Error::io("create", &tmp, e))
    }

    /// Current state, resetting it first if it describes another installation.
    fn current(&self) -> Result<(Versioned, bool)> {
        let current = self.state.initialise(self.namespace.clone())?;
        if current.state.namespace == self.namespace {
            return Ok((current, false));
        }
        let fresh = CacheState::empty(self.namespace.clone());
        match self.state.compare_and_set(current.generation, &fresh) {
            Ok(generation) => {
                // The old blobs belong to the old namespace. Dropping them is
                // not an optimisation: keeping artifacts signed by a retired key
                // around is exactly what the fingerprint binding exists to
                // prevent.
                let _ = self.blobs.retain(&[], std::time::Duration::ZERO);
                Ok((
                    Versioned {
                        generation,
                        state: fresh,
                    },
                    true,
                ))
            }
            // Somebody else reset it, or moved it on. Re-read and take theirs.
            Err(CasError::Conflict) => Ok((self.state.initialise(self.namespace.clone())?, true)),
            Err(CasError::Failed(e)) => Err(e),
        }
    }

    /// Bring the cache into line with the version actually running.
    ///
    /// Call this **on launch**, before any update is planned, with the version
    /// the running process reports about *itself* — never a version read from a
    /// manifest, a filename or the cache. The whole mechanism rests on that
    /// value being an observation rather than an expectation.
    pub fn reconcile(&self, running_version: &str) -> Result<Reconciliation> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let (current, was_reset) = self.current()?;

            let Some(pending) = current.state.pending.clone() else {
                self.collect(&current.state);
                return Ok(if was_reset {
                    Reconciliation::NamespaceReset
                } else {
                    Reconciliation::NothingStaged
                });
            };

            let mut next = current.state.clone();
            let outcome = if pending.version == running_version {
                next.active = Some(pending.clone());
                next.pending = None;
                Reconciliation::Promoted {
                    version: pending.version.clone(),
                }
            } else {
                // The staged artifact is not what came back up: the install
                // failed, was superseded, or never happened. ACTIVE is left
                // untouched, which is the point — it still describes what is
                // running.
                next.pending = None;
                Reconciliation::DiscardedPending {
                    staged: pending.version.clone(),
                    running: running_version.to_owned(),
                }
            };

            match self.state.compare_and_set(current.generation, &next) {
                Ok(_) => {
                    self.collect(&next);
                    let _ = self.state.prune(3);
                    return Ok(outcome);
                }
                Err(CasError::Conflict) => continue,
                Err(CasError::Failed(e)) => return Err(e),
            }
        }
        Err(Error::Manifest(
            "cache state could not be reconciled under contention".to_owned(),
        ))
    }

    /// Record a verified artifact as PENDING.
    ///
    /// Takes a [`VerifiedArtifact`], so bytes that have not passed signature
    /// verification cannot be staged — the same gate the installer sits behind.
    /// The digest and size are computed here from those bytes rather than taken
    /// on trust from a caller or a manifest, because they are what the blob is
    /// addressed by.
    ///
    /// The tar inside is expanded once, under the local ceiling, to record its
    /// digest. That costs one decompression per update and buys the ability to
    /// reject a mismatched base later without doing it again.
    ///
    /// ACTIVE is never touched. If staging fails at any point the previous
    /// ACTIVE is exactly as it was.
    pub fn stage_pending(
        &self,
        version: &str,
        artifact: &VerifiedArtifact,
        signature_b64: &str,
    ) -> Result<CacheEntry> {
        let compressed_size = artifact.len() as u64;
        if compressed_size > self.limits.max_blob_bytes {
            return Err(Error::DeclaredSizeTooLarge {
                declared: compressed_size,
                limit: self.limits.max_blob_bytes,
            });
        }

        let scratch = self.scratch()?;
        let tar = scratch.path().join("staged.tar");
        let tar_size =
            decompress_bytes_bounded(artifact.as_bytes(), &tar, self.limits.max_tar_bytes)?;
        let tar_blake3 = FileHash::of_file(&tar)?.to_hex();
        drop(scratch);

        let compressed_blake3 = self.blobs.put(artifact)?.to_hex();

        let entry = CacheEntry {
            version: version.to_owned(),
            compressed_blake3,
            compressed_size,
            tar_blake3,
            tar_size,
            signature: signature_b64.to_owned(),
        };

        for _ in 0..MAX_CAS_ATTEMPTS {
            let (current, _) = self.current()?;
            let mut next = current.state.clone();
            next.pending = Some(entry.clone());
            match self.state.compare_and_set(current.generation, &next) {
                Ok(_) => {
                    let _ = self.state.prune(3);
                    return Ok(entry);
                }
                Err(CasError::Conflict) => continue,
                Err(CasError::Failed(e)) => return Err(e),
            }
        }
        Err(Error::Manifest(
            "cache state could not be updated under contention".to_owned(),
        ))
    }

    /// The state as it stands, without changing it.
    pub fn state(&self) -> Result<CacheState> {
        Ok(self.current()?.0.state)
    }

    /// Load and re-verify the ACTIVE artifact.
    ///
    /// `Ok(None)` means there is no ACTIVE entry — the ordinary first-run case.
    /// `Err` means there was one and it did not survive re-verification, which
    /// is a cache miss and never a reason to abort an update.
    pub fn active(&self, pubkey_b64: &str) -> Result<Option<ActiveBase>> {
        let Some(entry) = self.current()?.0.state.active else {
            return Ok(None);
        };
        let artifact = self.blobs.get(
            &entry.compressed_blake3,
            entry.compressed_size,
            &entry.signature,
            pubkey_b64,
            self.limits.max_blob_bytes,
        )?;
        Ok(Some(ActiveBase { entry, artifact }))
    }

    /// Expand a verified artifact to the exact tar it contains, checking the
    /// result against what the manifest declares.
    ///
    /// Expands the artifact's **own bytes**, not the file they came from, so
    /// the tar is derived from what was actually verified. The declared size
    /// only ever lowers the local ceiling, never raises it.
    pub fn expand_to_tar(
        &self,
        artifact: &VerifiedArtifact,
        out: &Path,
        expected_tar_blake3: &str,
        expected_tar_size: u64,
    ) -> Result<()> {
        let expected = FileHash::from_hex(expected_tar_blake3)?;
        if expected_tar_size > self.limits.max_tar_bytes {
            return Err(Error::DeclaredSizeTooLarge {
                declared: expected_tar_size,
                limit: self.limits.max_tar_bytes,
            });
        }

        let ceiling = expected_tar_size.min(self.limits.max_tar_bytes);
        let written = decompress_bytes_bounded(artifact.as_bytes(), out, ceiling)?;
        if written != expected_tar_size {
            let _ = std::fs::remove_file(out);
            return Err(Error::UnexpectedOutputSize {
                expected: expected_tar_size,
                actual: written,
            });
        }

        let actual = FileHash::of_file(out)?;
        if actual != expected {
            let _ = std::fs::remove_file(out);
            return Err(Error::ChecksumMismatch {
                path: out.to_path_buf(),
                expected: expected.to_hex(),
                actual: actual.to_hex(),
            });
        }
        Ok(())
    }

    /// Drop blobs no live entry names, leaving recently written ones alone.
    ///
    /// Best-effort by design: a collection failure is not a reason to fail an
    /// update, so the result is discarded. The worst case is a cache that is
    /// larger than it needs to be.
    fn collect(&self, state: &CacheState) {
        let mut keep = Vec::new();
        if let Some(active) = &state.active {
            keep.push(active.compressed_blake3.as_str());
        }
        if let Some(pending) = &state.pending {
            keep.push(pending.compressed_blake3.as_str());
        }
        let _ = self.blobs.retain(&keep, self.limits.blob_grace);
    }
}

#[cfg(test)]
mod tests;
