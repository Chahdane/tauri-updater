//! The update flow, from a checked update to a verified artifact handed to an installer.
//!
//! Both boundaries are traits — [`Fetch`] for the network,
//! [`InstallHandoff`] for the platform installer — so this whole path is
//! exercised in tests with no server, no Tauri runtime and no filesystem
//! installer. The Tauri-specific implementations are thin wrappers around them.
//!
//! # The safety invariant, end to end
//!
//! Three things cannot be expressed here, by construction rather than by care:
//!
//! - **An update that fails.** [`plan_update`] returns an `UpdateSource`, so a
//!   delta failure resolves to a full download rather than an error.
//! - **An unverified install.** [`InstallHandoff::install`] takes a
//!   [`VerifiedArtifact`], which only `verify_artifact` can produce. Since
//!   Tauri does not verify what it is handed (see `docs/DECISIONS.md` #10),
//!   this is the sole gate — so it is the compiler's job, not a reviewer's.
//! - **A second opinion about which release this is.** [`run_update`] takes an
//!   [`UpdateIdentity`] and performs **no manifest fetch of its own**. There is
//!   no URL parameter here to point somewhere else, so the release planned,
//!   verified and installed is necessarily the one Tauri checked. See
//!   `docs/DECISIONS.md` #13.

use std::path::Path;

use tauri_updater_delta_core::cache::ArtifactCache;
use tauri_updater_delta_core::client::{plan_update, Fetch, PlanContext, UpdateSource};
use tauri_updater_delta_core::release_identity::current_platform;
use tauri_updater_delta_core::{
    verify_artifact, FileHash, Limits, Refusal, UpdateIdentity, VerifiedArtifact,
};

use crate::{Error, Result};

/// Hands a verified artifact to whatever actually installs it.
///
/// Takes a [`VerifiedArtifact`] rather than bytes: an unverified install is not
/// something a caller can express.
pub trait InstallHandoff {
    /// Install the artifact. The signature has already been checked.
    fn install(&self, artifact: &VerifiedArtifact) -> Result<()>;
}

/// What an update run did.
///
/// The variants name the **mechanism**, not just the result, and that is
/// load-bearing. A delta updater that quietly falls back on every update is
/// indistinguishable from a working one by its installed bytes alone; the only
/// thing that tells them apart is an assertion about which path ran
/// (`docs/DECISIONS.md` #22).
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Installed from a **tar-layer** patch: a cached base was expanded,
    /// patched, and recompressed into the published artifact.
    InstalledFromTarDelta {
        /// Bytes downloaded.
        downloaded: u64,
        /// Bytes a full download would have cost.
        saved_against: u64,
    },
    /// Installed from a patch against the compressed artifact.
    InstalledFromDelta {
        /// Bytes downloaded.
        downloaded: u64,
        /// Bytes a full download would have cost.
        saved_against: u64,
    },
    /// Installed after downloading the whole artifact.
    InstalledFromFullDownload,
    /// The release is already installed.
    UpToDate,
}

impl Outcome {
    /// A short, stable name for the path taken, matching
    /// [`UpdateSource::path_name`].
    pub fn path_name(&self) -> &'static str {
        match self {
            Self::InstalledFromTarDelta { .. } => "tar-delta",
            Self::InstalledFromDelta { .. } => "delta",
            Self::InstalledFromFullDownload => "full",
            Self::UpToDate => "up-to-date",
        }
    }
}

/// Everything the flow needs to know about the host.
///
/// Note what is *not* here: the platform, the version being installed, and the
/// manifest URL. All three come from the [`UpdateIdentity`], because all three
/// describe the release rather than the host — and a release described from two
/// places is a release that can be described two ways.
pub struct Context<'a> {
    /// Base64 minisign public key, as in `tauri.conf.json`.
    pub pubkey: &'a str,
    /// Previously cached installer, if any.
    pub base: Option<&'a Path>,
    /// The artifact cache, when the host keeps one.
    ///
    /// Supplies the base for the tar path, and receives every verified artifact
    /// as PENDING. `None` disables both — the flow then behaves exactly as it
    /// did before the cache existed.
    pub cache: Option<&'a ArtifactCache>,
    /// Scratch directory. Each update gets its own subdirectory inside it, so
    /// concurrent runs cannot consume one another's files.
    pub work_dir: &'a Path,
    /// Application bundle identifier, as in `tauri.conf.json`'s `identifier`.
    ///
    /// Checked against the identity the release signature authenticates, so an
    /// artifact belonging to another application signed with the same key is
    /// refused instead of installed.
    pub app_id: &'a str,
    /// Local ceilings on what a manifest may ask this host to do.
    pub limits: Limits,
}

