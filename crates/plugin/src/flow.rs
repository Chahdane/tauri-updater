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

use tauri_updater_delta_core::client::{plan_update, Fetch, UpdateSource};
use tauri_updater_delta_core::{verify_artifact, UpdateIdentity, VerifiedArtifact};

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
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Installed from a patch.
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
    /// Scratch directory for the patch and the rebuilt artifact.
    pub work_dir: &'a Path,
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

    let source = plan_update(identity, ctx.base, ctx.work_dir, fetch);

    match source {
        UpdateSource::UpToDate => Ok(Outcome::UpToDate),

        // Never a fallback. The full-download path is described by the same
        // document that just failed the check, so there is nothing safer to
        // reach for.
        UpdateSource::Refused { reason } => Err(Error::Refused(reason)),

        UpdateSource::Delta {
            artifact,
            signature,
            downloaded,
            would_have_downloaded,
        } => {
            let bytes = std::fs::read(&artifact)
                .map_err(|e| Error::Io(format!("reading the rebuilt artifact: {e}")))?;

            // The reconstruction already matched the manifest's digest. This is
            // the separate question of whether the release process actually
            // published it — and nothing else will ask it.
            match verify_artifact(bytes, &signature, ctx.pubkey) {
                Ok(verified) => {
                    handoff.install(&verified)?;
                    let _ = std::fs::remove_file(&artifact);
                    Ok(Outcome::InstalledFromDelta {
                        downloaded,
                        saved_against: would_have_downloaded,
                    })
                }
                Err(_) => {
                    // The artifact matched the digest this document published
                    // but not the signature it published alongside it. That
                    // does not prove the document is forged — it is not signed,
                    // so there was never a claim to disprove. What it does mean
                    // is that something in the chain that produced this release
                    // is wrong, and we cannot tell what. Falling back would
                    // fetch a second artifact chosen by the same document and
                    // check it against a signature from that same document,
                    // which grants a second attempt rather than a safer one.
                    // See docs/DECISIONS.md #11.
                    let _ = std::fs::remove_file(&artifact);
                    Err(Error::Signature(
                        "the rebuilt artifact did not match the manifest's signature".to_owned(),
                    ))
                }
            }
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

            handoff.install(&verified)?;
            let _ = std::fs::remove_file(&full);
            Ok(Outcome::InstalledFromFullDownload)
        }
    }
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
                work_dir: dir.path(),
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
                work_dir: dir.path(),
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
                work_dir: dir.path(),
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
                work_dir: dir.path(),
            },
            &server,
            &handoff,
        );
        assert!(matches!(result, Err(Error::Refused(_))), "got {result:?}");
    }

    #[test]
    fn a_target_with_no_published_artifact_installs_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        // No platforms at all in the document, so there is nothing to install
        // and the full download 404s rather than silently succeeding.
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
                work_dir: dir.path(),
            },
            &server,
            &handoff,
        );

        assert!(matches!(result, Err(Error::Fetch(_))), "got {result:?}");
        assert!(handoff.installed.borrow().is_empty());
    }
}
