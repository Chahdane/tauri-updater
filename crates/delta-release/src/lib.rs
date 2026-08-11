//! Release-time tooling: turn two installers into a patch and a manifest.
//!
//! This is the half of the system that runs on CI, once per release. It takes
//! the installer users already have and the installer they are moving to, and
//! produces everything a client needs to make that move cheaply:
//!
//! - the patch itself,
//! - the digests that prove a reconstruction is correct,
//! - a minisign signature over the **target installer**, and
//! - a manifest that is simultaneously a valid Tauri updater document.
//!
//! Nothing here runs on a user's machine, so it favours being obvious over being
//! fast.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod signing;

use std::path::Path;

use tauri_updater_delta_core::backend::{PatchBackend, ZstdBackend};
use tauri_updater_delta_core::manifest::{
    DeltaLayer, DeltaPlatform, Manifest, Patch, TauriPlatform, HASH_ALGO, SCHEMA_VERSION,
};
use tauri_updater_delta_core::FileHash;

use signing::SigningKey;

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong while preparing a release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed.
    #[error("{0}")]
    Io(String),

    /// A signing key could not be loaded.
    #[error("{0}")]
    Key(String),

    /// Producing a signature failed.
    #[error("{0}")]
    Sign(String),

    /// The requested release does not make sense.
    #[error("{0}")]
    Request(String),

    /// The delta engine failed.
    #[error(transparent)]
    Engine(#[from] tauri_updater_delta_core::Error),
}

/// One upgrade path to publish: from an installed version to this release.
#[derive(Debug, Clone)]
pub struct ReleaseRequest<'a> {
    /// Tauri platform identifier, e.g. `"linux-x86_64"`.
    pub platform: &'a str,
    /// Version being released.
    pub version: &'a str,
    /// Version this patch upgrades from.
    pub from_version: &'a str,
    /// Installer users on `from_version` already have.
    pub previous_installer: &'a Path,
    /// Installer being released.
    pub new_installer: &'a Path,
    /// Where the full installer will be downloadable.
    pub installer_url: &'a str,
    /// Where the patch will be downloadable.
    pub patch_url: &'a str,
    /// Where to write the generated patch.
    pub patch_out: &'a Path,
    /// Optional release notes.
    pub notes: Option<&'a str>,
    /// Optional RFC 3339 publication timestamp.
    pub pub_date: Option<&'a str>,
}

/// What a release run produced, beyond the manifest itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSummary {
    /// Size of the generated patch in bytes.
    pub patch_size: u64,
    /// Size of the target installer in bytes.
    pub installer_size: u64,
}

impl PatchSummary {
    /// Patch size as a percentage of a full download.
    pub fn ratio_percent(&self) -> f64 {
        if self.installer_size == 0 {
            return 0.0;
        }
        self.patch_size as f64 / self.installer_size as f64 * 100.0
    }
}

/// Generate a patch and fold it into a manifest.
///
/// `existing` is the manifest currently published, if any. Because schema 1
/// only supports patching *to the latest release*, a manifest describing an
/// older version is replaced rather than merged — its patches reconstruct an
/// artifact that is no longer current. A manifest for the same version has the
/// new upgrade path added alongside the ones already there, which is what lets a
/// release publish several `from` versions by calling this repeatedly.
pub fn build_release(
    req: &ReleaseRequest<'_>,
    key: &SigningKey,
    existing: Option<Manifest>,
) -> Result<(Manifest, PatchSummary)> {
    if req.from_version == req.version {
        return Err(Error::Request(format!(
            "cannot patch {} to itself",
            req.version
        )));
    }
    for (label, path) in [
        ("previous installer", req.previous_installer),
        ("new installer", req.new_installer),
    ] {
        if !path.is_file() {
            return Err(Error::Request(format!(
                "{label} {} does not exist",
                path.display()
            )));
        }
    }

    if let Some(parent) = req.patch_out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }

    ZstdBackend::new().diff(req.previous_installer, req.new_installer, req.patch_out)?;

    let installer_digest = FileHash::of_file(req.new_installer)?;
    let installer_size = file_size(req.new_installer)?;
    let patch_digest = FileHash::of_file(req.patch_out)?;
    let patch_size = file_size(req.patch_out)?;

    // Over the target installer, not the patch: both the delta path and the
    // full-download path end up holding this exact artifact, so one signature
    // covers both.
    let signature = key.sign_file(req.new_installer)?;

    let patch = Patch {
        backend_id: ZstdBackend::ID.to_owned(),
        patch_url: req.patch_url.to_owned(),
        patch_blake3: patch_digest.to_hex(),
        patch_size,
    };

    let mut manifest = match existing {
        Some(existing) if existing.version == req.version => existing,
        _ => Manifest {
            version: req.version.to_owned(),
            notes: None,
            pub_date: None,
            platforms: Default::default(),
            delta: None,
        },
    };

    if let Some(notes) = req.notes {
        manifest.notes = Some(notes.to_owned());
    }
    if let Some(pub_date) = req.pub_date {
        manifest.pub_date = Some(pub_date.to_owned());
    }

    manifest.platforms.insert(
        req.platform.to_owned(),
        TauriPlatform {
            url: req.installer_url.to_owned(),
            signature: signature.clone(),
        },
    );

    let delta = manifest.delta.get_or_insert_with(|| DeltaLayer {
        schema: SCHEMA_VERSION,
        hash_algo: HASH_ALGO.to_owned(),
        platforms: Default::default(),
    });

    let entry = delta
        .platforms
        .entry(req.platform.to_owned())
        .or_insert_with(|| DeltaPlatform {
            target_version: req.version.to_owned(),
            target_installer_blake3: installer_digest.to_hex(),
            target_installer_size: installer_size,
            signature: signature.clone(),
            patches: Default::default(),
        });

    // Re-signing produces a different signature each run (minisign includes
    // randomness), so keep the entry and the Tauri layer in step explicitly
    // rather than relying on them having been written together.
    entry.target_version = req.version.to_owned();
    entry.target_installer_blake3 = installer_digest.to_hex();
    entry.target_installer_size = installer_size;
    entry.signature = signature;
    entry.patches.insert(req.from_version.to_owned(), patch);

    manifest.validate()?;

    Ok((
        manifest,
        PatchSummary {
            patch_size,
            installer_size,
        },
    ))
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .map_err(|e| Error::Io(format!("stat {}: {e}", path.display())))?
        .len())
}

/// Read a manifest from disk, if it exists.
///
/// A missing file is not an error — that is simply the first release.
pub fn load_manifest(path: &Path) -> Result<Option<Manifest>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(Manifest::from_json(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(format!("reading {}: {e}", path.display()))),
    }
}

/// Write a manifest to disk as pretty JSON.
pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let json = manifest.to_json()?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }
    std::fs::write(path, json).map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))
}