/// Take the cheapest safe path to the release `identity` names, and install it.
///
/// The delta path is attempted when it can be; anything wrong with it results in
/// the full download the official updater would have performed anyway. A
/// downgrade or an identity mismatch is refused outright instead — see
/// [`Error::Refused`].
pub fn run_update(
    identity: &UpdateIdentity,
    ctx: &Context<'_>,
    fetch: &dyn Fetch,
    handoff: &dyn InstallHandoff,
) -> Result<Outcome> {
    std::fs::create_dir_all(ctx.work_dir)
        .map_err(|e| Error::Io(format!("creating {}: {e}", ctx.work_dir.display())))?;

    let source = plan_update(
        identity,
        &PlanContext {
            base: ctx.base,
            cache: ctx.cache,
            pubkey: ctx.pubkey,
            app_id: ctx.app_id,
            work_dir: ctx.work_dir,
            limits: ctx.limits,
        },
        fetch,
    );

    match source {
        UpdateSource::UpToDate => Ok(Outcome::UpToDate),

        // Never a fallback. The full-download path is described by the same
        // document that just failed the check, so there is nothing safer to
        // reach for.
        UpdateSource::Refused { reason } => Err(Error::Refused(reason)),

        UpdateSource::TarDelta {
            artifact,
            signature,
            downloaded,
            would_have_downloaded,
            _workspace,
        } => {
            let verified = verify_rebuilt(&artifact, &signature, ctx.pubkey)?;
            check_release_identity(ctx, identity, &verified)?;
            stage(ctx, identity, &verified, &signature);
            handoff.install(&verified)?;
            Ok(Outcome::InstalledFromTarDelta {
                downloaded,
                saved_against: would_have_downloaded,
            })
        }

        UpdateSource::Delta {
            artifact,
            signature,
            downloaded,
            would_have_downloaded,
            _workspace,
        } => {
            let verified = verify_rebuilt(&artifact, &signature, ctx.pubkey)?;
            check_release_identity(ctx, identity, &verified)?;
            stage(ctx, identity, &verified, &signature);
            handoff.install(&verified)?;
            // No explicit cleanup: dropping _workspace removes the whole
            // per-update directory, artifact included.
            Ok(Outcome::InstalledFromDelta {
                downloaded,
                saved_against: would_have_downloaded,
            })
        }

        UpdateSource::Full { url, signature, .. } => {
            let full = ctx.work_dir.join("full.artifact");
            fetch
                .fetch(&url, &full)
                .map_err(|e| Error::Fetch(format!("downloading the full artifact: {e}")))?;

            let bytes = std::fs::read(&full)
                .map_err(|e| Error::Io(format!("reading the downloaded artifact: {e}")))?;
            let verified = verify_artifact(bytes, &signature, ctx.pubkey)
                .map_err(|e| Error::Signature(e.to_string()))?;
            check_release_identity(ctx, identity, &verified)?;

            stage(ctx, identity, &verified, &signature);
            handoff.install(&verified)?;
            let _ = std::fs::remove_file(&full);
            Ok(Outcome::InstalledFromFullDownload)
        }
    }
}

