//! The last gate before assets become public.
//!
//! # Why this is not `Manifest::validate`
//!
//! [`Manifest::validate`](tauri_updater_delta_core::manifest::Manifest::validate)
//! asks whether a document is *internally consistent*: schema, hash algorithm,
//! every delta entry having a full-download counterpart, the two layers agreeing
//! on the signature. It is the right check and it cannot fail the way that
//! matters here, because a manifest describing the wrong release is perfectly
//! consistent with itself.
//!
//! Everything this module checks is a comparison against something **outside**
//! the document:
//!
//! | Claim in the manifest | Checked against |
//! | --- | --- |
//! | `version` | the git tag being published |
//! | `platforms[p].url` | the scheme policy for a public release |
//! | `target_installer_blake3` / `_size` | the bytes on disk about to be uploaded |
//! | the signature | those same bytes, under the configured public key |
//! | the authenticated identity | the app id, version, platform and digest |
//! | `tar_layer.representation` | what the signature says the artifact is |
//!
//! A manifest is the only thing a client trusts enough to act on, and once it is
//! uploaded it is acted on. Every check here is cheap and runs while the damage
//! is still one `rm` away.
//!
//! # Why it re-derives rather than re-reads
//!
//! The release tool wrote these numbers, so re-reading them from the manifest
//! would prove only that the file round-tripped through JSON. Each check hashes
//! the artifact, verifies the signature and parses the identity again, from the
//! bytes that are actually going to be published — which is the only version of
//! the question a client will ever ask.

use std::path::Path;

use tauri_updater_delta_core::manifest::Manifest;
use tauri_updater_delta_core::release_identity::ReleaseBinding;
use tauri_updater_delta_core::{verify_artifact, FileHash};

use crate::{Error, Result};

/// The release being published, as facts rather than as claims.
#[derive(Debug, Clone)]
pub struct ReleaseUnderTest<'a> {
    /// The tag being published, with or without a leading `v`.
    pub tag: &'a str,
    /// Application bundle identifier, from `tauri.conf.json`.
    pub app_id: &'a str,
    /// Tauri platform identifier this release publishes.
    pub platform: &'a str,
    /// The artifact file about to be uploaded.
    pub artifact: &'a Path,
    /// Base64 minisign public key, as it appears in `tauri.conf.json`.
    pub pubkey: &'a str,
    /// Permit `http://` URLs.
    ///
    /// For loopback rehearsals only. A public release with a plain-HTTP URL
    /// hands every client's update to anyone on the path, and the client
    /// refuses it anyway (`docs/DECISIONS.md` #19) — so publishing one produces
    /// a release that is both unsafe and broken.
    pub allow_insecure_urls: bool,
}

/// What was verified, so a CI log says more than "ok".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReport {
    /// Version the manifest and tag agree on.
    pub version: String,
    /// Platform checked.
    pub platform: String,
    /// BLAKE3 of the artifact, as computed here.
    pub artifact_blake3: String,
    /// Size of the artifact, as measured here.
    pub artifact_size: u64,
    /// The authenticated release identity, rendered as it was signed.
    pub identity: String,
    /// Versions a direct patch is published for.
    pub direct_patch_from: Vec<String>,
    /// Versions a tar-layer patch is published for.
    pub tar_patch_from: Vec<String>,
}

