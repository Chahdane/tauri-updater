//! The update flow, from manifest URL to a verified artifact handed to an installer.
//!
//! Both boundaries are traits — [`Fetch`] for the network,
//! [`InstallHandoff`] for the platform installer — so this whole path is
//! exercised in tests with no server, no Tauri runtime and no filesystem
//! installer. The Tauri-specific implementations are thin wrappers around them.
//!
//! # The safety invariant, end to end
//!
//! Two things cannot be expressed here, by construction rather than by care:
//!
//! - **An update that fails.** [`plan_update`] returns
//!   `Delta | Full | Unavailable`, so a delta failure resolves to a full
//!   download rather than an error.
//! - **An unverified install.** [`InstallHandoff::install`] takes a
//!   [`VerifiedArtifact`], which only `verify_artifact` can produce. Since
//!   Tauri does not verify what it is handed (see `docs/DECISIONS.md` #10),
//!   this is the sole gate — so it is the compiler's job, not a reviewer's.

use std::path::Path;

use tauri_updater_delta_core::client::{plan_update, Fetch, UpdateSource};
use tauri_updater_delta_core::{verify_artifact, Manifest, VerifiedArtifact};

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
    /// Nothing published for this platform.
    Unavailable,
}

/// Everything the flow needs to know about the host.
pub struct Context<'a> {
    /// Tauri platform identifier, e.g. `"linux-x86_64"`.
    pub platform: &'a str,
    /// Version currently installed.
    pub current_version: &'a str,
    /// Base64 minisign public key, as in `tauri.conf.json`.
    pub pubkey: &'a str,
    /// Previously cached installer, if any.
    pub base: Option<&'a Path>,
    /// Scratch directory for the patch and the rebuilt artifact.
    pub work_dir: &'a Path,
}

/// Fetch the manifest, take the cheapest safe path to the release, install it.
///
/// The delta path is attempted when it can be; anything wrong with it results
/// in the full download the official updater would have performed anyway.
pub fn run_update(
    manifest_url: &str,
    ctx: &Context<'_>,
    fetch: &dyn Fetch,
    handoff: &dyn InstallHandoff,
) -> Result<Outcome> {
    let manifest_path = ctx.work_dir.join("manifest.json");
    std::fs::create_dir_all(ctx.work_dir)
        .map_err(|e| Error::Io(format!("creating {}: {e}", ctx.work_dir.display())))?;

    fetch
        .fetch(manifest_url, &manifest_path)
        .map_err(|e| Error::Fetch(format!("fetching the manifest: {e}")))?;

    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| Error::Io(format!("reading the manifest: {e}")))?;
    let manifest = Manifest::from_json(&text).map_err(|e| Error::Manifest(e.to_string()))?;

    let source = plan_update(
        &manifest,
        ctx.platform,
        ctx.current_version,
        ctx.base,
        ctx.work_dir,
        fetch,
    );

    match source {
        UpdateSource::Unavailable => Ok(Outcome::Unavailable),

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
                    // A rebuilt artifact that matches the digest but not the
                    // signature means the manifest itself is not trustworthy.
                    // Falling back would download an artifact described by that
                    // same manifest, so there is nothing safe to fall back to.
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

    // The full harness lives in tests/update_flow.rs, which builds a real
    // manifest with the release tooling. These cover the paths that need no
    // signing key.

    #[test]
    fn a_missing_manifest_is_an_error_not_a_silent_success() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = FakeServer(HashMap::new());
        let handoff = RecordingHandoff::default();

        let result = run_update(
            "https://example.com/manifest.json",
            &Context {
                platform: "linux-x86_64",
                current_version: "1.0.0",
                pubkey: "",
                base: None,
                work_dir: dir.path(),
            },
            &server,
            &handoff,
        );

        assert!(matches!(result, Err(Error::Fetch(_))));
        assert!(
            handoff.installed.borrow().is_empty(),
            "nothing may be installed when the manifest could not be fetched"
        );
    }

    #[test]
    fn a_malformed_manifest_installs_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = "https://example.com/manifest.json".to_owned();
        let server = FakeServer(HashMap::from([(url.clone(), b"{ not json".to_vec())]));
        let handoff = RecordingHandoff::default();

        let result = run_update(
            &url,
            &Context {
                platform: "linux-x86_64",
                current_version: "1.0.0",
                pubkey: "",
                base: None,
                work_dir: dir.path(),
            },
            &server,
            &handoff,
        );

        assert!(matches!(result, Err(Error::Manifest(_))));
        assert!(handoff.installed.borrow().is_empty());
    }

    #[test]
    fn an_unlisted_platform_installs_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = "https://example.com/manifest.json".to_owned();
        let manifest = r#"{
            "version": "1.0.1",
            "platforms": {
                "linux-x86_64": { "url": "https://example.com/a", "signature": "sig" }
            }
        }"#;
        let server = FakeServer(HashMap::from([(url.clone(), manifest.as_bytes().to_vec())]));
        let handoff = RecordingHandoff::default();

        let outcome = run_update(
            &url,
            &Context {
                platform: "windows-x86_64",
                current_version: "1.0.0",
                pubkey: "",
                base: None,
                work_dir: dir.path(),
            },
            &server,
            &handoff,
        )
        .expect("an unlisted platform is not an error");

        assert_eq!(outcome, Outcome::Unavailable);
        assert!(handoff.installed.borrow().is_empty());
    }
}