/// Signature-check an artifact this crate rebuilt.
///
/// The reconstruction already matched the manifest's digest. This is the
/// separate question of whether the release process actually published it — and
/// nothing else will ask it, because Tauri does not verify what it installs
/// (`docs/DECISIONS.md` #10).
fn verify_rebuilt(
    artifact: &Path,
    signature: &str,
    pubkey: &str,
) -> Result<tauri_updater_delta_core::VerifiedArtifact> {
    let bytes = std::fs::read(artifact)
        .map_err(|e| Error::Io(format!("reading the rebuilt artifact: {e}")))?;

    verify_artifact(bytes, signature, pubkey).map_err(|_| {
        // The artifact matched the digest this document published but not the
        // signature it published alongside it. That does not prove the document
        // is forged — it is not signed, so there was never a claim to disprove.
        // What it does mean is that something in the chain that produced this
        // release is wrong, and we cannot tell what. Falling back would fetch a
        // second artifact chosen by the same document and check it against a
        // signature from that same document, which grants a second attempt
        // rather than a safer one. See docs/DECISIONS.md #11.
        Error::Signature("the rebuilt artifact did not match the manifest's signature".to_owned())
    })
}

/// The authoritative release-identity check.
///
/// Runs on [`VerifiedArtifact::binding`], which exists only because the
/// signature verified — so unlike the planner's advisory read, this one is
/// checking an authenticated statement. It also compares the digest and size
/// against the **bytes actually in hand** rather than against the manifest's
/// claims about them, which is the difference between binding the identity to
/// the artifact and binding two unauthenticated numbers to each other.
///
/// A contradiction here is [`Error::Refused`], never a fallback. The
/// full-download path is pointed at that same artifact by that same document,
/// so retrying there would run the contradiction down the other branch.
///
/// A [`ReleaseBinding::Legacy`] release reaches this point only on the full
/// path — the planner makes the delta paths unavailable for one — and is
/// allowed through, which is exactly what a stock Tauri client would do.
fn check_release_identity(
    ctx: &Context<'_>,
    identity: &UpdateIdentity,
    verified: &VerifiedArtifact,
) -> Result<()> {
    let Some(signed) = verified.binding().identity() else {
        return Ok(());
    };
    signed
        .check(
            ctx.app_id,
            identity.version(),
            &current_platform(),
            &FileHash::of_bytes(verified.as_bytes()).to_hex(),
            verified.len() as u64,
        )
        .map_err(|e| {
            Error::Refused(Refusal::ReleaseIdentity {
                reason: e.to_string(),
            })
        })
}