/// Check that `manifest` describes the release actually being published.
///
/// Every failure is a refusal to publish. None of them is recoverable by
/// retrying, because each one means two sources of truth already disagree.
pub fn verify_release(
    manifest: &Manifest,
    release: &ReleaseUnderTest<'_>,
) -> Result<ReleaseReport> {
    // ---- the document against the tag -----------------------------------
    let expected_version = release.tag.strip_prefix('v').unwrap_or(release.tag);
    if manifest.version != expected_version {
        return Err(reject(format!(
            "the manifest describes {} but the tag being published is {} ({})",
            manifest.version, expected_version, release.tag
        )));
    }

    // Structural consistency, so the checks below can assume a well-formed
    // document rather than re-deriving that themselves.
    //
    // Wrapped rather than propagated: everything this function returns is a
    // refusal to publish, and a CI log that reports some of them in one voice
    // and some in another makes the reader work out which is which.
    manifest
        .validate()
        .map_err(|e| reject(format!("the manifest is not internally consistent: {e}")))?;

    let tauri = manifest.platforms.get(release.platform).ok_or_else(|| {
        reject(format!(
            "the manifest has no {} entry, so Tauri's own check would find nothing \
             to download",
            release.platform
        ))
    })?;

    check_url("the full-download URL", &tauri.url, release)?;

    // ---- the document against the bytes ----------------------------------
    let bytes = std::fs::read(release.artifact)
        .map_err(|e| Error::Io(format!("reading {}: {e}", release.artifact.display())))?;
    let artifact_blake3 = FileHash::of_bytes(&bytes).to_hex();
    let artifact_size = bytes.len() as u64;

    // The signature, verified the way a client verifies it: against these bytes
    // and this key, with no help from the manifest.
    let verified = verify_artifact(bytes, &tauri.signature, release.pubkey).map_err(|e| {
        reject(format!(
            "the signature in the manifest does not verify against {} under the \
             configured public key: {e}",
            release.artifact.display()
        ))
    })?;

    // ---- the signature's own statement -----------------------------------
    let identity = match verified.binding() {
        ReleaseBinding::Authenticated(identity) => identity,
        // Publishable by a client's standards -- a legacy signature installs
        // fine, by the migration policy in #27. Not publishable by ours: this
        // repository's tooling always binds an identity, so its absence means
        // the artifact was signed by something else, and the delta paths would
        // be unavailable to every client that fetched it.
        ReleaseBinding::Legacy => {
            return Err(reject(
                "the signature carries no authenticated release identity. Every \
                 client would refuse the delta paths and download in full. This \
                 tooling always binds one, so an artifact without one was signed \
                 by something else."
                    .to_owned(),
            ))
        }
    };

    identity
        .check(
            release.app_id,
            expected_version,
            release.platform,
            &artifact_blake3,
            artifact_size,
        )
        .map_err(|e| reject(format!("the signed identity contradicts the release: {e}")))?;

    // ---- the delta metadata ----------------------------------------------
    let mut direct_patch_from = Vec::new();
    let mut tar_patch_from = Vec::new();

    if let Some(entry) = manifest
        .delta
        .as_ref()
        .and_then(|d| d.platforms.get(release.platform))
    {
        if entry.target_installer_blake3 != artifact_blake3 {
            return Err(reject(format!(
                "the delta layer describes an artifact with digest {} but the file \
                 being uploaded hashes to {}. A client would rebuild something and \
                 compare it against the wrong number.",
                entry.target_installer_blake3, artifact_blake3
            )));
        }
        if entry.target_installer_size != artifact_size {
            return Err(reject(format!(
                "the delta layer declares {} bytes, the artifact is {}",
                entry.target_installer_size, artifact_size
            )));
        }

        for (from, patch) in &entry.patches {
            check_url(&format!("the {from} patch URL"), &patch.patch_url, release)?;
            direct_patch_from.push(from.clone());
        }

        if let Some(layer) = &entry.tar_layer {
            // The representation is a manifest claim; the signature also states
            // one. Requiring them to agree is what stops a document describing
            // an artifact as something it is not.
            identity
                .check_representation(&layer.representation)
                .map_err(|e| {
                    reject(format!(
                        "the tar layer and the signature disagree about what this \
                         artifact is: {e}"
                    ))
                })?;

            for (from, patch) in &layer.patches {
                check_url(
                    &format!("the {from} tar-patch URL"),
                    &patch.patch_url,
                    release,
                )?;
                tar_patch_from.push(from.clone());
            }
        }
    }

    direct_patch_from.sort();
    tar_patch_from.sort();

    Ok(ReleaseReport {
        version: manifest.version.clone(),
        platform: release.platform.to_owned(),
        artifact_blake3,
        artifact_size,
        identity: identity.to_trusted_comment(),
        direct_patch_from,
        tar_patch_from,
    })
}

/// Refuse plain HTTP, and anything that is not a URL at all.
fn check_url(what: &str, url: &str, release: &ReleaseUnderTest<'_>) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if url.starts_with("http://") {
        if release.allow_insecure_urls {
            return Ok(());
        }
        return Err(reject(format!(
            "{what} is plain HTTP ({url}). Clients refuse these without an explicit \
             opt-in, so publishing one produces a release that is both unsafe and \
             unusable."
        )));
    }
    Err(reject(format!("{what} is not an http(s) URL ({url})")))
}

fn reject(reason: String) -> Error {
    Error::Request(format!("refusing to publish: {reason}"))
}