/// Record a verified artifact as the cache's PENDING entry.
///
/// Runs **before** the install, deliberately. The artifact is fully verified by
/// this point, and staging it first means a process that dies during the install
/// still has the bytes it just paid for — while the promotion rule keeps that
/// from being mistaken for a successful update, because promotion asks what is
/// running rather than what was installed.
///
/// Failures are swallowed. A cache that cannot be written costs the *next*
/// update its patch; failing this one instead would make an optimisation into a
/// dependency, which is the thing this whole design refuses to do.
fn stage(
    ctx: &Context<'_>,
    identity: &UpdateIdentity,
    verified: &tauri_updater_delta_core::VerifiedArtifact,
    signature: &str,
) {
    let Some(cache) = ctx.cache else {
        return;
    };
    let _ = cache.stage_pending(identity.version(), verified, signature);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use tauri_updater_delta_core::Refusal;

    struct FakeServer(HashMap<String, Vec<u8>>);

    impl Fetch for FakeServer {
        fn fetch(&self, url: &str, out: &Path) -> std::result::Result<(), String> {
            let body = self.0.get(url).ok_or_else(|| format!("404 {url}"))?;
            std::fs::write(out, body).map_err(|e| e.to_string())
        }
    }

    /// Records what it was asked to install, so tests can assert on the bytes
    /// that actually reached the installer.
    #[derive(Default)]
    struct RecordingHandoff {
        installed: RefCell<Vec<Vec<u8>>>,
    }

    impl InstallHandoff for RecordingHandoff {
        fn install(&self, artifact: &VerifiedArtifact) -> Result<()> {
            self.installed
                .borrow_mut()
                .push(artifact.as_bytes().to_vec());
            Ok(())
        }
    }

    /// A minimal manifest with no delta layer, describing one release.
    fn manifest_json(version: &str, url: &str, signature: &str) -> String {
        format!(
            r#"{{"version":"{version}","platforms":{{"linux-x86_64":{{"url":"{url}","signature":"{signature}"}}}}}}"#
        )
    }

    fn identity(current: &str, target: &str) -> UpdateIdentity {
        UpdateIdentity::new(
            current,
            target,
            "linux-x86_64",
            "https://example.com/full.bin",
            "a-signature",
            manifest_json(target, "https://example.com/full.bin", "a-signature"),
        )
    }

    // The full harness lives in tests/update_flow.rs, which builds a real
    // manifest with the release tooling. These cover the paths that need no
    // signing key.

    #[test]
    fn a_refused_downgrade_installs_nothing_and_does_not_fall_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        // A genuinely signed older release. Verification would succeed on those
        // bytes, so the only thing that can stop this is the version policy.
        let result = run_update(
            &identity("2.0.0", "1.0.0"),
            &Context {
                pubkey: "",
                base: None,
                cache: None,
                app_id: "dev.example.testapp",
                work_dir: dir.path(),
                limits: Limits::default(),
            },
            &server,
            &handoff,
        );

        assert!(
            matches!(result, Err(Error::Refused(Refusal::Downgrade { .. }))),
            "expected a downgrade refusal, got {result:?}"
        );
        assert!(
            handoff.installed.borrow().is_empty(),
            "a downgrade must never reach the installer"
        );
    }

    #[test]
    fn an_identity_mismatch_installs_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        // Tauri was told 3.0.0; the document describes 1.0.1.
        let lying = UpdateIdentity::new(
            "1.0.0",
            "3.0.0",
            "linux-x86_64",
            "https://example.com/full.bin",
            "a-signature",
            manifest_json("1.0.1", "https://example.com/full.bin", "a-signature"),
        );

        let result = run_update(
            &lying,
            &Context {
                pubkey: "",
                base: None,
                cache: None,
                app_id: "dev.example.testapp",
                work_dir: dir.path(),
                limits: Limits::default(),
            },
            &server,
            &handoff,
        );

        assert!(
            matches!(
                result,
                Err(Error::Refused(Refusal::IdentityMismatch {
                    field: "version",
                    ..
                }))
            ),
            "expected a version identity mismatch, got {result:?}"
        );
        assert!(handoff.installed.borrow().is_empty());
    }

    #[test]
    fn an_already_installed_release_is_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        let outcome = run_update(
            &identity("1.0.1", "1.0.1"),
            &Context {
                pubkey: "",
                base: None,
                cache: None,
                app_id: "dev.example.testapp",
                work_dir: dir.path(),
                limits: Limits::default(),
            },
            &server,
            &handoff,
        )
        .expect("already being up to date is not a failure");

        assert_eq!(outcome, Outcome::UpToDate);
        assert!(handoff.installed.borrow().is_empty());
    }

    #[test]
    fn a_refusal_downloads_nothing() {
        // Gate A test 10, at the flow level: the flow has no manifest URL to
        // fetch, and a refusal stops before the artifact download too.
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        // Every fetch would 404, so reaching the network at all would surface as
        // Error::Fetch rather than Error::Refused.
        let result = run_update(
            &identity("2.0.0", "1.0.0"),
            &Context {
                pubkey: "",
                base: None,
                cache: None,
                app_id: "dev.example.testapp",
                work_dir: dir.path(),
                limits: Limits::default(),
            },
            &server,
            &handoff,
        );
        assert!(matches!(result, Err(Error::Refused(_))), "got {result:?}");
    }

    #[test]
    fn a_document_missing_the_selected_artifact_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        // Tauri says it selected an artifact, and the document we parsed has no
        // entry describing it. That is one document read two ways, so it is
        // refused outright rather than downloaded and checked afterwards.
        let identity = UpdateIdentity::new(
            "1.0.0",
            "1.0.1",
            "windows-x86_64",
            "https://example.com/missing.bin",
            "a-signature",
            r#"{"version":"1.0.1","platforms":{}}"#,
        );

        let result = run_update(
            &identity,
            &Context {
                pubkey: "",
                base: None,
                cache: None,
                app_id: "dev.example.testapp",
                work_dir: dir.path(),
                limits: Limits::default(),
            },
            &server,
            &handoff,
        );

        assert!(
            matches!(
                result,
                Err(Error::Refused(Refusal::IdentityMismatch {
                    field: "platform",
                    ..
                }))
            ),
            "got {result:?}"
        );
        assert!(handoff.installed.borrow().is_empty());
    }
}
